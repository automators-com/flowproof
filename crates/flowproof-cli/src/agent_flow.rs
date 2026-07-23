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
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use flowproof_adapters::agent_proxy::AgentProxy;
use flowproof_adapters::agent_runner::{run_against, run_http, AgentRun};
use flowproof_adapters::mcp_stdio::{McpCall, McpOut, McpPlan};
use flowproof_agent::{FlowSpec, SpecStep};
use flowproof_trace::cassette::Cassette;
use flowproof_trace::substitution::Mocks;
use flowproof_trace::toolcalls::{self, ToolCallExpectation};

/// How long to wait for a stand-in's out file after the agent exits. The
/// agent closing its side prompts the stand-in to write and exit, but the
/// stand-in is a grandchild that may flush a beat later, so the read polls
/// briefly rather than racing the rename.
const MCP_OUT_WAIT: Duration = Duration::from_secs(10);

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
    /// The recorded MCP lanes, one per stdio server. ADDITIVE and skipped
    /// when empty, so a v1/v2 trace and any mcp-less agent flow serialize
    /// byte-identical (no `mcp` key), and an old trace still deserializes
    /// (the field defaults to empty).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    mcp: BTreeMap<String, McpServerTrace>,
}

/// One MCP server's recorded lane: its mocks, snapshotted (travel-in-trace,
/// like `AgentTrace.mocks`), and the JSON-RPC calls captured in order.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct McpServerTrace {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    mocks: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    calls: Vec<McpCall>,
}

/// The MCP boundary set up for one phase: the per-run temp dir holding the
/// `<server>.plan.json` files the stand-in reads and the `<server>.out.json`
/// files it writes, plus the env the agent needs to reach the stand-in.
///
/// `None` when the spec declares no `mcp:` servers, so an mcp-less flow is
/// byte-for-byte the flow it always was.
struct McpContext {
    dir: PathBuf,
    mode: &'static str,
    /// (server name, env var name), one per declared server.
    servers: Vec<(String, String)>,
}

impl McpContext {
    /// Prepare the boundary for `mode` (`"record"` or `"replay"`): make the
    /// run dir and write each server's plan. In replay the plan carries the
    /// recorded lane from `trace_mcp`. Returns `None` for an mcp-less flow.
    fn setup(
        spec: &FlowSpec,
        mode: &'static str,
        trace_mcp: &BTreeMap<String, McpServerTrace>,
    ) -> Result<Option<Self>, String> {
        if spec.mcp.is_empty() {
            return Ok(None);
        }
        // A per-setup nonce so record and replay in one process get distinct
        // dirs and a re-run starts clean.
        let dir = std::env::temp_dir().join(format!(
            "flowproof-mcp-{}-{}-{mode}",
            std::process::id(),
            mcp_nonce()
        ));
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating MCP run dir: {e}"))?;

        let mut servers = Vec::new();
        for server in &spec.mcp {
            let command = flowproof_trace::secret::resolve_refs(&server.command)
                .map_err(|e| e.to_string())?;
            // MCP mocks are the per-server tools with a non-null result -
            // the same "a name-only entry is a declaration" rule the model
            // boundary uses.
            let mocks: BTreeMap<String, serde_json::Value> = server
                .tools
                .iter()
                .filter(|t| !t.result.is_null())
                .map(|t| (t.name.clone(), t.result.clone()))
                .collect();
            let calls = if mode == "replay" {
                trace_mcp
                    .get(&server.name)
                    .map(|t| t.calls.clone())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let plan = McpPlan {
                mode: mode.to_string(),
                command,
                mocks,
                calls,
            };
            let plan_path = dir.join(format!("{}.plan.json", server.name));
            std::fs::write(
                &plan_path,
                serde_json::to_string(&plan).map_err(|e| e.to_string())?,
            )
            .map_err(|e| format!("writing {}: {e}", plan_path.display()))?;
            servers.push((server.name.clone(), env_var_name(&server.name)));
        }
        Ok(Some(McpContext { dir, mode, servers }))
    }

    /// The env the agent's process needs: the run dir, the mode, and one
    /// `FLOWPROOF_MCP_SERVER_<NAME>` per server pointing at this executable's
    /// `mcp-stdio` subcommand. The SUT's MCP config points its server
    /// command at that variable - the documented contract.
    fn env_vars(&self) -> Result<BTreeMap<String, String>, String> {
        // `current_exe()` is the flowproof binary in production. An explicit
        // `FLOWPROOF_MCP_EXE` overrides it for the cases where it is not: a
        // wrapper on PATH, or a test harness driving `run_cli` in-process
        // (where `current_exe()` is the test binary, not flowproof).
        let exe = match std::env::var("FLOWPROOF_MCP_EXE") {
            Ok(path) if !path.trim().is_empty() => path,
            _ => std::env::current_exe()
                .map_err(|e| format!("finding the flowproof executable for mcp-stdio: {e}"))?
                .display()
                .to_string(),
        };
        let mut env = BTreeMap::new();
        env.insert(
            "FLOWPROOF_MCP_DIR".to_string(),
            self.dir.display().to_string(),
        );
        env.insert("FLOWPROOF_MCP_MODE".to_string(), self.mode.to_string());
        for (name, var) in &self.servers {
            // The exe path is quoted so an argv splitter survives spaces in
            // it; the agent spawns this command as its MCP server.
            env.insert(var.clone(), format!("\"{exe}\" mcp-stdio --server {name}"));
        }
        Ok(env)
    }

    /// After a RECORD run: read each server's out file, enforce the progress
    /// guard (a declared server the agent never contacted), and fold the
    /// captured calls plus the snapshotted mocks into the trace.
    fn collect_record(&self, spec: &FlowSpec) -> Result<BTreeMap<String, McpServerTrace>, String> {
        let mut out = BTreeMap::new();
        for server in &spec.mcp {
            let out_path = self.dir.join(format!("{}.out.json", server.name));
            let parsed = read_out(&out_path).ok_or_else(|| wiring_guard(&server.name))?;
            if let Some(err) = &parsed.error {
                return Err(format!(
                    "recording the MCP server `{}` failed: {err}",
                    server.name
                ));
            }
            // The progress guard: a real MCP client handshakes with
            // `initialize`, so a lane with no captured call means the agent
            // never spawned the stand-in.
            if parsed.calls.is_empty() {
                return Err(wiring_guard(&server.name));
            }
            let mocks = server
                .tools
                .iter()
                .filter(|t| !t.result.is_null())
                .map(|t| (t.name.clone(), t.result.clone()))
                .collect();
            out.insert(
                server.name.clone(),
                McpServerTrace {
                    mocks,
                    calls: parsed.calls,
                },
            );
        }
        Ok(out)
    }

    /// After a REPLAY run: a divergence on any lane fails the run with its
    /// reason; fewer served than recorded on any lane fails it too (the
    /// `reproduced` analog for the MCP boundary).
    fn check_replay(
        &self,
        spec: &FlowSpec,
        trace_mcp: &BTreeMap<String, McpServerTrace>,
    ) -> Result<(), String> {
        for server in &spec.mcp {
            let out_path = self.dir.join(format!("{}.out.json", server.name));
            let parsed = read_out(&out_path).ok_or_else(|| wiring_guard(&server.name))?;
            if let Some(div) = &parsed.divergence {
                return Err(format!(
                    "the MCP server `{}` diverged at call {}: {}",
                    server.name,
                    div.index + 1,
                    div.detail
                ));
            }
            let expected = trace_mcp
                .get(&server.name)
                .map(|t| t.calls.len())
                .unwrap_or(0);
            let served = parsed.served.unwrap_or(0);
            if served != expected {
                return Err(format!(
                    "the agent made {served} MCP calls to `{}`, the recording has {expected}",
                    server.name
                ));
            }
        }
        Ok(())
    }
}

impl Drop for McpContext {
    fn drop(&mut self) {
        // The run dir is scratch: plans in, outs out, nothing to keep.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The record-time wiring guard message, shared by "no out file" and "no
/// captured handshake": both mean the agent's config still points at the
/// real server instead of flowproof's stand-in.
fn wiring_guard(server: &str) -> String {
    format!(
        "the agent never spawned flowproof's MCP stand-in for `{server}`; its config still \
         points at the real server (point it at ${{FLOWPROOF_MCP_SERVER_{}}})",
        env_suffix(server)
    )
}

/// Poll for a stand-in's out file after the agent exits: the atomic rename
/// means a present file is complete, so a successful read is the whole file.
fn read_out(path: &Path) -> Option<McpOut> {
    let deadline = Instant::now() + MCP_OUT_WAIT;
    loop {
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(parsed) = serde_json::from_str::<McpOut>(&raw) {
                return Some(parsed);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A monotonic-ish nonce so two `setup`s in one process get distinct dirs.
fn mcp_nonce() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    u128::from(COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// The uppercase, `_`-sanitized suffix of a server name for its env var.
fn env_suffix(server: &str) -> String {
    server
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// The full `FLOWPROOF_MCP_SERVER_<NAME>` env var name for a server.
fn env_var_name(server: &str) -> String {
    format!("FLOWPROOF_MCP_SERVER_{}", env_suffix(server))
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
    let mut plan = plan(spec)?;
    // Set up the MCP boundary BEFORE the agent starts: write the plans and
    // inject the env that points the agent's MCP config at the stand-in.
    let mcp = McpContext::setup(spec, "record", &BTreeMap::new())?;
    if let Some(ctx) = &mcp {
        plan.env.extend(ctx.env_vars()?);
    }
    let upstream = upstream()?;
    let auth = upstream_auth();
    let proxy = AgentProxy::record(&upstream, auth, plan.mocks.clone(), plan.proxy_port())
        .map_err(|e| format!("starting the record proxy: {e}"))?;
    let run = plan.drive(&proxy)?;
    let cassette = proxy.captured();
    drop(proxy);

    require_progress(&run, &cassette, &plan)?;
    // The MCP lanes, folded in with the same progress guard: a declared
    // server the agent never contacted fails the record.
    let mcp_trace = match &mcp {
        Some(ctx) => ctx.collect_record(spec)?,
        None => BTreeMap::new(),
    };
    // Recording asserts: no trace for a trajectory that fails the spec.
    check_assertions(&plan, &cassette)?;

    let trace = AgentTrace {
        app: "agent".into(),
        mocks: plan.mocks.into_iter().collect(),
        cassette,
        mcp: mcp_trace,
    };
    let json = serde_json::to_string_pretty(&trace).map_err(|e| e.to_string())?;
    std::fs::write(out, json).map_err(|e| format!("writing {}: {e}", out.display()))?;
    Ok(())
}

/// Replay an `app: agent` flow: serve the recorded cassette, run the
/// agent, and check that the trajectory reproduced and the assertions
/// still hold.
pub fn replay(spec: &FlowSpec, trace_path: &Path) -> Result<(), String> {
    let mut plan = plan(spec)?;
    let raw = std::fs::read_to_string(trace_path)
        .map_err(|e| format!("reading {}: {e}", trace_path.display()))?;
    let trace: AgentTrace = serde_json::from_str(&raw)
        .map_err(|e| format!("{} is not an agent trace: {e}", trace_path.display()))?;
    let expected = trace.cassette.len();

    // Set up the MCP boundary from the recorded lanes BEFORE the agent
    // starts: the lanes TRAVEL IN THE TRACE, so replay serves what was
    // recorded even if the spec's mocks were edited since.
    let mcp = McpContext::setup(spec, "replay", &trace.mcp)?;
    if let Some(ctx) = &mcp {
        plan.env.extend(ctx.env_vars()?);
    }

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
    // The MCP lanes reproduced: a divergence, or fewer served than recorded,
    // fails the run the same way the cassette's `reproduced` does.
    if let Some(ctx) = &mcp {
        ctx.check_replay(spec, &trace.mcp)?;
    }
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
            mcp: BTreeMap::new(),
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

    /// An mcp-less agent trace serializes byte-identical with and without
    /// the new field, and a trace written before v3.1 (no `mcp` key) still
    /// deserializes - the additive rule the whole slice rests on.
    #[test]
    fn an_mcp_less_trace_round_trips_byte_identical() {
        let trace = AgentTrace {
            app: "agent".into(),
            mocks: BTreeMap::new(),
            cassette: neutral_cassette("hi", "there"),
            mcp: BTreeMap::new(),
        };
        let json = serde_json::to_string_pretty(&trace).expect("serialize");
        // The `mcp` key is skipped when empty, so the bytes match a pre-v3.1
        // agent trace exactly.
        assert!(
            !json.contains("mcp"),
            "no mcp key on an mcp-less trace: {json}"
        );

        // A hand-built pre-v3.1 trace (no `mcp` field at all) deserializes,
        // its mcp map defaulting to empty.
        let v2 = r#"{"app":"agent","mocks":{},"cassette":{"turns":[]}}"#;
        let back: AgentTrace = serde_json::from_str(v2).expect("v2 trace still deserializes");
        assert!(back.mcp.is_empty(), "absent mcp defaults to empty");
    }

    /// A trace WITH an mcp lane carries it through the round trip, and it
    /// serializes only when non-empty.
    #[test]
    fn an_mcp_lane_survives_the_round_trip() {
        let mut mcp = BTreeMap::new();
        mcp.insert(
            "weather".to_string(),
            McpServerTrace {
                mocks: BTreeMap::new(),
                calls: vec![McpCall {
                    method: "tools/call".into(),
                    params: serde_json::json!({ "name": "get_weather", "arguments": {} }),
                    result: serde_json::json!({ "isError": false }),
                }],
            },
        );
        let trace = AgentTrace {
            app: "agent".into(),
            mocks: BTreeMap::new(),
            cassette: neutral_cassette("hi", "there"),
            mcp,
        };
        let json = serde_json::to_string(&trace).expect("serialize");
        assert!(json.contains("\"mcp\""), "mcp present: {json}");
        assert!(json.contains("get_weather"), "call carried: {json}");
        let back: AgentTrace = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.mcp["weather"].calls.len(), 1);
        assert_eq!(back.mcp["weather"].calls[0].method, "tools/call");
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
