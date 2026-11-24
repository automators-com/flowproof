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
    json!([{
        "name": "flowproof_run",
        "title": "Replay a flow",
        "description": "Replay a recorded flow deterministically, with no model calls. Point \
            `spec` at a directory to run every *.flow.yaml under it as a suite, which can take \
            many minutes. A failing flow comes back as a result with `passed: false` and the \
            report saying which step failed and why; that is not a tool error. Runs headless \
            unless `headed` is true.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "spec": path("A *.flow.yaml, or a directory of them. Relative to `cwd`."),
                "trace": path("The trace to replay (default: the one `record` wrote for the spec)."),
                "vars": path("A business-data values file to load before resolving ${VAR}s."),
                "var": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "Business-data overrides, KEY: VALUE. Never secrets: those \
                        come from `flowproof config` or the environment.",
                },
                "from": step("Replay only from this step, counted from 1. One flow only."),
                "until": step("Replay only up to and including this step. One flow only."),
                "retries": { "type": "integer", "minimum": 0, "maximum": 255,
                    "description": "Re-run a failed flow up to this many extra times." },
                "headed": { "type": "boolean", "description": "Show the browser." },
                "cwd": path("The folder to run in, for this call only (default: the folder \
                    the server was started in). The result echoes the folder used."),
            },
            "required": ["spec"],
            "additionalProperties": false,
        },
    }])
}

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
    match name {
        Some("flowproof_run") => Ok(resolve_cwd(args)
            .and_then(|cwd| Ok((cwd, run_argv(args)?)))
            .and_then(|(cwd, argv)| supervise(&cwd, &argv))
            .unwrap_or_else(|message| tool_error(&message))),
        Some(other) => Err(format!("unknown tool: {other}")),
        None => Err("tools/call needs a tool name".into()),
    }
}

/// The `run` command line. Every value goes in `--flag=value` form and the
/// spec after `--`, so no argument can be read as a flag of its own.
fn run_argv(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    let known = [
        "spec", "trace", "vars", "var", "from", "until", "retries", "headed", "cwd",
    ];
    if let Some(unknown) = args.keys().find(|k| !known.contains(&k.as_str())) {
        return Err(format!("unknown argument `{unknown}`"));
    }
    let spec = string(args, "spec")?.ok_or("`spec` is required")?;
    let mut argv = vec!["run".to_string(), "--json".to_string()];
    for name in ["trace", "vars"] {
        if let Some(value) = string(args, name)? {
            argv.push(format!("--{name}={value}"));
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
    for name in ["from", "until", "retries"] {
        if let Some(n) = args.get(name).filter(|v| !v.is_null()) {
            let n = n.as_u64().ok_or(format!("`{name}` must be a number"))?;
            argv.push(format!("--{name}={n}"));
        }
    }
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

/// Run one child to completion and turn what it printed into a tool result.
/// Exit 0 and 1 with a report are a pass and a fail; anything else is the
/// engine refusing or breaking, reported with the tail of its stderr.
fn supervise(cwd: &Path, argv: &[String]) -> Result<Value, String> {
    let exe = flowproof_exe()?;
    let output = Command::new(&exe)
        .args(argv)
        .current_dir(cwd)
        .env("FLOWPROOF_NO_UPDATE_CHECK", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("starting {}: {e}", exe.display()))?;
    let code = output.status.code();
    let report = serde_json::from_slice::<Value>(&output.stdout).ok();
    let mut result = json!({ "cwd": cwd.display().to_string(), "exit_code": code });
    let failed = match (code, report) {
        (Some(code @ (0 | 1)), Some(report)) => {
            result["passed"] = json!(code == 0);
            result["report"] = report;
            false
        }
        (_, report) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let lines: Vec<&str> = stderr.lines().collect();
            result["error"] = json!(lines[lines.len().saturating_sub(20)..].join("\n"));
            if let Some(report) = report {
                result["report"] = report;
            }
            true
        }
    };
    Ok(tool_result(result, failed))
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
