//! The tools `flowproof mcp` serves (plans/017-flowproof-mcp-server.md).
//! Each one is a supervised `flowproof <cmd> --json` child (decision 1):
//! the child's stdout is a pipe read here, so nothing the engine prints can
//! reach the protocol, and the tool surface tracks the CLI instead of
//! copying it.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

pub(crate) fn list() -> Value {
    let path = |what: &str| json!({ "type": "string", "description": what });
    let step = |what: &str| json!({ "type": "integer", "minimum": 1, "description": what });
    let switch = |what: &str| json!({ "type": "boolean", "description": what });
    let spec = path("A *.flow.yaml. Relative to `cwd`.");
    let vars = path("A business-data values file to load before resolving ${VAR}s.");
    let var = json!({
        "type": "object",
        "additionalProperties": { "type": "string" },
        "description": "Business-data overrides, KEY: VALUE. Never secrets: those come from \
            `flowproof config` or the environment.",
    });
    let headed = switch("Show the browser. Headless otherwise.");
    let author = json!({ "type": "string", "enum": ["auto", "rules", "llm"],
        "description": "auto (default): the model, falling back to grammar; rules: grammar \
            only; llm: the model for every plain step." });
    let allow_exec = switch(
        "Let this call run commands the flow's files author: a suite.yaml's env_from, \
        before_each and after_each shell, a spec's agent or MCP server command, a Windows app's \
        launch command. Off by default, and the refusal names the command and the file it came \
        from. Show the person that command and pass true only if they agree.",
    );
    let schema = |mut properties: Value, required: &[&str]| {
        properties["cwd"] = path(
            "The folder to run in, for this call only (default: the folder the server was \
            started in). The result echoes the folder used.",
        );
        json!({ "type": "object", "properties": properties, "required": required,
            "additionalProperties": false })
    };
    json!([{
        "name": "flowproof_run",
        "title": "Replay a flow",
        "description": "Replay a recorded flow deterministically, with no model calls. Point \
            `spec` at a directory to run every *.flow.yaml under it as a suite, which can take \
            many minutes. A failing flow comes back as a result with `passed: false` and the \
            report saying which step failed and why; that is not a tool error.",
        "inputSchema": schema(json!({
            "spec": spec, "vars": vars, "var": var, "headed": headed, "allow_exec": allow_exec,
            "trace": path("The trace to replay (default: the one `record` wrote for the spec)."),
            "from": step("Replay only from this step, counted from 1. One flow only."),
            "until": step("Replay only up to and including this step. One flow only."),
            "retries": { "type": "integer", "minimum": 0, "maximum": 255,
                "description": "Re-run a failed flow up to this many extra times." },
        }), &["spec"]),
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
        "inputSchema": schema(json!({
            "spec": spec, "vars": vars, "var": var, "headed": headed, "allow_exec": allow_exec,
            "author": author,
            "out": path("Where to write the trace (default: next to the spec)."),
            "reuse": switch("Reuse every old step whose target still resolves; re-author only \
                what drifted. Needs an existing trace."),
            "resume": switch("Pick up where the last recording of this flow stopped."),
            "no_repair": switch("On a failure, stop and report instead of letting the model \
                patch the failing step and retry."),
        }), &["spec"]),
    }, {
        "name": "flowproof_heal",
        "title": "Propose a repair",
        "description": "Re-drive a flow whose app has drifted and propose an updated trace. \
            The proposal is written beside the trace as .proposed.jsonl and the trace is left \
            alone; the result says what changed. `apply` and `apply_proposal` overwrite the \
            trace and are refused without `reviewed: true`: pass that only after the person \
            has reviewed the proposal and said to apply it.",
        "inputSchema": schema(json!({
            "spec": spec, "vars": vars, "var": var, "headed": headed, "author": author,
            "trace": path("The trace to heal (default: the one `record` wrote for the spec)."),
            "apply": switch("Overwrite the trace with the proposal if anything changed."),
            "apply_proposal": switch("Apply the .proposed.jsonl an earlier heal wrote."),
            "reviewed": switch("The person has reviewed the proposal and said to apply it. \
                Required with apply or apply_proposal."),
            "from_run": path("Heal only the steps a run's report flagged, from that run's \
                folder, without driving the app."),
            "step": { "type": "array", "items": { "type": "string" },
                "description": "With from_run: only these step ids." },
        }), &["spec"]),
    }, {
        "name": "flowproof_doctor",
        "title": "Check the setup",
        "description": "Check that flowproof can work on this machine. `ai`: is an authoring \
            model configured and reachable (record falls back to grammar without one). `sap`: \
            can SAP GUI Scripting be reached. A failed check is a result, with the fix in its \
            message; the fix is for the person to make with `flowproof config`.",
        "inputSchema": schema(json!({
            "check": { "type": "string", "enum": ["ai", "sap"], "description": "What to check." },
        }), &["check"]),
    }, {
        "name": "flowproof_audit",
        "title": "Report control coverage",
        "description": "The control-coverage report for a suite: each `control:` block with its \
            pass, fail or capability-error verdict, read from the run record `flowproof_run` \
            wrote for that directory. Nothing is replayed.",
        "inputSchema": schema(json!({
            "dir": path("The suite directory. Relative to `cwd`."),
            "run": path("Report this run id instead of the latest."),
            "since": path("Diff the latest run against this run id."),
        }), &["dir"]),
    }])
}

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Number,
    Switch,
    /// `var`: an object of KEY: VALUE, one `--var` each.
    Vars,
    /// A list of strings, one flag each.
    List,
    /// Always `--headed` or `--headless`: `record` shows the browser by
    /// default, and an ambient FLOWPROOF_HEADED must not open one on
    /// someone's screen. Left off when `from_run` means no app is driven.
    Headed,
    /// `doctor`'s `check`, which becomes `--ai` or `--sap`.
    Check,
    /// heal's `reviewed`: no flag of its own; `apply` and `apply_proposal`
    /// are refused without it.
    Reviewed,
}

/// One tool: the command it runs, the argument passed after `--`, and how
/// each other argument becomes a flag (`no_repair` → `--no-repair`).
struct Tool {
    command: &'static str,
    positional: Option<&'static str>,
    args: &'static [(&'static str, Kind)],
    read: fn(Value, Outcome) -> Value,
}

fn tool(name: &str) -> Option<Tool> {
    use Kind::*;
    let (command, positional, args, read): (_, _, &'static [_], fn(_, _) -> _) = match name {
        "flowproof_run" => (
            "run",
            Some("spec"),
            &[
                ("trace", Text),
                ("from", Number),
                ("until", Number),
                ("retries", Number),
                ("vars", Text),
                ("var", Vars),
                ("headed", Headed),
                ("allow_exec", Switch),
            ],
            report_result,
        ),
        "flowproof_record" => (
            "record",
            Some("spec"),
            &[
                ("out", Text),
                ("author", Text),
                ("reuse", Switch),
                ("resume", Switch),
                ("no_repair", Switch),
                ("vars", Text),
                ("var", Vars),
                ("headed", Headed),
                ("allow_exec", Switch),
            ],
            record_result,
        ),
        "flowproof_heal" => (
            "heal",
            Some("spec"),
            &[
                ("trace", Text),
                ("apply", Switch),
                ("apply_proposal", Switch),
                ("reviewed", Reviewed),
                ("author", Text),
                ("from_run", Text),
                ("step", List),
                ("vars", Text),
                ("var", Vars),
                ("headed", Headed),
            ],
            record_result,
        ),
        "flowproof_doctor" => ("doctor", None, &[("check", Check)], checked_result),
        "flowproof_audit" => (
            "audit",
            Some("dir"),
            &[("run", Text), ("since", Text)],
            report_result,
        ),
        _ => return None,
    };
    Some(Tool {
        command,
        positional,
        args,
        read,
    })
}

/// Why `doctor` checks other than `ai` and `sap` are not tools (plan 17,
/// decision 3).
fn refused_check(check: &str) -> Option<&'static str> {
    match check {
        "fiori" => Some(
            "doctor --fiori logs in to a live Fiori system for real and must never \
            run on a repeated trigger. Ask the person to run `flowproof doctor --fiori`.",
        ),
        "agent" => Some(
            "doctor --agent runs a command the caller supplies. Ask the person to \
            run `flowproof doctor --agent` themselves.",
        ),
        _ => None,
    }
}

/// What a running call shares with the server's read loop.
pub(crate) struct Call<'a> {
    /// Set by `notifications/cancelled`; the child is then stopped.
    pub cancelled: &'a AtomicBool,
    /// Called with each line the engine prints to stderr.
    pub progress: &'a (dyn Fn(&str) + Sync),
}

/// A JSON-RPC error for a call that names no known tool; everything wrong
/// with a known tool's input is a tool result, so the model can correct it.
pub(crate) fn call(params: &Value, call: &Call) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("tools/call needs a tool name")?;
    let tool = tool(name).ok_or(format!("unknown tool: {name}"))?;
    let empty = Map::new();
    let args = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(args)) => args,
        Some(_) => return Ok(tool_error("arguments must be an object")),
    };
    let outcome = resolve_cwd(args).and_then(|cwd| {
        let argv = argv(&tool, args)?;
        Ok((cwd.clone(), supervise(&cwd, &argv, call)?))
    });
    Ok(match outcome {
        Ok((cwd, outcome)) => (tool.read)(json!({ "cwd": cwd.display().to_string() }), outcome),
        Err(message) => tool_error(&message),
    })
}

/// The command line for one call. Every value goes in `--flag=value` form
/// and the positional after `--`, so no argument can become a flag.
fn argv(tool: &Tool, args: &Map<String, Value>) -> Result<Vec<String>, String> {
    let known = |k: &str| {
        k == "cwd" || Some(k) == tool.positional || tool.args.iter().any(|(name, _)| *name == k)
    };
    if let Some(unknown) = args.keys().find(|k| !known(k)) {
        return Err(format!("unknown argument `{unknown}`"));
    }
    let mut argv = vec![tool.command.to_string(), "--json".to_string()];
    for &(name, kind) in tool.args {
        let flag = name.replace('_', "-");
        let value = args.get(name).filter(|v| !v.is_null());
        let text = |v: &Value| {
            v.as_str()
                .map(str::to_string)
                .ok_or(format!("`{name}` must be a string"))
        };
        let switch = |v: Option<&Value>| match v {
            None => Ok(false),
            Some(v) => v.as_bool().ok_or(format!("`{name}` must be true or false")),
        };
        match (kind, value) {
            (Kind::Headed, value) if !args.contains_key("from_run") => {
                argv.push(
                    if switch(value)? {
                        "--headed"
                    } else {
                        "--headless"
                    }
                    .into(),
                );
            }
            (Kind::Check, None) => return Err("`check` is required: ai or sap".into()),
            (Kind::Reviewed, value) => {
                let applies = argv
                    .iter()
                    .any(|a| a == "--apply" || a == "--apply-proposal");
                if applies && !switch(value)? {
                    return Err(UNREVIEWED.into());
                }
            }
            (_, None) | (Kind::Headed, _) => {}
            (Kind::Text, Some(v)) => argv.push(format!("--{flag}={}", text(v)?)),
            (Kind::Number, Some(v)) => {
                let n = v.as_u64().ok_or(format!("`{name}` must be a number"))?;
                argv.push(format!("--{flag}={n}"));
            }
            (Kind::Switch, value) => {
                if switch(value)? {
                    argv.push(format!("--{flag}"));
                }
            }
            (Kind::Vars, Some(v)) => {
                let vars = v.as_object().ok_or("`var` must be an object")?;
                for (key, value) in vars {
                    let value = value
                        .as_str()
                        .ok_or(format!("`var.{key}` must be a string"))?;
                    argv.push(format!("--var={key}={value}"));
                }
            }
            (Kind::List, Some(v)) => {
                let items = v.as_array().ok_or(format!("`{name}` must be a list"))?;
                for item in items {
                    argv.push(format!("--{flag}={}", text(item)?));
                }
            }
            (Kind::Check, Some(v)) => match text(v)?.as_str() {
                check @ ("ai" | "sap") => argv.push(format!("--{check}")),
                other => {
                    return Err(refused_check(other)
                        .map(str::to_string)
                        .unwrap_or(format!("`check` must be ai or sap, not `{other}`")))
                }
            },
        }
    }
    if let Some(positional) = tool.positional {
        let value = string(args, positional)?.ok_or(format!("`{positional}` is required"))?;
        argv.extend(["--".to_string(), value]);
    }
    Ok(argv)
}

/// Applying overwrites a committed trace, so the person decides, as with
/// `allow_exec` (plan 17, decision 4).
const UNREVIEWED: &str = "Nothing was applied: `apply` and `apply_proposal` overwrite the \
    committed trace. Heal without them, show the person the proposal, and call again with \
    reviewed: true only if they say to apply it.";

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

fn supervise(cwd: &Path, argv: &[String], call: &Call) -> Result<Outcome, String> {
    let exe = flowproof_exe()?;
    let mut command = Command::new(&exe);
    command
        .args(argv)
        .current_dir(cwd)
        .env("FLOWPROOF_NO_UPDATE_CHECK", "1")
        // Consent is per call, as `allow_exec` (plan 17, decision 4): an
        // opt-in the client happened to inherit must not stand in for it.
        .env_remove("FLOWPROOF_ALLOW_EXEC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so a cancel also reaches the browser and the
    // suite shell the engine started.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command
        .spawn()
        .map_err(|e| format!("starting {}: {e}", exe.display()))?;
    let (Some(mut stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err("the engine's output was not piped".into());
    };
    std::thread::scope(|scope| {
        let json = scope.spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.read_to_end(&mut bytes);
            serde_json::from_slice(&bytes).ok()
        });
        // Each stderr line is the engine's own progress output, the same
        // lines the desktop shows while a job runs.
        let tail = scope.spawn(move || {
            let mut tail = VecDeque::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                (call.progress)(&line);
                if tail.len() == 20 {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
            Vec::from(tail).join("\n")
        });
        let code = wait(&mut child, call.cancelled)?.code();
        Ok(Outcome {
            code,
            json: json.join().unwrap_or(None),
            stderr: tail.join().unwrap_or_default(),
        })
    })
}

/// How long a cancelled engine gets to close its browser and exit before its
/// whole process group is killed.
const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// Wait for the child, stopping it once the call is cancelled. The same
/// sequence as the desktop's cancel: SIGTERM to the engine so it can close
/// its shared browser, then to what is left in its group (a browser `record`
/// launched for itself, a suite shell), and SIGKILL to the group if the
/// engine outlives the grace.
fn wait(child: &mut Child, cancelled: &AtomicBool) -> Result<ExitStatus, String> {
    let mut stopping: Option<Instant> = None;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| format!("waiting: {e}"))? {
            if stopping.is_some() {
                signal(child.id(), true, false);
            }
            return Ok(status);
        }
        match stopping {
            None if cancelled.load(Ordering::SeqCst) => {
                signal(child.id(), false, false);
                if cfg!(not(unix)) {
                    let _ = child.kill();
                }
                stopping = Some(Instant::now());
            }
            Some(since) if since.elapsed() > CANCEL_GRACE => {
                signal(child.id(), true, true);
                let _ = child.kill();
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
fn signal(pid: u32, group: bool, kill: bool) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    let signal = if kill { libc::SIGKILL } else { libc::SIGTERM };
    // SAFETY: kill(2) on a child this call spawned, and on the process group
    // it leads (spawned with `process_group(0)`). Neither touches memory.
    unsafe {
        if group {
            libc::killpg(pid, signal);
        } else {
            libc::kill(pid, signal);
        }
    }
}

#[cfg(not(unix))]
fn signal(_pid: u32, _group: bool, _kill: bool) {}

/// A single flow's `run --json` is `{report, report_path, ...}`; lift the
/// report so its steps are at `report.steps`, not `report.report.steps`,
/// and return the other fields (`report_path`, `skipped`) to sit beside it.
/// A suite's summary (each flow in `flows` keeps its own `report`) and
/// `audit`'s report pass through as they are.
fn lift_single_flow(json: Value) -> (Value, Map<String, Value>) {
    match json {
        Value::Object(mut fields)
            if fields.get("report").is_some_and(Value::is_object)
                && fields.contains_key("report_path")
                && !fields.contains_key("flows") =>
        {
            let report = fields.remove("report").unwrap_or_default();
            (report, fields)
        }
        other => (other, Map::new()),
    }
}

/// Exit 0 and 1 with a report are a pass and a fail (`run`, `audit`).
fn report_result(mut result: Value, outcome: Outcome) -> Value {
    match (outcome.code, outcome.json) {
        (Some(code @ (0 | 1)), Some(report)) => {
            result["exit_code"] = json!(code);
            result["passed"] = json!(code == 0);
            let (report, beside) = lift_single_flow(report);
            result["report"] = report;
            for (key, value) in beside {
                if result.get(&key).is_none() {
                    result[key] = value;
                }
            }
            tool_result(result, false)
        }
        (code, report) => engine_error(result, code, report, outcome.stderr),
    }
}

/// Exit 0 is a trace written (or a flow skipped by its gate), or for `heal`
/// a proposal. A step that needs clarifying is data for the agent to act
/// on, not an error.
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

/// `doctor` prints its verdict, `pass` and the checks behind it, whatever
/// it exits with: a failed check is the answer, not an error.
fn checked_result(mut result: Value, outcome: Outcome) -> Value {
    match outcome.json {
        Some(verdict) if verdict.get("pass").is_some() => {
            result["exit_code"] = json!(outcome.code);
            result["passed"] = verdict["pass"].clone();
            result["report"] = verdict;
            tool_result(result, false)
        }
        json => engine_error(result, outcome.code, json, outcome.stderr),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn heal_argv(args: Value) -> Vec<String> {
        let tool = tool("flowproof_heal").expect("heal");
        argv(&tool, args.as_object().expect("object")).expect("argv")
    }

    #[test]
    fn heal_from_a_run_drives_no_app_so_passes_no_browser_flag() {
        let live = heal_argv(json!({ "spec": "a.flow.yaml" }));
        assert_eq!(live, ["heal", "--json", "--headless", "--", "a.flow.yaml"]);
        let from_run = heal_argv(json!({ "spec": "a.flow.yaml", "from_run": "r", "step": ["s1"] }));
        assert_eq!(
            from_run,
            [
                "heal",
                "--json",
                "--from-run=r",
                "--step=s1",
                "--",
                "a.flow.yaml"
            ]
        );
    }

    #[test]
    fn applying_needs_the_person_to_have_reviewed_the_proposal() {
        let tool = tool("flowproof_heal").expect("heal");
        for apply in ["apply", "apply_proposal"] {
            let mut unreviewed = json!({ "spec": "a.flow.yaml" });
            unreviewed[apply] = json!(true);
            let refused = argv(&tool, unreviewed.as_object().expect("object"));
            assert_eq!(refused, Err(UNREVIEWED.to_string()), "{apply}");
            let mut said_no = unreviewed.clone();
            said_no["reviewed"] = json!(false);
            assert!(argv(&tool, said_no.as_object().expect("object")).is_err());
        }
        let reviewed = heal_argv(json!({ "spec": "a.flow.yaml", "apply": true, "reviewed": true }));
        assert_eq!(
            reviewed,
            [
                "heal",
                "--json",
                "--apply",
                "--headless",
                "--",
                "a.flow.yaml"
            ]
        );
    }

    #[test]
    fn a_single_flow_report_is_lifted_and_a_suite_is_not() {
        let single = json!({ "report": { "steps": [] }, "report_path": "r/result.json" });
        let (report, beside) = lift_single_flow(single);
        assert_eq!(report, json!({ "steps": [] }));
        assert_eq!(
            Value::Object(beside),
            json!({ "report_path": "r/result.json" })
        );
        let skipped = json!({ "report": {}, "report_path": null, "skipped": "env not set" });
        let (_, beside) = lift_single_flow(skipped);
        assert_eq!(beside["skipped"], "env not set");
        let suite = json!({ "passed": true, "flows": [{ "report": {}, "report_path": null }] });
        assert_eq!(lift_single_flow(suite.clone()), (suite, Map::new()));
        let audit = json!({ "suite": "flows", "checks": [] });
        assert_eq!(lift_single_flow(audit.clone()), (audit, Map::new()));
    }
}
