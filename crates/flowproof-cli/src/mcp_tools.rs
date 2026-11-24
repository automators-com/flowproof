//! The tools `flowproof mcp` serves (plans/017-flowproof-mcp-server.md).
//! Each one is a supervised `flowproof <cmd> --json` child (decision 1):
//! the child's stdout is a pipe read here, so nothing the engine prints can
//! reach the protocol, and the tool surface tracks the CLI instead of
//! copying it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Map, Value};

pub(crate) fn list() -> Value {
    let path = |what: &str| json!({ "type": "string", "description": what });
    let step = |what: &str| json!({ "type": "integer", "minimum": 1, "description": what });
    let switch = |what: &str| json!({ "type": "boolean", "description": what });
    let common = |mut properties: Value| {
        properties["spec"] = path("A *.flow.yaml. Relative to `cwd`.");
        properties["vars"] = path("A business-data values file to load before resolving ${VAR}s.");
        properties["var"] = json!({
            "type": "object",
            "additionalProperties": { "type": "string" },
            "description": "Business-data overrides, KEY: VALUE. Never secrets: those come \
                from `flowproof config` or the environment.",
        });
        properties["headed"] = switch("Show the browser. Headless otherwise.");
        properties["allow_exec"] = switch(
            "Let this call run commands the flow's files author: a suite.yaml's env_from, \
            before_each and after_each shell, a spec's agent or MCP server command, a Windows \
            app's launch command. Off by default, and the refusal names the command and the \
            file it came from. Show the person that command and pass true only if they agree.",
        );
        properties["cwd"] = path(
            "The folder to run in, for this call only (default: the folder the server was \
            started in). The result echoes the folder used.",
        );
        json!({ "type": "object", "properties": properties, "required": ["spec"],
            "additionalProperties": false })
    };
    json!([{
        "name": "flowproof_run",
        "title": "Replay a flow",
        "description": "Replay a recorded flow deterministically, with no model calls. Point \
            `spec` at a directory to run every *.flow.yaml under it as a suite, which can take \
            many minutes. A failing flow comes back as a result with `passed: false` and the \
            report saying which step failed and why; that is not a tool error.",
        "inputSchema": common(json!({
            "trace": path("The trace to replay (default: the one `record` wrote for the spec)."),
            "from": step("Replay only from this step, counted from 1. One flow only."),
            "until": step("Replay only up to and including this step. One flow only."),
            "retries": { "type": "integer", "minimum": 0, "maximum": 255,
                "description": "Re-run a failed flow up to this many extra times." },
        })),
    }, {
        "name": "flowproof_record",
        "title": "Record a flow",
        "description": "Perform a flow once against the live app and write its trace next to \
            the spec. This does whatever the flow does, for real, so use test accounts. Plain \
            steps are authored by the model a person set with `flowproof config ai`; with none, \
            they fall back to deterministic grammar. A step that cannot be authored comes back \
            with `needs_clarification`: what was ambiguous and what the screen offered. That is \
            a result, not an error: reword that step in the .flow.yaml and record again, or ask \
            the person for missing business data rather than guessing it.",
        "inputSchema": common(json!({
            "out": path("Where to write the trace (default: next to the spec)."),
            "author": { "type": "string", "enum": ["auto", "rules", "llm"],
                "description": "auto (default): the model, falling back to grammar; rules: \
                    grammar only; llm: the model for every plain step." },
            "reuse": switch("Reuse every old step whose target still resolves; re-author only \
                what drifted. Needs an existing trace."),
            "resume": switch("Pick up where the last recording of this flow stopped."),
            "no_repair": switch("On a failure, stop and report instead of letting the model \
                patch the failing step and retry."),
        })),
    }])
}

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Number,
    Switch,
}

/// The arguments each tool takes beyond the shared `spec`, `vars`, `var`,
/// `headed` and `cwd`, and how each becomes a flag (`no_repair` →
/// `--no-repair`).
const RUN: &[(&str, Kind)] = &[
    ("trace", Kind::Text),
    ("from", Kind::Number),
    ("until", Kind::Number),
    ("retries", Kind::Number),
];
const RECORD: &[(&str, Kind)] = &[
    ("out", Kind::Text),
    ("author", Kind::Text),
    ("reuse", Kind::Switch),
    ("resume", Kind::Switch),
    ("no_repair", Kind::Switch),
];

/// A JSON-RPC error for a call that names no known tool; everything wrong
/// with a known tool's input is a tool result, so the model can correct it.
pub(crate) fn call(params: &Value) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str);
    let empty = Map::new();
    let args = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(args)) => args,
        Some(_) => return Ok(tool_error("arguments must be an object")),
    };
    let (command, table, interpret): (_, _, fn(Value, Outcome) -> Value) = match name {
        Some("flowproof_run") => ("run", RUN, run_result),
        Some("flowproof_record") => ("record", RECORD, record_result),
        Some(other) => return Err(format!("unknown tool: {other}")),
        None => return Err("tools/call needs a tool name".into()),
    };
    let outcome = resolve_cwd(args).and_then(|cwd| {
        let argv = argv(command, table, args)?;
        Ok((cwd.clone(), supervise(&cwd, &argv)?))
    });
    Ok(match outcome {
        Ok((cwd, outcome)) => interpret(json!({ "cwd": cwd.display().to_string() }), outcome),
        Err(message) => tool_error(&message),
    })
}

/// The command line for one call. Every value goes in `--flag=value` form
/// and the spec after `--`, so no argument can be read as a flag of its own.
fn argv(
    command: &str,
    table: &[(&str, Kind)],
    args: &Map<String, Value>,
) -> Result<Vec<String>, String> {
    let shared = ["spec", "vars", "var", "headed", "cwd", "allow_exec"];
    let known = |k: &str| shared.contains(&k) || table.iter().any(|(name, _)| *name == k);
    if let Some(unknown) = args.keys().find(|k| !known(k)) {
        return Err(format!("unknown argument `{unknown}`"));
    }
    let spec = string(args, "spec")?.ok_or("`spec` is required")?;
    let mut argv = vec![command.to_string(), "--json".to_string()];
    let shared = [("vars", Kind::Text), ("allow_exec", Kind::Switch)];
    for &(name, kind) in shared.iter().chain(table) {
        let flag = name.replace('_', "-");
        let Some(value) = args.get(name).filter(|v| !v.is_null()) else {
            continue;
        };
        match kind {
            Kind::Text => {
                let text = value.as_str().ok_or(format!("`{name}` must be a string"))?;
                argv.push(format!("--{flag}={text}"));
            }
            Kind::Number => {
                let n = value.as_u64().ok_or(format!("`{name}` must be a number"))?;
                argv.push(format!("--{flag}={n}"));
            }
            Kind::Switch => {
                if value
                    .as_bool()
                    .ok_or(format!("`{name}` must be true or false"))?
                {
                    argv.push(format!("--{flag}"));
                }
            }
        }
    }
    if let Some(vars) = args.get("var").filter(|v| !v.is_null()) {
        let vars = vars.as_object().ok_or("`var` must be an object")?;
        for (key, value) in vars {
            let value = value
                .as_str()
                .ok_or(format!("`var.{key}` must be a string"))?;
            argv.push(format!("--var={key}={value}"));
        }
    }
    // Always one or the other: `record` shows the browser by default, and
    // an ambient FLOWPROOF_HEADED must not open one on someone's screen.
    let headed = match args.get("headed") {
        None | Some(Value::Null) => false,
        Some(b) => b.as_bool().ok_or("`headed` must be true or false")?,
    };
    argv.push(if headed { "--headed" } else { "--headless" }.into());
    argv.extend(["--".to_string(), spec]);
    Ok(argv)
}

fn string(args: &Map<String, Value>, name: &str) -> Result<Option<String>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{name}` must be a string")),
    }
}

/// The folder a call runs in: the server's own, or `cwd` resolved against
/// it. Set on the child only; the server's working directory never moves.
fn resolve_cwd(args: &Map<String, Value>) -> Result<PathBuf, String> {
    let here = std::env::current_dir().map_err(|e| format!("reading the working folder: {e}"))?;
    let cwd = match string(args, "cwd")? {
        Some(dir) => here.join(dir),
        None => here,
    };
    if !cwd.is_dir() {
        return Err(format!("`cwd` {} is not a folder", cwd.display()));
    }
    Ok(cwd)
}

/// `current_exe()`, or `FLOWPROOF_MCP_EXE` for a wrapper on PATH, the same
/// override the `mcp-stdio` stand-in honours.
fn flowproof_exe() -> Result<PathBuf, String> {
    match std::env::var("FLOWPROOF_MCP_EXE") {
        Ok(path) if !path.trim().is_empty() => Ok(PathBuf::from(path)),
        _ => std::env::current_exe().map_err(|e| format!("finding the flowproof executable: {e}")),
    }
}

/// What one child left behind: its exit code, the JSON it printed on
/// stdout (if that parsed), and the tail of its stderr.
struct Outcome {
    code: Option<i32>,
    json: Option<Value>,
    stderr: String,
}

fn supervise(cwd: &Path, argv: &[String]) -> Result<Outcome, String> {
    let exe = flowproof_exe()?;
    let output = Command::new(&exe)
        .args(argv)
        .current_dir(cwd)
        .env("FLOWPROOF_NO_UPDATE_CHECK", "1")
        // Consent is per call, as `allow_exec` (plan 17, decision 4): an
        // opt-in the client happened to inherit must not stand in for it.
        .env_remove("FLOWPROOF_ALLOW_EXEC")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("starting {}: {e}", exe.display()))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    Ok(Outcome {
        code: output.status.code(),
        json: serde_json::from_slice(&output.stdout).ok(),
        stderr: lines[lines.len().saturating_sub(20)..].join("\n"),
    })
}

/// Exit 0 and 1 with a report are a pass and a fail.
fn run_result(mut result: Value, outcome: Outcome) -> Value {
    match (outcome.code, outcome.json) {
        (Some(code @ (0 | 1)), Some(report)) => {
            result["exit_code"] = json!(code);
            result["passed"] = json!(code == 0);
            result["report"] = report;
            tool_result(result, false)
        }
        (code, report) => engine_error(result, code, report, outcome.stderr),
    }
}

/// Exit 0 is a trace written (or a flow skipped by its gate). A step that
/// needs clarifying is data for the agent to act on, not an error.
fn record_result(mut result: Value, outcome: Outcome) -> Value {
    match (outcome.code, outcome.json) {
        (Some(0), Some(payload)) => {
            result["exit_code"] = json!(0);
            result["recorded"] = json!(payload.get("skipped").is_none());
            result["result"] = payload;
            tool_result(result, false)
        }
        (code, Some(payload)) if payload.get("needs_clarification").is_some() => {
            result["exit_code"] = json!(code);
            result["recorded"] = json!(false);
            result["result"] = payload;
            tool_result(result, false)
        }
        (code, payload) => engine_error(result, code, payload, outcome.stderr),
    }
}

/// Anything else is the engine refusing or breaking: its stderr says why,
/// and whatever JSON it did print (a record failure's step and hint) rides
/// along.
fn engine_error(
    mut result: Value,
    code: Option<i32>,
    json: Option<Value>,
    stderr: String,
) -> Value {
    result["exit_code"] = json!(code);
    if stderr.contains("without consent") {
        result["consent"] = json!(
            "Nothing ran. Show the person the command above and where it comes from; \
            call again with allow_exec: true only if they agree to run it."
        );
    }
    result["error"] = json!(stderr);
    if let Some(json) = json {
        result["result"] = json;
    }
    tool_result(result, true)
}

fn tool_result(structured: Value, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": structured.to_string() }],
        "structuredContent": structured,
        "isError": is_error,
    })
}

fn tool_error(message: &str) -> Value {
    tool_result(json!({ "error": message }), true)
}
