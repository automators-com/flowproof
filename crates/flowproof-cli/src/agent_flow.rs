//! Orchestration for `app: agent` flows: the only place that sees both
//! the spec and the proxy/runner, so it is where record and replay are
//! tied together for the model boundary.
//!
//! An agent flow does not use the record/replay driver trait at all - its
//! "trace" is a cassette, not a step log - so it gets its own record and
//! run paths here, dispatched from the CLI when `app: agent`.
//!
//! Both phases resolve the same three things from the spec: the `tools:`
//! mocks (a tool with a `result:` is substituted; a name-only entry is a
//! declaration and passes through), the prompt (the user turn handed to
//! the agent), and the assertions (`assert_tool_call` /
//! `assert_no_tool_call` / `assert: reply ...`), which are checked against
//! the resulting trajectory. Recording asserts: a trace is not written for
//! a run whose assertions fail, the same rule every other app kind has.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use flowproof_adapters::agent_proxy::AgentProxy;
use flowproof_adapters::agent_runner::{run_against, run_http, AgentRun};
use flowproof_agent::{FlowSpec, SpecStep};
use flowproof_trace::cassette::Cassette;
use flowproof_trace::substitution::Mocks;
use flowproof_trace::toolcalls::{self, ToolCallExpectation};

/// How long an agent process gets before it is killed. Generous: a
/// multi-turn agent against a real model at record can be slow, and replay
/// is bounded by the agent's own logic, not the model.
const AGENT_TIMEOUT: Duration = Duration::from_secs(300);

/// The env var naming the real model to record against. A record run needs
/// a real OpenAI-compatible endpoint; this points at it, falling back to
/// the standard `OPENAI_BASE_URL` the developer already has set.
const UPSTREAM_VARS: [&str; 2] = ["FLOWPROOF_AGENT_UPSTREAM", "OPENAI_BASE_URL"];

/// The env vars a real-model KEY is read from at record time. flowproof
/// passes it straight into the outbound `Authorization` header and never
/// anywhere else: the trace stores request bodies only, so no key is ever
/// written to disk. Bearer-prefixed if it is a bare key.
const UPSTREAM_KEY_VARS: [&str; 3] = ["FLOWPROOF_AGENT_KEY", "ANTHROPIC_API_KEY", "OPENAI_API_KEY"];

/// The env var the prompt is delivered through. A documented handle, so an
/// agent reads its task the same way it reads its model URL.
const PROMPT_VAR: &str = "FLOWPROOF_PROMPT";

/// The on-disk shape of an `app: agent` trace: a self-contained JSON
/// document. A new app kind gets a new trace shape rather than bending the
/// step-log format; old readers never open an agent trace.
#[derive(serde::Serialize, serde::Deserialize)]
struct AgentTrace {
    /// Always `"agent"`, so a reader can tell this file apart.
    app: String,
    /// The effective mocks, snapshotted so replay does not change behavior
    /// when the spec's mock is edited without re-recording - the same
    /// "rules travel in the trace" rule browser network mocks follow.
    #[serde(default)]
    mocks: BTreeMap<String, serde_json::Value>,
    cassette: Cassette,
}

/// The mocks a flow substitutes: a tool with a non-null `result:`. A
/// name-only entry (result defaults to null) is a declaration only, per
/// Fable - it validates `assert_tool_call` targets and its real result
/// passes through unsubstituted.
fn mocks_of(spec: &FlowSpec) -> Mocks {
    spec.tools
        .iter()
        .filter(|t| !t.result.is_null())
        .map(|t| (t.name.clone(), t.result.clone()))
        .collect()
}

/// The prompt handed to the agent: every `prompt:` step, in order, joined
/// by newlines. Validation already guaranteed at least one exists.
fn prompt_of(spec: &FlowSpec) -> String {
    spec.steps
        .iter()
        .filter_map(|s| match s {
            SpecStep::Prompt { prompt } => Some(prompt.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The agent's own env, with `${VAR}` references resolved.
fn agent_env(spec: &FlowSpec) -> Result<BTreeMap<String, String>, String> {
    let mut env = BTreeMap::new();
    if let Some(agent) = &spec.agent {
        for (key, value) in &agent.env {
            let resolved =
                flowproof_trace::secret::resolve_refs(value).map_err(|e| e.to_string())?;
            env.insert(key.clone(), resolved);
        }
    }
    Ok(env)
}

/// Which system under test a flow drives, resolved from the spec. A trace
/// records NEITHER of these - a cassette is driver-blind, so a recording made
/// through a `command:` replays through a `url:` and vice versa.
enum Driver {
    /// A process flowproof starts, with the proxy on an ephemeral port.
    Command(String),
    /// An already-running service flowproof POSTs to, with the proxy bound to
    /// the fixed port the service already calls.
    Http {
        url: String,
        headers: BTreeMap<String, String>,
        proxy_port: u16,
    },
}

/// Everything a phase needs pulled off the spec once.
struct Plan {
    driver: Driver,
    /// The process env (command driver): the injected proxy vars plus the
    /// spec's own, PROMPT_VAR among them. Unused by the http driver, which
    /// carries the prompt in the trigger body instead.
    env: BTreeMap<String, String>,
    /// The joined prompt steps: the http trigger's POST body.
    prompt: String,
    mocks: Mocks,
    strict: bool,
    tool_calls: Vec<ToolCallExpectation>,
    forbidden: Vec<ToolCallExpectation>,
    reply_contains: Vec<String>,
}

impl Plan {
    /// The port the proxy binds: the fixed `proxy_port` for an http driver,
    /// ephemeral (`0`) for a process.
    fn proxy_port(&self) -> u16 {
        match &self.driver {
            Driver::Http { proxy_port, .. } => *proxy_port,
            Driver::Command(_) => 0,
        }
    }

    /// The hint appended to a reproduction failure when the driver is http:
    /// the usual reason a service made zero (or too few) model calls is that
    /// it is not pointed at the proxy. `None` for a process driver.
    fn http_hint(&self) -> Option<String> {
        match &self.driver {
            Driver::Http { proxy_port, .. } => Some(format!(
                "is the service pointed at http://127.0.0.1:{proxy_port}/v1?"
            )),
            Driver::Command(_) => None,
        }
    }

    /// Trigger the system under test against an already-started proxy.
    fn drive(&self, proxy: &AgentProxy) -> Result<AgentRun, String> {
        match &self.driver {
            Driver::Command(command) => {
                run_against(proxy, command, &self.env, AGENT_TIMEOUT).map_err(|e| e.to_string())
            }
            Driver::Http { url, headers, .. } => {
                run_http(proxy, url, headers, &self.prompt, AGENT_TIMEOUT)
                    .map_err(|e| e.to_string())
            }
        }
    }
}

/// Append the http driver's hint to a reproduction/progress failure, so a
/// mispointed service tells the author where to point it. A no-op for a
/// process driver.
fn with_http_hint(message: String, plan: &Plan) -> String {
    match plan.http_hint() {
        Some(hint) => format!("{message}\n{hint}"),
        None => message,
    }
}

fn plan(spec: &FlowSpec) -> Result<Plan, String> {
    let agent = spec
        .agent
        .as_ref()
        .ok_or("an app: agent flow needs an agent: block")?;
    let prompt = prompt_of(spec);
    let mut env = agent_env(spec)?;
    env.insert(PROMPT_VAR.to_string(), prompt.clone());

    // The driver: exactly one of command/url (the spec validator already
    // guaranteed the choice). `${VAR}` refs in the command, url, and header
    // values resolve here, at execution, and are never stored.
    let driver = match (&agent.command, &agent.url) {
        (Some(command), _) => Driver::Command(
            flowproof_trace::secret::resolve_refs(command).map_err(|e| e.to_string())?,
        ),
        (None, Some(url)) => {
            let proxy_port = agent.proxy_port.ok_or(
                "agent.url needs a proxy_port: the running service must already point its \
                 model calls at that local port",
            )?;
            let url = flowproof_trace::secret::resolve_refs(url).map_err(|e| e.to_string())?;
            let mut headers = BTreeMap::new();
            for (name, value) in &agent.headers {
                let resolved =
                    flowproof_trace::secret::resolve_refs(value).map_err(|e| e.to_string())?;
                headers.insert(name.clone(), resolved);
            }
            Driver::Http {
                url,
                headers,
                proxy_port,
            }
        }
        (None, None) => {
            return Err("an app: agent flow needs an agent.command or an agent.url".into())
        }
    };

    let mut tool_calls = Vec::new();
    let mut forbidden = Vec::new();
    let mut reply_contains = Vec::new();
    for step in &spec.steps {
        match step {
            SpecStep::AssertToolCall { assert_tool_call } => {
                tool_calls.push(parse_expectation(assert_tool_call)?);
            }
            SpecStep::AssertNoToolCall {
                assert_no_tool_call,
            } => {
                forbidden.push(parse_expectation(assert_no_tool_call)?);
            }
            SpecStep::Assert { assert } => {
                // v1 reply assertion: `reply contains <text>`.
                let trimmed = assert.trim();
                let rest = trimmed
                    .strip_prefix("reply contains ")
                    .or_else(|| trimmed.strip_prefix("reply is "));
                match rest {
                    Some(text) => reply_contains.push(text.trim().to_string()),
                    None => {
                        return Err(format!(
                            "an agent flow's `assert:` only supports `reply contains <text>` \
                             in v1; got `{trimmed}`"
                        ))
                    }
                }
            }
            _ => {}
        }
    }

    Ok(Plan {
        driver,
        env,
        prompt,
        mocks: mocks_of(spec),
        strict: spec.strict,
        tool_calls,
        forbidden,
        reply_contains,
    })
}

fn parse_expectation(text: &str) -> Result<ToolCallExpectation, String> {
    flowproof_agent::agent_steps::parse(text).map_err(|e| e.to_string())
}

/// Check the trajectory against the spec's assertions. Returns the first
/// failure, so record can refuse a trace and replay can fail the flow.
fn check_assertions(plan: &Plan, cassette: &Cassette) -> Result<(), String> {
    let calls = cassette.tool_calls();
    toolcalls::check_trajectory(&plan.tool_calls, &calls, plan.strict)
        .map_err(|e| e.to_string())?;
    for forbidden in &plan.forbidden {
        toolcalls::check_absent(forbidden, &calls).map_err(|e| e.to_string())?;
    }
    if !plan.reply_contains.is_empty() {
        let reply = cassette.reply().unwrap_or("");
        for want in &plan.reply_contains {
            if !reply.contains(want.as_str()) {
                return Err(format!(
                    "reply does not contain `{want}`; the agent's final message was `{reply}`"
                ));
            }
        }
    }
    Ok(())
}

/// Fail a run that did not actually exercise the agent, so an empty
/// trajectory cannot pass by making zero assertions true.
fn require_progress(run: &AgentRun, cassette: &Cassette, plan: &Plan) -> Result<(), String> {
    if let Some(err) = &run.upstream_error {
        return Err(format!(
            "recording touched the real model and it failed: {err}"
        ));
    }
    if cassette.is_empty() {
        let message = format!(
            "the agent made no model calls; it exited {} without talking to the proxy.\n\
             stderr:\n{}",
            run.exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "with no code".into()),
            run.stderr.trim()
        );
        return Err(with_http_hint(message, plan));
    }
    Ok(())
}

/// Record an `app: agent` flow: run it against a real model, capture the
/// trajectory, check the assertions, and write the cassette to `out`.
pub fn record(spec: &FlowSpec, out: &Path) -> Result<(), String> {
    let plan = plan(spec)?;
    let upstream = upstream()?;
    let auth = upstream_auth();
    let proxy = AgentProxy::record(&upstream, auth, plan.mocks.clone(), plan.proxy_port())
        .map_err(|e| format!("starting the record proxy: {e}"))?;
    let run = plan.drive(&proxy)?;
    let cassette = proxy.captured();
    drop(proxy);

    require_progress(&run, &cassette, &plan)?;
    // Recording asserts: no trace for a trajectory that fails the spec.
    check_assertions(&plan, &cassette)?;

    let trace = AgentTrace {
        app: "agent".into(),
        mocks: plan.mocks.into_iter().collect(),
        cassette,
    };
    let json = serde_json::to_string_pretty(&trace).map_err(|e| e.to_string())?;
    std::fs::write(out, json).map_err(|e| format!("writing {}: {e}", out.display()))?;
    Ok(())
}

/// Replay an `app: agent` flow: serve the recorded cassette, run the
/// agent, and check that the trajectory reproduced and the assertions
/// still hold.
pub fn replay(spec: &FlowSpec, trace_path: &Path) -> Result<(), String> {
    let plan = plan(spec)?;
    let raw = std::fs::read_to_string(trace_path)
        .map_err(|e| format!("reading {}: {e}", trace_path.display()))?;
    let trace: AgentTrace = serde_json::from_str(&raw)
        .map_err(|e| format!("{} is not an agent trace: {e}", trace_path.display()))?;
    let expected = trace.cassette.len();

    // The mocks that TRAVEL IN THE TRACE win, so replay reproduces the
    // recording even if the spec's mock was edited since - the spec's copy
    // is only used at record.
    let mocks: Mocks = trace.mocks.into_iter().collect();
    let proxy = AgentProxy::start(trace.cassette, mocks, plan.proxy_port())
        .map_err(|e| format!("starting the replay proxy: {e}"))?;
    let run = plan.drive(&proxy)?;
    let cassette = proxy.captured(); // empty in replay
    drop(proxy);
    let _ = cassette;

    run.reproduced(expected)
        .map_err(|e| with_http_hint(e, &plan))?;
    // The cassette to assert against is the recording; re-load it, since
    // the proxy consumed it.
    let trace: AgentTrace = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    check_assertions(&plan, &trace.cassette)?;
    Ok(())
}

/// The real model to record against.
fn upstream() -> Result<String, String> {
    for var in UPSTREAM_VARS {
        if let Ok(value) = std::env::var(var) {
            if !value.trim().is_empty() {
                return Ok(value);
            }
        }
    }
    Err(format!(
        "recording an app: agent flow needs a real model to record against; \
         set {}",
        UPSTREAM_VARS.join(" or ")
    ))
}

/// The auth value for the real model, read from flowproof's environment and
/// handed to the proxy, which puts it in the header its dialect needs: an
/// OpenAI request gets `Authorization: <value>`, an Anthropic request gets
/// `x-api-key: <value>` (a `Bearer ` prefix stripped there defensively).
///
/// So the wrapping is dialect-aware: an OpenAI bare key becomes `Bearer
/// <key>` the way v1 always did, but an `ANTHROPIC_API_KEY` is passed BARE,
/// because `x-api-key` carries the raw key and a `Bearer ` prefix would be
/// wrong. A value that already names a scheme (contains a space) is passed
/// as written. `None` when no key is set - a local fake model needs none.
fn upstream_auth() -> Option<String> {
    for var in UPSTREAM_KEY_VARS {
        if let Ok(value) = std::env::var(var) {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            if value.contains(' ') {
                return Some(value.to_string());
            }
            // Anthropic authenticates with a bare key in `x-api-key`, so it
            // must not be Bearer-wrapped; OpenAI keeps its Bearer scheme.
            if var == "ANTHROPIC_API_KEY" {
                return Some(value.to_string());
            }
            return Some(format!("Bearer {value}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowproof_trace::cassette::{Message, Turn, TurnRequest, TurnResponse};
    use std::io::{BufRead, Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};

    /// A neutral one-turn cassette: a user prompt, an assistant reply. This
    /// is exactly the shape a `command:` record produces - NOTHING in it names
    /// how it was driven, which is what lets a `url:` replay it.
    fn neutral_cassette(prompt: &str, reply: &str) -> Cassette {
        Cassette {
            turns: vec![Turn {
                protocol: flowproof_trace::cassette::default_protocol(),
                request: TurnRequest {
                    model: "gpt-4o".into(),
                    messages: vec![Message::new("user", prompt)],
                    tools: vec!["search_flights".into()],
                },
                response: TurnResponse {
                    message: Message::new("assistant", reply),
                    stop_reason: None,
                },
            }],
        }
    }

    /// Write a trace file the way `record` would, but hand-built so the test
    /// needs no real model - a driver-blind cassette on disk.
    fn write_trace(cassette: Cassette) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("flowproof-agent-flow");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("trace-{}.json", std::process::id()));
        let trace = AgentTrace {
            app: "agent".into(),
            mocks: BTreeMap::new(),
            cassette,
        };
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&trace).expect("serialize"),
        )
        .expect("write trace");
        path
    }

    /// A free localhost port: bind ephemeral, read the number, release it.
    /// A tiny race, acceptable in a test - the proxy re-binds it immediately.
    fn free_port() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        port
    }

    fn read_prompt(stream: &mut TcpStream) -> String {
        let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
        let mut line = String::new();
        reader.read_line(&mut line).ok();
        let mut length = 0usize;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                break;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).ok();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        json.get("prompt")
            .and_then(|p| p.as_str())
            .unwrap_or_default()
            .to_string()
    }

    fn answer_trigger(stream: &mut TcpStream, body: &str) {
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }

    /// One chat-completions POST to the proxy at the FIXED port a `url:`
    /// service is pointed at, over a raw socket. `Some(body)` on 200.
    fn call_proxy(proxy_port: u16, prompt: &str) -> Option<String> {
        let addr = format!("127.0.0.1:{proxy_port}");
        let payload = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": prompt}],
            "tools": [{"type": "function", "function": {"name": "search_flights"}}],
        })
        .to_string();
        let mut stream = TcpStream::connect(&addr).ok()?;
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nhost: {addr}\r\n\
             content-type: application/json\r\ncontent-length: {}\r\n\r\n{payload}",
            payload.len()
        );
        stream.write_all(request.as_bytes()).ok()?;
        let mut raw = String::new();
        stream.read_to_string(&mut raw).ok()?;
        let status: u16 = raw.split_whitespace().nth(1)?.parse().ok()?;
        (status == 200)
            .then(|| raw.split("\r\n\r\n").nth(1).map(str::to_string))
            .flatten()
    }

    /// A fake already-running SUT: on the trigger POST it makes one model
    /// call to the proxy at `proxy_port` (like a real service pointed there),
    /// then answers. `calls_proxy=false` is the MISPOINTED service.
    fn spawn_service(proxy_port: u16, calls_proxy: bool) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind service");
        let addr = listener.local_addr().expect("addr");
        let url = format!("http://{addr}/run");
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let prompt = read_prompt(&mut stream);
                if calls_proxy {
                    call_proxy(proxy_port, &prompt);
                }
                answer_trigger(&mut stream, "{\"ok\":true}");
            }
        });
        (url, handle)
    }

    /// D4: a cassette carrying no driver replays through `url:` even though a
    /// `command:` shape is what produced it - the trace is driver-blind. And
    /// the on-disk trace names no driver at all.
    #[test]
    fn a_driver_blind_cassette_replays_via_url() {
        let trace = write_trace(neutral_cassette("Reserve a table", "Booked KQ311."));
        // The trace JSON must not persist a driver: no command, no url.
        let raw = std::fs::read_to_string(&trace).expect("read");
        assert!(!raw.contains("command"), "trace names a driver: {raw}");
        assert!(!raw.contains("\"url\""), "trace names a driver: {raw}");

        let proxy_port = free_port();
        let (url, handle) = spawn_service(proxy_port, true);
        let spec = FlowSpec::parse(&format!(
            "name: url flow\napp: agent\nagent:\n  url: {url}\n  proxy_port: {proxy_port}\nsteps:\n  - prompt: Reserve a table\n  - assert: reply contains Booked\n"
        ))
        .expect("spec parses");

        replay(&spec, &trace).expect("driver-blind replay via url passes");
        handle.join().ok();
    }

    /// A mispointed service (never calls the proxy) fails replay, and the
    /// failure carries the http hint naming where the service should point.
    #[test]
    fn a_mispointed_service_replay_reports_the_http_hint() {
        let trace = write_trace(neutral_cassette("Reserve a table", "Booked KQ311."));
        let proxy_port = free_port();
        let (url, handle) = spawn_service(proxy_port, false);
        let spec = FlowSpec::parse(&format!(
            "name: url flow\napp: agent\nagent:\n  url: {url}\n  proxy_port: {proxy_port}\nsteps:\n  - prompt: Reserve a table\n  - assert: reply contains Booked\n"
        ))
        .expect("spec parses");

        let why = replay(&spec, &trace).expect_err("mispointed service must fail");
        handle.join().ok();
        assert!(why.contains("made 0 model calls"), "{why}");
        assert!(
            why.contains(&format!("http://127.0.0.1:{proxy_port}/v1")),
            "names where to point the service: {why}"
        );
    }
}
