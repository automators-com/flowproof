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
use flowproof_adapters::agent_runner::{run_against, run_against_contained, run_http, AgentRun};
use flowproof_adapters::egress::{AllowSet, Containment};
use flowproof_adapters::mcp_http::McpHttpServer;
use flowproof_adapters::mcp_stdio::{McpCall, McpOut, McpPlan, McpServerEvent};
use flowproof_agent::{FlowSpec, SpecStep};
use flowproof_trace::cassette::Cassette;
use flowproof_trace::egress::EgressEvent;
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

/// The env vars a real-model KEY is read from at record time.
///
/// The key never reaches disk: the trace stores request bodies only.
///
/// WHICH HEADER it goes out in is DIALECT-DEPENDENT, and getting this wrong
/// costs real debugging time, so it is spelled out here rather than left to
/// the reader to discover from `agent_proxy`:
///
///   - OpenAI-compatible upstream: `Authorization: <key>`, Bearer-prefixed if
///     the key is bare.
///   - Anthropic upstream: `x-api-key: <key>`, with any `Bearer ` prefix
///     stripped, because that is what the Messages API reads.
///
/// The practical consequence: an upstream that speaks the Anthropic dialect
/// but authenticates with `Authorization: Bearer` (an in-house gateway, say)
/// will reject every call, because the credential arrives in `x-api-key`
/// instead. Reported as a real case in #187.
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
    /// The egress audit lane, written at record when containment is engaged.
    /// ADDITIVE and OMITTED entirely when there is nothing to say, so a flow
    /// that never uses the feature serializes BYTE-IDENTICAL to today. The
    /// allow-list is stored as UNRESOLVED `${VAR}` text (an audit record, not
    /// authority: enforcement always uses the CURRENT spec's set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    egress: Option<EgressTrace>,
}

/// The trace's egress lane: the containment tier the recording ran under, the
/// declared allow-list (unresolved text), and the denied attempts observed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct EgressTrace {
    /// The containment tier line, e.g. `enforced (linux seccomp)`.
    containment: String,
    /// The spec's `allow_egress` entries, UNRESOLVED - an audit record of the
    /// policy in force, never resolved (which would leak the destination).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allowed: Vec<String>,
    /// Undeclared destinations the agent attempted and containment denied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    blocked: Vec<EgressEvent>,
}

/// One MCP server's recorded lane: its mocks, snapshotted (travel-in-trace,
/// like `AgentTrace.mocks`), and the JSON-RPC calls captured in order.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct McpServerTrace {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    mocks: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    calls: Vec<McpCall>,
    /// Server-initiated notifications captured on this lane, re-emitted at
    /// replay. ADDITIVE and skipped when empty, so a v3.1/v3.2 lane with no
    /// `events` key deserializes (empty) and re-serializes byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    events: Vec<McpServerEvent>,
}

/// The MCP boundary set up for one phase, across both transports.
///
/// A stdio server uses the file contract: the per-run temp dir holds the
/// `<server>.plan.json` the stand-in reads and the `<server>.out.json` it
/// writes. An http server has NO plan/out files - flowproof hosts an
/// in-process [`McpHttpServer`] and reads its verdict from shared memory
/// after the agent exits. Both inject one env var pointing the agent's MCP
/// config at flowproof instead of the real server.
///
/// `None` when the spec declares no `mcp:` servers, so an mcp-less flow is
/// byte-for-byte the flow it always was.
struct McpContext {
    dir: PathBuf,
    mode: &'static str,
    /// stdio servers: (server name, `FLOWPROOF_MCP_SERVER_<NAME>`).
    stdio: Vec<(String, String)>,
    /// http servers: (server name, `FLOWPROOF_MCP_URL_<NAME>`, the live
    /// in-process listener whose captured/served/divergence is read after
    /// the run).
    http: Vec<(String, String, McpHttpServer)>,
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

        let mut stdio = Vec::new();
        let mut http = Vec::new();
        for server in &spec.mcp {
            // MCP mocks are the per-server tools with a non-null result -
            // the same "a name-only entry is a declaration" rule the model
            // boundary uses.
            let mocks = server_mocks(server);
            let (calls, events) = if mode == "replay" {
                trace_mcp
                    .get(&server.name)
                    .map(|t| (t.calls.clone(), t.events.clone()))
                    .unwrap_or_default()
            } else {
                (Vec::new(), Vec::new())
            };
            // Exactly one transport (the spec validator guaranteed the
            // choice): a stdio `command:` flowproof stands in as, or a
            // streamable-HTTP `url:` flowproof hosts a listener for.
            match (&server.command, &server.url) {
                (Some(command), _) => {
                    let command = flowproof_trace::secret::resolve_refs(command)
                        .map_err(|e| e.to_string())?;
                    let plan = McpPlan {
                        mode: mode.to_string(),
                        command,
                        mocks,
                        calls,
                        events,
                    };
                    let plan_path = dir.join(format!("{}.plan.json", server.name));
                    std::fs::write(
                        &plan_path,
                        serde_json::to_string(&plan).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| format!("writing {}: {e}", plan_path.display()))?;
                    stdio.push((server.name.clone(), stdio_env_var_name(&server.name)));
                }
                (None, Some(url)) => {
                    // Ephemeral (0) by default; a fixed `port:` for a flow
                    // whose agent cannot be handed the port at launch.
                    let port = server.port.unwrap_or(0);
                    let listener = if mode == "replay" {
                        McpHttpServer::replay(calls, events, mocks, port)
                    } else {
                        let url = flowproof_trace::secret::resolve_refs(url)
                            .map_err(|e| e.to_string())?;
                        McpHttpServer::record(&url, mocks, port)
                    }
                    .map_err(|e| {
                        format!("starting the MCP HTTP listener for `{}`: {e}", server.name)
                    })?;
                    http.push((
                        server.name.clone(),
                        url_env_var_name(&server.name),
                        listener,
                    ));
                }
                // The validator rejects a server naming neither transport,
                // so this is unreachable in practice; fail loudly rather
                // than skip it silently if that ever changes.
                (None, None) => {
                    return Err(format!(
                        "mcp server `{}` names no transport (should have been rejected at parse)",
                        server.name
                    ));
                }
            }
        }
        Ok(Some(McpContext {
            dir,
            mode,
            stdio,
            http,
        }))
    }

    /// The env the agent's process needs: the run dir and mode (for the
    /// stdio stand-in), one `FLOWPROOF_MCP_SERVER_<NAME>` per stdio server
    /// pointing at this executable's `mcp-stdio` subcommand, and one
    /// `FLOWPROOF_MCP_URL_<NAME>` per http server pointing at its in-process
    /// listener. The SUT's MCP config points each server at its variable -
    /// the documented contract.
    fn env_vars(&self) -> Result<BTreeMap<String, String>, String> {
        let mut env = BTreeMap::new();
        env.insert(
            "FLOWPROOF_MCP_DIR".to_string(),
            self.dir.display().to_string(),
        );
        env.insert("FLOWPROOF_MCP_MODE".to_string(), self.mode.to_string());

        if !self.stdio.is_empty() {
            // `current_exe()` is the flowproof binary in production. An
            // explicit `FLOWPROOF_MCP_EXE` overrides it for the cases where
            // it is not: a wrapper on PATH, or a test harness driving
            // `run_cli` in-process (where `current_exe()` is the test binary,
            // not flowproof). Only the stdio transport spawns a subprocess,
            // so an http-only flow never needs the exe.
            let exe = match std::env::var("FLOWPROOF_MCP_EXE") {
                Ok(path) if !path.trim().is_empty() => path,
                _ => std::env::current_exe()
                    .map_err(|e| format!("finding the flowproof executable for mcp-stdio: {e}"))?
                    .display()
                    .to_string(),
            };
            for (name, var) in &self.stdio {
                // The exe path is quoted so an argv splitter survives spaces
                // in it; the agent spawns this command as its MCP server.
                env.insert(var.clone(), format!("\"{exe}\" mcp-stdio --server {name}"));
            }
        }

        for (_name, var, listener) in &self.http {
            // The listener's own endpoint URL, `http://127.0.0.1:<port>/mcp`.
            env.insert(var.clone(), listener.url());
        }
        Ok(env)
    }

    /// After a RECORD run: read each server's out file, enforce the progress
    /// guard (a declared server the agent never contacted), and fold the
    /// captured calls plus the snapshotted mocks into the trace.
    fn collect_record(&self, spec: &FlowSpec) -> Result<BTreeMap<String, McpServerTrace>, String> {
        let mut out = BTreeMap::new();
        // stdio servers: read the out file the stand-in wrote.
        for (name, _var) in &self.stdio {
            let out_path = self.dir.join(format!("{name}.out.json"));
            let parsed =
                read_out(&out_path).ok_or_else(|| stdio_missing_out_guard(name, &out_path))?;
            if let Some(err) = &parsed.error {
                return Err(format!("recording the MCP server `{name}` failed: {err}"));
            }
            // A real MCP client handshakes with `initialize`, so an empty
            // lane means something went wrong AFTER the stand-in ran - a
            // different fact from the stand-in never running at all.
            if parsed.calls.is_empty() {
                return Err(stdio_empty_lane_guard(name, &out_path));
            }
            out.insert(
                name.clone(),
                McpServerTrace {
                    mocks: server_mocks_by_name(spec, name),
                    calls: parsed.calls,
                    events: parsed.events,
                },
            );
        }
        // http servers: read the captured lane straight from the live
        // in-process listener (no out file).
        for (name, _var, listener) in &self.http {
            // A named record failure (a server-initiated request mid-response)
            // is surfaced with its own message, not swallowed.
            if let Some(err) = &listener.log().record_error {
                return Err(format!("recording the MCP server `{name}` failed: {err}"));
            }
            let calls = listener.captured();
            if calls.is_empty() {
                return Err(http_wiring_guard(name));
            }
            out.insert(
                name.clone(),
                McpServerTrace {
                    mocks: server_mocks_by_name(spec, name),
                    calls,
                    events: listener.events(),
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
        _spec: &FlowSpec,
        trace_mcp: &BTreeMap<String, McpServerTrace>,
    ) -> Result<(), String> {
        let expected = |name: &str| trace_mcp.get(name).map(|t| t.calls.len()).unwrap_or(0);
        // stdio servers: read the out file the stand-in wrote.
        for (name, _var) in &self.stdio {
            let out_path = self.dir.join(format!("{name}.out.json"));
            let parsed =
                read_out(&out_path).ok_or_else(|| stdio_missing_out_guard(name, &out_path))?;
            if let Some(div) = &parsed.divergence {
                return Err(format!(
                    "the MCP server `{name}` diverged at call {}: {}",
                    div.index + 1,
                    div.detail
                ));
            }
            let served = parsed.served.unwrap_or(0);
            if served != expected(name) {
                return Err(format!(
                    "the agent made {served} MCP calls to `{name}`, the recording has {}",
                    expected(name)
                ));
            }
        }
        // http servers: read the verdict straight from the live listener.
        for (name, _var, listener) in &self.http {
            let log = listener.log();
            if let Some(div) = &log.divergence {
                return Err(format!(
                    "the MCP server `{name}` diverged at call {}: {}",
                    div.index + 1,
                    div.detail
                ));
            }
            if log.served != expected(name) {
                return Err(format!(
                    "the agent made {} MCP calls to `{name}`, the recording has {}",
                    log.served,
                    expected(name)
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

/// The stdio record-time guard when NO out file appeared: the stand-in
/// either never ran or died before writing.
///
/// This used to be shared with the empty-lane case below and asserted a
/// single cause - "its config still points at the real server" - as fact.
/// An adopter proved that wrong the expensive way: their agent HAD spawned
/// the stand-in (verified by the real server logging its parent process as
/// `flowproof mcp-stdio --server files`), calls went through it, and the
/// message still sent them to debug wiring that was already correct. So
/// these now say what was OBSERVED and offer causes as possibilities.
fn stdio_missing_out_guard(server: &str, out_path: &Path) -> String {
    format!(
        "no recording appeared for the MCP server `{server}`: flowproof's stand-in wrote \
         nothing to {}.\n\
         Most likely its config still points at the real server, so the stand-in never ran \
         (point it at ${{FLOWPROOF_MCP_SERVER_{}}}). If the stand-in DID run, it was killed \
         before it could write - check whether the agent terminates its MCP servers abruptly.",
        out_path.display(),
        env_suffix(server)
    )
}

/// The stdio record-time guard when the out file EXISTS but is empty: the
/// stand-in ran, so the wiring is right, and something else is wrong.
/// Saying "you never spawned it" here is simply false.
fn stdio_empty_lane_guard(server: &str, out_path: &Path) -> String {
    format!(
        "flowproof's MCP stand-in for `{server}` ran and wrote {}, but captured no calls - \
         not even the `initialize` handshake every MCP client sends.\n\
         The wiring is therefore NOT the obvious suspect: the stand-in was reached. Either \
         the agent connected and disconnected without handshaking, or it spawned a SECOND \
         copy for the calls it actually made. Recording stops here because an empty lane \
         would replay as a boundary that proves nothing.",
        out_path.display()
    )
}

/// The http record-time wiring guard: an empty captured lane means the
/// agent never dialed flowproof's in-process listener.
fn http_wiring_guard(server: &str) -> String {
    format!(
        "the agent never contacted flowproof's MCP listener for `{server}`; its config still \
         points at the real server (point it at ${{FLOWPROOF_MCP_URL_{}}})",
        env_suffix(server)
    )
}

/// The per-server mocks: tools with a non-null `result:`. The same
/// "a name-only entry is a declaration" rule the model boundary uses.
fn server_mocks(server: &flowproof_agent::McpServerSpec) -> BTreeMap<String, serde_json::Value> {
    server
        .tools
        .iter()
        .filter(|t| !t.result.is_null())
        .map(|t| (t.name.clone(), t.result.clone()))
        .collect()
}

/// The mocks for the server named `name` in the spec, snapshotted into the
/// trace. Absent name yields empty (the caller only asks for a declared one).
fn server_mocks_by_name(spec: &FlowSpec, name: &str) -> BTreeMap<String, serde_json::Value> {
    spec.mcp
        .iter()
        .find(|s| s.name == name)
        .map(server_mocks)
        .unwrap_or_default()
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

/// The full `FLOWPROOF_MCP_SERVER_<NAME>` env var name for a stdio server.
fn stdio_env_var_name(server: &str) -> String {
    format!("FLOWPROOF_MCP_SERVER_{}", env_suffix(server))
}

/// The full `FLOWPROOF_MCP_URL_<NAME>` env var name for an http server.
fn url_env_var_name(server: &str) -> String {
    format!("FLOWPROOF_MCP_URL_{}", env_suffix(server))
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
    /// The resolved egress allow-set enforced on a `command:` driver (empty
    /// and unused for `url:`).
    allow: AllowSet,
    /// The spec's `allow_egress` entries, kept UNRESOLVED for the trace's
    /// audit lane.
    allow_unresolved: Vec<String>,
    /// Whether the flow carries an `assert_no_egress` step.
    assert_no_egress: bool,
    /// Whether this flow ENGAGES egress containment: it declares an
    /// `allow_egress` set or asserts no egress. The PURE predicate (see
    /// [`engages_egress`]) that gates whether the command driver runs
    /// contained - identical in record and replay, so both install (or skip)
    /// the seccomp filter the same way.
    engages_egress: bool,
    /// The `assert_no_secret_leak` steps, each naming one or more `${VAR}`
    /// selectors and its 1-based position in the flow's `steps:` (for the
    /// failure message). Only the variable NAMES travel here - never a value.
    /// The shared type web and api flows also build their scan from.
    secret_leaks: Vec<flowproof_trace::secret_scan::LeakAssertion>,
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

    /// Trigger the system under test against an already-started proxy. A
    /// `command:` driver runs CONTAINED only when the flow ENGAGES egress
    /// (`allow_egress` or `assert_no_egress`); this is opt-in, so the common
    /// path installs no seccomp filter. The gating predicate is pure and
    /// identical in record and replay, so both make the same choice. A `url:`
    /// service flowproof did not start is never contained.
    fn drive(&self, proxy: &AgentProxy) -> Result<AgentRun, String> {
        match &self.driver {
            Driver::Command(command) if self.engages_egress => {
                run_against_contained(proxy, command, &self.env, AGENT_TIMEOUT, &self.allow)
                    .map_err(|e| e.to_string())
            }
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

    // The egress allow-set: the CURRENT spec's `allow_egress`, `${VAR}` refs
    // resolved at execution and DNS names pinned to their IP set once. The
    // UNRESOLVED text is kept for the trace's audit lane. `url:` flows reject
    // `allow_egress` at validation, so this is only ever non-empty for a
    // `command:` driver.
    let allow_unresolved = spec
        .agent
        .as_ref()
        .map(|a| a.allow_egress.clone())
        .unwrap_or_default();
    let mut resolved_allow = Vec::new();
    for entry in &allow_unresolved {
        resolved_allow
            .push(flowproof_trace::secret::resolve_refs(entry).map_err(|e| e.to_string())?);
    }
    let allow = AllowSet::resolve(&resolved_allow)?;
    let assert_no_egress = spec
        .steps
        .iter()
        .any(|s| matches!(s, SpecStep::AssertNoEgress));

    let mut tool_calls = Vec::new();
    let mut forbidden = Vec::new();
    let mut reply_contains = Vec::new();
    // The secret-leak assertions come from the shared spec accessor, the same
    // source web and api flows build their scan from.
    let secret_leaks = spec.secret_leak_assertions();
    for step in spec.steps.iter() {
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
        allow,
        allow_unresolved,
        assert_no_egress,
        engages_egress: engages_egress(spec),
        secret_leaks,
    })
}

/// Whether a flow ENGAGES egress containment: it declares an `allow_egress`
/// set OR carries an `assert_no_egress` step. A PURE function of the spec, so
/// record and replay make the identical containment choice (a determinism
/// requirement) and so the SAME predicate drives both the containment decision
/// in [`Plan::drive`] and the tier reported by [`containment`]. When this is
/// false, no seccomp filter is installed and no containment tier is claimed;
/// `assert_no_egress` forces it true, which is exactly the certification.
pub fn engages_egress(spec: &FlowSpec) -> bool {
    let declares_allow = spec
        .agent
        .as_ref()
        .is_some_and(|a| !a.allow_egress.is_empty());
    let asserts_no_egress = spec
        .steps
        .iter()
        .any(|s| matches!(s, SpecStep::AssertNoEgress));
    declares_allow || asserts_no_egress
}

/// The egress destinations containment DENIED during the recorded run, read
/// from the trace's egress lane as value-free `destination (protocol)`
/// descriptors. Empty for a flow with no egress lane, an unreadable trace, or
/// a run that attempted no undeclared egress - so it is safe to call on any
/// agent trace. Feeds the run record's `evidence.blocked`.
pub fn egress_blocked(trace_path: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(trace_path) else {
        return Vec::new();
    };
    let Ok(trace) = serde_json::from_str::<AgentTrace>(&raw) else {
        return Vec::new();
    };
    trace
        .egress
        .map(|e| {
            e.blocked
                .iter()
                .map(|ev| format!("{} ({})", ev.destination, ev.protocol))
                .collect()
        })
        .unwrap_or_default()
}

/// The containment tier a flow's run achieves, on this platform. Pure and
/// cheap (a kernel probe on Linux); computed both here, to print the report's
/// tier line, and inside record/replay, to gate `assert_no_egress` and build
/// the trace's egress lane. Containment is OPT-IN: a flow that does not engage
/// egress installs no filter and claims no tier.
pub fn containment(spec: &FlowSpec) -> Containment {
    if !engages_egress(spec) {
        return Containment::not_engaged();
    }
    match spec.agent.as_ref() {
        // A service flowproof did not start cannot be contained.
        Some(agent) if agent.url.is_some() => Containment::url_flow(),
        // A process flowproof starts: what this platform/kernel can enforce.
        _ => Containment::command_flow(),
    }
}

/// Check the run's egress against the flow's policy. Returns the trace's
/// egress lane to store (record) or discard (replay). Fails - so record mints
/// no trace and replay fails the flow - when `assert_no_egress` cannot be
/// certified or was violated.
fn check_egress(
    plan: &Plan,
    run: &AgentRun,
    containment: &Containment,
) -> Result<Option<EgressTrace>, String> {
    // `assert_no_egress` is a CAPABILITY claim: it can only certify where
    // containment is actually enforced. There is no bypass flag.
    if plan.assert_no_egress && !containment.is_enforced() {
        return Err(format!(
            "egress containment is not enforced on this platform/driver ({}); \
             assert_no_egress cannot certify",
            containment.reason().unwrap_or("not contained")
        ));
    }
    // A supervisor FAULT means the filter was installed and then could not do
    // its job: `process_vm_readv` or `pidfd_getfd` was refused, so trapped
    // syscalls were denied without ever being adjudicated. Nothing reached the
    // network, but nothing was observed either - so the emptiness of
    // `blocked` below proves nothing, and certifying on it would be a false
    // green of exactly the kind CHARTER §5 ranks first.
    //
    // Checked BEFORE the set predicate, because the set predicate is the
    // thing a fault invalidates.
    let faults = run.egress.distinct_faults();
    if plan.assert_no_egress && !faults.is_empty() {
        return Err(format!(
            "egress containment was installed but could not adjudicate {} trapped \
             syscall(s) ({}); the run is not contained and assert_no_egress cannot \
             certify",
            run.egress.faults.len(),
            faults.join("; ")
        ));
    }

    // The verdict is the SET predicate: the set of undeclared destinations
    // attempted is empty (deduped by destination, so retry-count variance is
    // irrelevant).
    let undeclared = run.egress.undeclared_destinations();
    if plan.assert_no_egress && !undeclared.is_empty() {
        return Err(egress_failure_message(&undeclared));
    }

    // The audit lane is written only when the feature is engaged, so a flow
    // that never touches egress serializes byte-identical to today. This is
    // the SAME pure predicate that gated containment (`plan.engages_egress`);
    // `run.egress.blocked` can only be non-empty when it was already true, so
    // the two decisions never disagree.
    let engaged = plan.engages_egress || !run.egress.blocked.is_empty();
    if !engaged {
        return Ok(None);
    }
    Ok(Some(EgressTrace {
        containment: containment_tag(containment),
        allowed: plan.allow_unresolved.clone(),
        blocked: run.egress.blocked.clone(),
    }))
}

/// The warning a run earns by declaring an allow-list nothing will enforce.
///
/// `allow_egress` without `assert_no_egress` is the quiet case: the flow says
/// which destinations the agent may reach, the platform cannot enforce that,
/// and the run passes anyway having reached whatever it liked. Nothing here is
/// a bug - `assert_no_egress` is the certification claim and this flow makes
/// none - but declaring an allow-list IS a statement of intent, and silently
/// not enforcing it is the gap most likely to be discovered late.
///
/// So it warns rather than failing: turning it into an error would break every
/// existing macOS and Windows flow that passes today, and the flow never
/// claimed certification. The tier line is already printed on every agent run,
/// but a tier line on a green build is not something anyone reads.
///
/// Returns `None` when there is nothing to say: containment is enforced, the
/// flow declares no allow-list, or it carries `assert_no_egress` and has
/// therefore already been failed outright by [`check_egress`].
fn egress_warning(plan: &Plan, containment: &Containment) -> Option<String> {
    if containment.is_enforced() || plan.assert_no_egress || plan.allow_unresolved.is_empty() {
        return None;
    }
    Some(format!(
        "warning: this flow declares allow_egress ({}) but egress containment is \
         not enforced here ({}), so the allow-list was NOT applied and the agent \
         could reach any destination. Add assert_no_egress to make this a hard \
         failure instead of a warning.",
        plan.allow_unresolved.join(", "),
        containment.reason().unwrap_or("not contained"),
    ))
}

/// The short containment tag stored in the trace lane (the parenthetical of
/// the report line): `enforced (linux seccomp)` or `not contained (<reason>)`.
/// The run record stores the SAME string, so the trace and the artifact an
/// auditor reads cannot describe the run differently.
pub fn containment_tag(containment: &Containment) -> String {
    containment
        .report_line()
        .strip_prefix("egress containment: ")
        .unwrap_or("")
        .to_string()
}

/// The failure naming the undeclared destinations, e.g.
/// `undeclared egress attempted: 198.51.100.9:443 (tcp), 203.0.113.9:53 (udp);
/// declare it in agent.allow_egress or remove it from the agent`.
fn egress_failure_message(undeclared: &[EgressEvent]) -> String {
    let list = undeclared
        .iter()
        .map(|e| format!("{} ({})", e.destination, e.protocol))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "undeclared egress attempted: {list}; declare it in agent.allow_egress or remove it \
         from the agent"
    )
}

/// The secret-leak corpus for an `app: agent` flow: the model-boundary
/// trajectory (the cassette's request + response bodies) plus each MCP lane,
/// each as a NAMED element so a failure can say WHERE a secret appeared. A
/// closed corpus, not "everything": the audit output lists exactly these so
/// nobody mistakes it for a proof about channels the engine never saw.
fn secret_corpus(
    cassette: &Cassette,
    mcp: &BTreeMap<String, McpServerTrace>,
) -> Vec<(String, String)> {
    let mut corpus = Vec::new();
    // The cassette element is always present on an agent flow (a run with an
    // empty trajectory fails the progress guard long before this), so the
    // corpus is never empty and the empty-corpus capability error cannot
    // arise here.
    corpus.push((
        "the model-boundary trajectory".to_string(),
        serde_json::to_string(cassette).unwrap_or_default(),
    ));
    for (name, lane) in mcp {
        corpus.push((
            format!("the `{name}` MCP lane"),
            serde_json::to_string(lane).unwrap_or_default(),
        ));
    }
    corpus
}

/// Scan the run's captured corpus for any declared secret, by the SAME shared
/// mechanism web, api, and replay use, so an unchanged system yields the same
/// verdict. Builds the agent corpus (model-boundary trajectory + MCP lanes)
/// and delegates the resolve/refuse/substring-scan to
/// [`flowproof_trace::secret_scan::scan_corpus`]; only variable NAMES are ever
/// stored or printed.
fn check_secret_leak(
    plan: &Plan,
    cassette: &Cassette,
    mcp: &BTreeMap<String, McpServerTrace>,
) -> Result<(), String> {
    if plan.secret_leaks.is_empty() {
        return Ok(());
    }
    let corpus = secret_corpus(cassette, mcp);
    flowproof_trace::secret_scan::scan_corpus(&plan.secret_leaks, &corpus)
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
        // This is the failure a determinism tool must never let pass, so it
        // is worth spending words on. An agent that reached the real
        // provider instead of the proxy produces a green-looking run that
        // proves nothing - except that recording refuses to mint a trace,
        // which is what keeps it honest. Naming the likely cause matters
        // because the usual one is invisible: a client whose base URL comes
        // from a config object or a custom variable never sees the standard
        // ones flowproof injects.
        let custom_env = plan
            .env
            .keys()
            .filter(|k| {
                !k.starts_with("FLOWPROOF_") && !k.contains("OPENAI") && !k.contains("ANTHROPIC")
            })
            .cloned()
            .collect::<Vec<_>>();
        let mapped = if custom_env.is_empty() {
            "This flow maps no custom variables. If your client reads one \
             (an AI gateway URL, a config object it builds itself), map it in \
             `agent.env` with `${flowproof.proxy_url}` or \
             `${flowproof.proxy_url_no_v1}`."
                .to_string()
        } else {
            format!(
                "This flow maps {}. Check the value actually reaches the process \
                 that makes the model call - a client that spawns a child may not \
                 pass it down.",
                custom_env.join(", ")
            )
        };
        let message = format!(
            "0 model requests reached the proxy, so there is nothing to record and \
             no trace was written.\n\
             The agent exited {} without talking to the proxy, which usually means \
             it called the real provider instead.\n\
             {mapped}\n\
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

/// `flowproof doctor`: does this agent's model traffic reach the proxy?
///
/// The dominant adoption failure is a client that never honours the injected
/// base URL - it talks to the real provider, flowproof captures nothing, and
/// today that is discovered only after a spec has been written and a key
/// spent on `record`. This answers the same question in ten seconds, with no
/// spec, no assertions and NO KEY: a synthetic one-turn cassette answers
/// whatever arrives.
///
/// It deliberately does NOT print a verdict like "your wiring is correct".
/// An agent with two clients can reach the proxy with one and the real
/// provider with the other, so a request arriving proves that A client found
/// the proxy - not that all of them did. Reporting the observation and
/// letting the reader judge is the honest shape, and the same reason the
/// zero-capture guard names causes as possibilities rather than facts.
pub fn cmd_doctor(command: &str, timeout_secs: u64, prompt: &str) -> Result<u8, String> {
    use flowproof_trace::cassette::{Cassette, Message, Turn, TurnRequest, TurnResponse};

    // One canned turn. Anything the agent sends is answered from this, so no
    // upstream and no key are involved. A request that DIVERGES from it still
    // arrived, which is the fact being measured.
    let probe = Cassette {
        turns: vec![Turn {
            protocol: "openai".to_string(),
            request: TurnRequest {
                model: "flowproof-doctor".to_string(),
                messages: Vec::new(),
                tools: Vec::new(),
            },
            response: TurnResponse {
                message: Message {
                    role: "assistant".to_string(),
                    content: Some(
                        "flowproof doctor: this reply came from the proxy, not a model."
                            .to_string(),
                    ),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                },
                stop_reason: None,
            },
        }],
    };

    let proxy = AgentProxy::start(probe, Mocks::new(), 0)
        .map_err(|e| format!("starting the probe proxy: {e}"))?;
    let base = proxy.base_url();
    println!("proxy listening on {base}");
    println!("running: {command}");

    let mut env = BTreeMap::new();
    env.insert(PROMPT_VAR.to_string(), prompt.to_string());
    let run = run_against(&proxy, command, &env, Duration::from_secs(timeout_secs))
        .map_err(|e| e.to_string())?;
    let log = proxy.log();
    // A divergence means the request did not match the canned turn, which is
    // expected and irrelevant: it still reached the proxy.
    let arrived = log.served + usize::from(log.divergence.is_some());
    drop(log);

    println!();
    println!("model requests that reached the proxy: {arrived}");
    if run.timed_out {
        println!(
            "the agent was still running after {timeout_secs}s and was stopped. That is an \
             observation, not a wiring failure: an agent waiting for a useful reply will hang \
             against a canned one."
        );
        // Found while testing this command: the deadline kills the process
        // flowproof started, but a GRANDCHILD holding the inherited stdout
        // pipe keeps the read blocking, so the wall-clock wait can exceed
        // the timeout by a lot. Say so rather than let it look like a hang
        // with no explanation.
        println!(
            "  (if this took much longer than {timeout_secs}s, the agent spawned a child that \
             outlived it and kept the output pipe open. flowproof stops the process it started, \
             not the tree.)"
        );
    } else {
        println!(
            "the agent exited {}",
            run.exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "with no code".into())
        );
    }

    if arrived == 0 {
        println!();
        println!("NOTHING reached the proxy. The client is not honouring the base URL flowproof");
        println!("injected (OPENAI_BASE_URL, OPENAI_API_BASE, OPENAI_BASE, ANTHROPIC_BASE_URL,");
        println!("FLOWPROOF_LLM_PROXY). If it reads a different variable, or builds its base URL");
        println!("from a config object, map it in `agent.env`:");
        println!();
        println!("    env:");
        println!("      YOUR_VARIABLE: \"${{flowproof.proxy_url}}\"        # includes /v1");
        println!(
            "      OR:            \"${{flowproof.proxy_url_no_v1}}\"  # client appends its own"
        );
        println!();
        println!("If the model call happens in a CHILD process, check that it inherits the");
        println!("environment - that is the usual cause when the command itself looks right.");
        if !run.stderr.trim().is_empty() {
            println!();
            println!("stderr:\n{}", run.stderr.trim());
        }
        return Ok(crate::EXIT_FAIL);
    }

    println!();
    println!("At least one client reached the proxy, so recording can capture that traffic.");
    println!("This does NOT prove every model call goes through flowproof: an agent with more");
    println!("than one client can reach the proxy with one and the real provider with another.");
    println!("`record` is the check that settles it - it fails and writes no trace if nothing");
    println!("is captured.");
    Ok(crate::EXIT_PASS)
}

/// Record an `app: agent` flow: run it against a real model, capture the
/// trajectory, check the assertions, and write the cassette to `out`.
/// Warn when a flow reasons about a tool that nothing actually intercepts.
///
/// flowproof sits at the MODEL boundary: a `tools:` mock rewrites what the
/// model is told a tool returned, but the system under test ran that tool
/// itself, for real. So `assert_no_tool_call` proves the agent did not ASK
/// for the tool - it does not stop one from firing. The docs have always
/// said so; this says it at the moment it can actually cost something.
///
/// The replay half is the part prose never covered: replay serves the
/// RECORDED assistant message, tool calls included, to a live agent process.
/// A tool that fired once during record therefore fires again on every CI
/// run. That is the sentence worth printing.
///
/// A tool declared under `mcp:` with a `result:` IS intercepted - flowproof
/// answers it and the real server never sees it - so those are silent. The
/// remedy named here is that boundary, not a mute switch: a warning you can
/// turn off without changing what runs is worth nothing.
/// Does `asserted` name the same tool as `intercepted`, allowing for the
/// namespace an MCP client may prefix?
///
/// An MCP client routinely namespaces a server's tools before offering them to
/// the model, so a tool intercepted under `mcp:` as `delete_all` can reach the
/// model boundary as `files__delete_all`. Comparing the two names literally then
/// warns that a CORRECTLY intercepted tool is unprotected - a false positive
/// landing on exactly the flows that did the right thing, and one that this
/// warning cannot afford, because an adopter who learns to ignore it stops
/// reading the true ones.
///
/// Only a namespace SEPARATOR counts, never a bare substring: `soft_delete_all`
/// does not match `delete_all`, so a genuinely unprotected tool is still named.
fn names_same_tool(asserted: &str, intercepted: &str) -> bool {
    if asserted == intercepted {
        return true;
    }
    ["__", ".", "/", ":"].iter().any(|sep| {
        asserted
            .strip_suffix(intercepted)
            .and_then(|head| head.strip_suffix(*sep))
            // A separator with nothing before it is not a namespace.
            .is_some_and(|prefix| !prefix.is_empty())
    })
}

fn unprotected_tool_warning(spec: &FlowSpec, plan: &Plan, phase: &str) -> Option<String> {
    // Everything the MCP boundary answers on the flow's behalf.
    let intercepted: std::collections::BTreeSet<&str> = spec
        .mcp
        .iter()
        .flat_map(|server| server.tools.iter())
        .filter(|t| !t.result.is_null())
        .map(|t| t.name.as_str())
        .collect();

    // Tools the flow reasons about at the MODEL boundary: a substitution
    // (the model is lied to, the real tool still ran) or a forbid assertion
    // (proved not-asked-for, not prevented).
    let mut exposed: Vec<&str> = spec
        .tools
        .iter()
        .filter(|t| !t.result.is_null())
        .map(|t| t.name.as_str())
        .chain(plan.forbidden.iter().map(|f| f.tool.as_str()))
        .filter(|name| !intercepted.iter().any(|t| names_same_tool(name, t)))
        .collect();
    exposed.sort_unstable();
    exposed.dedup();
    if exposed.is_empty() {
        return None;
    }

    let names = exposed.join(", ");
    let consequence = if phase == "replay" {
        "replay serves the RECORDED tool calls to a live agent, so any side effect fires \
         again on every run"
    } else {
        "the agent executes its own tools for real during record, so any side effect fires"
    };
    Some(format!(
        "warning: {names} is asserted or mocked at the MODEL boundary, which does not \
         intercept tool execution - {consequence}.\n         \
         For a tool with real side effects, declare it under `mcp:` with a `result:` \
         (flowproof answers it and the real server never runs it), or stub/sandbox it \
         author-side. See \
         https://github.com/automators-com/flowproof/blob/main/docs/agent-testing.md."
    ))
}

/// Print the warning above, if this flow earns one. Stderr, so `--json`
/// keeps its stdout contract.
fn warn_unprotected_tools(spec: &FlowSpec, plan: &Plan, phase: &str) {
    if let Some(warning) = unprotected_tool_warning(spec, plan, phase) {
        eprintln!("{warning}");
    }
}

pub fn record(spec: &FlowSpec, out: &Path) -> Result<(), String> {
    let mut plan = plan(spec)?;
    warn_unprotected_tools(spec, &plan, "record");
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
    // The agent has exited, which is not the same as the proxy being done:
    // a call the agent fired without waiting for is still in flight, and
    // reading the cassette now would drop it whenever the upstream is slow.
    // See AgentProxy::quiesce.
    proxy.quiesce(
        std::time::Duration::from_millis(300),
        std::time::Duration::from_secs(15),
    );
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
    // Egress asserts too: a failing `assert_no_egress` (or a blocked
    // undeclared attempt when the step is present) mints NO trace, beside the
    // trajectory assertions. The returned lane is the audit record written
    // into the trace.
    let tier = containment(spec);
    if let Some(warning) = egress_warning(&plan, &tier) {
        eprintln!("{warning}");
    }
    let egress = check_egress(&plan, &run, &tier)?;
    // The secret-leak scan runs BEFORE the trace is minted: a leak fails the
    // run so NO trace is written. That doubles as a store-guard - a secret
    // leaked into a cassette body never reaches disk.
    check_secret_leak(&plan, &cassette, &mcp_trace)?;

    let trace = AgentTrace {
        app: "agent".into(),
        mocks: plan.mocks.into_iter().collect(),
        cassette,
        mcp: mcp_trace,
        egress,
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
    warn_unprotected_tools(spec, &plan, "replay");
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
    // Egress is checked against THIS phase's LIVE log, not the recorded lane:
    // enforcement uses the current spec's set both phases, and the trace's
    // lane is an audit record only.
    let tier = containment(spec);
    if let Some(warning) = egress_warning(&plan, &tier) {
        eprintln!("{warning}");
    }
    check_egress(&plan, &run, &tier)?;
    // Re-scan the recorded corpus for declared secrets by the SAME mechanism
    // as record, so an unchanged system replays the same verdict. The corpus
    // is the recorded cassette + MCP lanes (the proxy consumed the live one).
    check_secret_leak(&plan, &trace.cassette, &trace.mcp)?;
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
    /// An MCP client namespaces a server's tools, so an intercepted
    /// `delete_all` reaches the model boundary as `files__delete_all`. The
    /// unprotected-tool warning must recognise it as the SAME tool, or it fires
    /// on precisely the flows that did the right thing.
    #[test]
    fn a_namespaced_tool_is_the_same_tool() {
        for asserted in [
            "delete_all",
            "files__delete_all",
            "files.delete_all",
            "files/delete_all",
            "files:delete_all",
        ] {
            assert!(
                super::names_same_tool(asserted, "delete_all"),
                "{asserted} names the intercepted delete_all"
            );
        }
    }

    /// ...but a bare substring is NOT a namespace. A genuinely unprotected tool
    /// must still be named, or the fix would silence the warning it exists for.
    #[test]
    fn a_substring_is_not_a_namespace() {
        for asserted in [
            "soft_delete_all",
            "predelete_all",
            "__delete_all",
            ".delete_all",
            "delete_all_now",
        ] {
            assert!(
                !super::names_same_tool(asserted, "delete_all"),
                "{asserted} must NOT be taken for the intercepted delete_all"
            );
        }
    }

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
    ///
    /// The name carries a nonce as well as the pid. Cargo runs tests as threads
    /// in ONE process, so a pid-only name is shared by every caller: two of them
    /// wrote the same file while a third read it, and the reader got
    /// `EOF while parsing`. That flake failed CI on two unrelated pull requests
    /// before anyone traced it. Same reason `setup` pairs the pid with
    /// `mcp_nonce()` a few hundred lines up.
    fn write_trace(cassette: Cassette) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("flowproof-agent-flow");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("trace-{}-{}.json", std::process::id(), mcp_nonce()));
        let trace = AgentTrace {
            app: "agent".into(),
            mocks: BTreeMap::new(),
            cassette,
            mcp: BTreeMap::new(),
            egress: None,
        };
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&trace).expect("serialize"),
        )
        .expect("write trace");
        path
    }

    /// Concurrent `write_trace` calls must not collide.
    ///
    /// This asserts the property directly rather than hoping a race shows up:
    /// with a pid-only name every thread returns the SAME path, so the
    /// distinctness assertion fails every time instead of one run in twenty.
    /// Running the suite repeatedly proved nothing - eight consecutive green
    /// full-suite runs on a four-core box failed to reproduce the flake that had
    /// already broken CI twice.
    #[test]
    fn concurrent_write_trace_calls_get_their_own_files() {
        use std::sync::{Arc, Barrier};

        const THREADS: usize = 8;
        let barrier = Arc::new(Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    // Start together, to overlap the write as much as possible.
                    barrier.wait();
                    write_trace(neutral_cassette(
                        &format!("prompt {i}"),
                        &format!("reply {i}"),
                    ))
                })
            })
            .collect();

        let paths: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect();

        let unique: std::collections::BTreeSet<_> = paths.iter().collect();
        assert_eq!(
            unique.len(),
            THREADS,
            "every concurrent write_trace needs its own path; got {} distinct of {THREADS}",
            unique.len()
        );

        // And each file must be complete: a shared path leaves a reader parsing
        // a half-written file, which is the `EOF while parsing` the flake showed.
        for path in &paths {
            let text = std::fs::read_to_string(path).expect("trace readable");
            serde_json::from_str::<AgentTrace>(&text)
                .unwrap_or_else(|e| panic!("{} is not a complete trace: {e}", path.display()));
        }
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
            egress: None,
        };
        let json = serde_json::to_string_pretty(&trace).expect("serialize");
        // The `mcp` and `egress` keys are skipped when empty, so the bytes
        // match a pre-feature agent trace exactly.
        assert!(
            !json.contains("mcp"),
            "no mcp key on an mcp-less trace: {json}"
        );
        assert!(
            !json.contains("egress"),
            "no egress key on an egress-less trace: {json}"
        );

        // A hand-built pre-feature trace (no `mcp`/`egress` fields at all)
        // deserializes, its added fields defaulting to empty/None.
        let v2 = r#"{"app":"agent","mocks":{},"cassette":{"turns":[]}}"#;
        let back: AgentTrace = serde_json::from_str(v2).expect("v2 trace still deserializes");
        assert!(back.mcp.is_empty(), "absent mcp defaults to empty");
        assert!(back.egress.is_none(), "absent egress defaults to None");
    }

    /// The egress lane round-trips and is OMITTED entirely when there is
    /// nothing to say - the additivity the byte-identical invariant rests on.
    #[test]
    fn an_egress_lane_survives_the_round_trip() {
        let trace = AgentTrace {
            app: "agent".into(),
            mocks: BTreeMap::new(),
            cassette: neutral_cassette("hi", "there"),
            mcp: BTreeMap::new(),
            egress: Some(EgressTrace {
                containment: "enforced (linux seccomp)".into(),
                allowed: vec!["api.example.com:443".into(), "${SERVICE_HOST}:443".into()],
                blocked: vec![EgressEvent {
                    destination: "198.51.100.9:443".into(),
                    protocol: "tcp".into(),
                    at_ms: 42,
                }],
            }),
        };
        let json = serde_json::to_string(&trace).expect("serialize");
        assert!(json.contains("\"egress\""), "egress present: {json}");
        // The allow-list travels UNRESOLVED - the `${VAR}` ref is not expanded.
        assert!(json.contains("${SERVICE_HOST}:443"), "unresolved: {json}");
        let back: AgentTrace = serde_json::from_str(&json).expect("deserialize");
        let lane = back.egress.expect("lane present");
        assert_eq!(lane.allowed.len(), 2);
        assert_eq!(lane.blocked[0].destination, "198.51.100.9:443");
        assert_eq!(lane.blocked[0].protocol, "tcp");
    }

    // ---- egress containment verdict (cross-platform) ----

    /// A minimal `command:` Plan for exercising `check_egress` directly.
    fn egress_plan(assert_no_egress: bool, allow_unresolved: Vec<String>) -> Plan {
        Plan {
            driver: Driver::Command("agent".into()),
            env: BTreeMap::new(),
            prompt: String::new(),
            mocks: Mocks::new(),
            strict: false,
            tool_calls: Vec::new(),
            forbidden: Vec::new(),
            reply_contains: Vec::new(),
            allow: AllowSet::default(),
            engages_egress: !allow_unresolved.is_empty() || assert_no_egress,
            allow_unresolved,
            assert_no_egress,
            secret_leaks: Vec::new(),
        }
    }

    /// An `AgentRun` carrying `blocked` egress events and nothing else.
    fn egress_run(blocked: Vec<EgressEvent>) -> AgentRun {
        egress_run_with(blocked, Vec::new())
    }

    /// An `AgentRun` carrying both halves of the egress log.
    fn egress_run_with(blocked: Vec<EgressEvent>, faults: Vec<String>) -> AgentRun {
        AgentRun {
            served: 1,
            divergence: None,
            trigger: flowproof_adapters::Trigger::Process,
            exit_code: Some(0),
            timed_out: false,
            stdout: String::new(),
            stderr: String::new(),
            upstream_error: None,
            egress: flowproof_adapters::egress::EgressLog { blocked, faults },
        }
    }

    /// `assert_no_egress` is a CAPABILITY claim: on any tier that is not
    /// enforced it fails outright, with no bypass, rather than passing
    /// vacuously.
    #[test]
    fn assert_no_egress_is_a_capability_error_when_not_enforced() {
        let plan = egress_plan(true, vec![]);
        let run = egress_run(vec![]);
        let err = check_egress(&plan, &run, &Containment::url_flow())
            .expect_err("cannot certify when not enforced");
        assert!(err.contains("assert_no_egress cannot certify"), "{err}");
        assert!(err.contains("not enforced"), "{err}");

        // A non-enforced kernel is the same story.
        let not = Containment::NotContained("kernel lacks seccomp user-notification".into());
        let err = check_egress(&plan, &run, &not).expect_err("cannot certify");
        assert!(err.contains("cannot certify"), "{err}");
    }

    /// A declared allow-list that nothing enforces is the quiet gap: the run
    /// passes, having reached whatever it liked, and only a tier line on a
    /// green build says otherwise. It now says so in words.
    #[test]
    fn a_declared_allow_list_nobody_enforces_warns() {
        let plan = egress_plan(false, vec!["api.acme.internal:443".into()]);
        let not = Containment::NotContained("egress containment is Linux-only".into());

        let warning = egress_warning(&plan, &not).expect("an unenforced allow-list warns");
        // It names what was declared, why it did not apply, and the fix.
        assert!(warning.contains("api.acme.internal:443"), "{warning}");
        assert!(warning.contains("NOT applied"), "{warning}");
        assert!(warning.contains("Linux-only"), "{warning}");
        assert!(warning.contains("assert_no_egress"), "{warning}");

        // And it is only a warning: the run still passes, because this flow
        // never claimed certification.
        let run = egress_run(vec![]);
        check_egress(&plan, &run, &not).expect("a warning is not a failure");
    }

    /// The three cases with nothing to say. A warning that fires when it
    /// should not is the same defect as one that never fires: people stop
    /// reading it.
    #[test]
    fn nothing_to_warn_about_stays_quiet() {
        let not = Containment::NotContained("egress containment is Linux-only".into());

        // Enforced: the allow-list was applied, so there is no gap.
        let declared = egress_plan(false, vec!["api.acme.internal:443".into()]);
        assert!(egress_warning(&declared, &Containment::Enforced).is_none());

        // No allow-list declared: nothing was promised.
        let bare = egress_plan(false, vec![]);
        assert!(egress_warning(&bare, &not).is_none());

        // assert_no_egress present: check_egress already fails this outright,
        // so a warning beside a hard error would only add noise.
        let asserting = egress_plan(true, vec!["api.acme.internal:443".into()]);
        assert!(egress_warning(&asserting, &not).is_none());
        check_egress(&asserting, &egress_run(vec![]), &not).expect_err("still a capability error");
    }

    /// The false green from #300, pinned.
    ///
    /// A supervisor that cannot read child memory denies every trapped
    /// syscall and records NOTHING, so `blocked` is empty and the old
    /// emptiness test passed while the run was never actually adjudicated.
    /// The tier still says `Enforced`, because the filter really was
    /// installed - that is what made it silent.
    ///
    /// The predicate is no longer "we saw nothing bad"; it is "we saw
    /// nothing bad AND we were able to look".
    #[test]
    fn a_faulted_supervisor_cannot_certify_even_with_an_empty_blocked_list() {
        let plan = egress_plan(true, vec![]);
        let run = egress_run_with(
            vec![],
            vec!["connect: process_vm_readv: Operation not permitted".into()],
        );

        // The tier is Enforced: the filter installed fine, then went blind.
        let err = check_egress(&plan, &run, &Containment::Enforced)
            .expect_err("a blind supervisor must not certify");

        assert!(err.contains("cannot certify"), "{err}");
        assert!(err.contains("could not adjudicate"), "{err}");
        // The operator learns WHICH mechanism was refused, not just that
        // something went wrong.
        assert!(err.contains("process_vm_readv"), "{err}");
        // And it classifies as capability-error, never as a control that
        // failed: we could not check, which is not the same as "it broke".
        assert!(
            flowproof_replay::runrecord::is_capability_error(&err),
            "must be capability-error, got: {err}"
        );
    }

    /// The same run with no faults still passes. The guard has to be
    /// falsifiable in both directions: a fault-check wired to fire
    /// unconditionally would satisfy the test above and quietly make every
    /// contained run a capability error.
    #[test]
    fn a_healthy_supervisor_with_nothing_blocked_still_passes() {
        let plan = egress_plan(true, vec![]);
        let run = egress_run_with(vec![], vec![]);
        let lane = check_egress(&plan, &run, &Containment::Enforced)
            .expect("a clean contained run certifies");
        let lane = lane.expect("engaged, so the lane is written");
        assert!(lane.blocked.is_empty());
        assert!(lane.containment.contains("enforced"));
    }

    /// Retries of one broken mechanism are one finding, not a hundred. The
    /// message counts every fault but names each distinct one once.
    #[test]
    fn repeated_faults_collapse_in_the_message() {
        let plan = egress_plan(true, vec![]);
        let fault = "connect: process_vm_readv: Operation not permitted".to_string();
        let run = egress_run_with(vec![], vec![fault.clone(), fault.clone(), fault]);
        let err = check_egress(&plan, &run, &Containment::Enforced).expect_err("faulted");
        assert!(err.contains("3 trapped syscall(s)"), "{err}");
        assert_eq!(
            err.matches("process_vm_readv").count(),
            1,
            "the distinct fault is named once: {err}"
        );
    }

    /// A spec whose only shape that matters here is its tools/mcp blocks.
    fn tool_spec(yaml: &str) -> FlowSpec {
        FlowSpec::parse(yaml).expect("spec parses")
    }

    fn forbidding(plan_tools: &[&str]) -> Plan {
        let mut plan = egress_plan(false, vec![]);
        plan.forbidden = plan_tools
            .iter()
            .map(|t| ToolCallExpectation::new(t))
            .collect();
        plan
    }

    /// A model-boundary mock does NOT stop the tool running, so a flow that
    /// mocks one gets told - at record, where the side effect fires.
    #[test]
    fn a_model_boundary_mock_warns_that_the_real_tool_still_runs() {
        let spec = tool_spec(
            "name: charge\napp: agent\nagent:\n  command: ./agent\n\
             tools:\n  - name: charge_card\n    result: {ok: true}\n\
             steps:\n  - prompt: pay the invoice\n",
        );
        let warning = unprotected_tool_warning(&spec, &egress_plan(false, vec![]), "record")
            .expect("a model-boundary mock earns a warning");
        assert!(warning.contains("charge_card"), "{warning}");
        assert!(warning.contains("MODEL boundary"), "{warning}");
        assert!(warning.contains("side effect fires"), "{warning}");
    }

    /// The half no prose covered: replay re-serves the RECORDED tool calls
    /// to a live agent, so a side effect recorded once fires on every CI
    /// run. The replay wording must say that, not repeat the record line.
    #[test]
    fn the_replay_warning_says_the_side_effect_fires_every_run() {
        let spec = tool_spec(
            "name: charge\napp: agent\nagent:\n  command: ./agent\n\
             tools:\n  - name: charge_card\n    result: {ok: true}\n\
             steps:\n  - prompt: pay the invoice\n",
        );
        let warning = unprotected_tool_warning(&spec, &egress_plan(false, vec![]), "replay")
            .expect("replay warns too");
        assert!(warning.contains("every run"), "{warning}");
        assert!(warning.contains("RECORDED"), "{warning}");
    }

    /// `assert_no_tool_call` proves the agent did not ASK for the tool. It
    /// does not prevent one, so it earns the same warning.
    #[test]
    fn a_forbid_assertion_alone_earns_the_warning() {
        let spec = tool_spec(
            "name: no alert\napp: agent\nagent:\n  command: ./agent\n\
             steps:\n  - prompt: summarise\n  - assert_no_tool_call: send_alert\n",
        );
        let warning = unprotected_tool_warning(&spec, &forbidding(&["send_alert"]), "record")
            .expect("a forbid assertion earns a warning");
        assert!(warning.contains("send_alert"), "{warning}");
    }

    /// The MCP boundary DOES intercept: flowproof answers the tool and the
    /// real server never runs it. Warning there would be false, and a
    /// warning that cries wolf is one people learn to skip past.
    #[test]
    fn an_mcp_intercepted_tool_is_silent() {
        let spec = tool_spec(
            "name: charge\napp: agent\nagent:\n  command: ./agent\n\
             mcp:\n  - name: billing\n    command: ./server\n\
             \x20   tools:\n      - name: charge_card\n        result: {ok: true}\n\
             steps:\n  - prompt: pay the invoice\n  - assert_no_tool_call: charge_card\n",
        );
        assert_eq!(
            unprotected_tool_warning(&spec, &forbidding(&["charge_card"]), "record"),
            None,
            "an MCP-answered tool must not warn"
        );
    }

    /// A flow that neither mocks nor forbids anything says nothing.
    #[test]
    fn a_plain_flow_is_silent() {
        let spec = tool_spec(
            "name: plain\napp: agent\nagent:\n  command: ./agent\n\
             steps:\n  - prompt: summarise\n",
        );
        assert_eq!(
            unprotected_tool_warning(&spec, &egress_plan(false, vec![]), "record"),
            None
        );
    }

    fn empty_run() -> AgentRun {
        AgentRun {
            served: 0,
            trigger: flowproof_adapters::Trigger::Process,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: "agent finished".into(),
            upstream_error: None,
            egress: Default::default(),
            divergence: None,
            timed_out: false,
        }
    }

    /// The failure a determinism tool must never let pass: the agent talked
    /// to the real provider, so nothing was captured. It must be an ERROR
    /// (no trace is written), and it must say WHY, because the usual cause
    /// is invisible - a client whose base URL comes from somewhere other
    /// than the variables flowproof injects.
    #[test]
    fn zero_captured_requests_fails_and_names_the_likely_cause() {
        let plan = egress_plan(false, vec![]);
        let err = require_progress(&empty_run(), &Cassette::default(), &plan)
            .expect_err("an empty cassette must fail the record");
        assert!(err.contains("0 model requests"), "{err}");
        assert!(err.contains("no trace was written"), "{err}");
        // The actionable half: what to do about it.
        assert!(err.contains("agent.env"), "{err}");
        assert!(err.contains("flowproof.proxy_url"), "{err}");
    }

    /// When the flow DOES map a custom variable, the advice changes: the
    /// mapping exists, so the suspect is whether it reaches the process
    /// that actually calls the model. That is the multi-process case.
    #[test]
    fn a_custom_mapping_is_named_and_the_advice_shifts_to_child_processes() {
        let mut plan = egress_plan(false, vec![]);
        plan.env
            .insert("AI_GATEWAY_URL".into(), "http://127.0.0.1:1".into());
        let err =
            require_progress(&empty_run(), &Cassette::default(), &plan).expect_err("still fails");
        assert!(err.contains("AI_GATEWAY_URL"), "names the mapping: {err}");
        assert!(err.contains("child"), "points at the child process: {err}");
    }

    /// A record run whose upstream failed is a different diagnosis and must
    /// not be reported as "you wired the proxy wrong".
    #[test]
    fn an_upstream_failure_is_reported_as_itself() {
        let mut run = empty_run();
        run.upstream_error = Some("connection refused".into());
        let err = require_progress(&run, &Cassette::default(), &egress_plan(false, vec![]))
            .expect_err("upstream failure fails");
        assert!(err.contains("touched the real model"), "{err}");
        assert!(
            !err.contains("0 model requests"),
            "not the wiring message: {err}"
        );
    }

    /// The two stdio guards describe DIFFERENT facts and must not be
    /// conflated again. An adopter lost real time to the merged version: it
    /// told them the stand-in was never spawned when it demonstrably had
    /// been, sending them to debug wiring that was already correct.
    #[test]
    fn the_missing_out_and_empty_lane_guards_say_different_things() {
        let path = std::path::Path::new("/tmp/run/files.out.json");
        let missing = stdio_missing_out_guard("files", path);
        let empty = stdio_empty_lane_guard("files", path);

        // Both name the server and the file they looked at, so the reader
        // can check the same thing flowproof checked.
        for m in [&missing, &empty] {
            assert!(m.contains("files"), "{m}");
            assert!(m.contains("/tmp/run/files.out.json"), "{m}");
        }

        // Missing out file: wiring IS the likely cause, and the env var to
        // point at is named.
        assert!(missing.contains("wrote nothing"), "{missing}");
        assert!(missing.contains("FLOWPROOF_MCP_SERVER_FILES"), "{missing}");

        // Empty lane: the stand-in RAN, so claiming otherwise is false. This
        // is the assertion that would have saved the adopter the detour.
        assert!(empty.contains("ran and wrote"), "{empty}");
        assert!(
            !empty.contains("never spawned") && !empty.contains("never ran"),
            "the empty-lane guard must not claim the stand-in never ran: {empty}"
        );
        assert!(
            empty.contains("NOT the obvious suspect"),
            "it must say wiring is not the suspect: {empty}"
        );
    }

    /// The missing-out guard offers its cause as a possibility rather than
    /// stating it as fact, because the previous version stated a cause it
    /// could not know.
    #[test]
    fn the_missing_out_guard_hedges_its_cause() {
        let missing = stdio_missing_out_guard("files", std::path::Path::new("/x/files.out.json"));
        assert!(missing.contains("Most likely"), "{missing}");
        // And it names the other possibility rather than pretending there
        // is only one.
        assert!(missing.contains("killed before"), "{missing}");
    }

    /// Under enforcement, an undeclared attempt fails the assertion, naming
    /// every destination and its protocol, deduped by destination.
    #[test]
    fn undeclared_egress_fails_naming_destinations() {
        let plan = egress_plan(true, vec!["api.example.com:443".into()]);
        let run = egress_run(vec![
            EgressEvent {
                destination: "198.51.100.9:443".into(),
                protocol: "tcp".into(),
                at_ms: 10,
            },
            // A retry of the same destination collapses to one.
            EgressEvent {
                destination: "198.51.100.9:443".into(),
                protocol: "tcp".into(),
                at_ms: 20,
            },
            EgressEvent {
                destination: "203.0.113.9:53".into(),
                protocol: "udp".into(),
                at_ms: 30,
            },
        ]);
        let err =
            check_egress(&plan, &run, &Containment::Enforced).expect_err("undeclared egress fails");
        assert!(err.contains("undeclared egress attempted"), "{err}");
        assert!(err.contains("198.51.100.9:443 (tcp)"), "{err}");
        assert!(err.contains("203.0.113.9:53 (udp)"), "{err}");
        assert!(err.contains("declare it in agent.allow_egress"), "{err}");
        // Deduped: the retry does not appear twice.
        assert_eq!(err.matches("198.51.100.9:443").count(), 1, "{err}");
    }

    /// A clean enforced run with declared allow passes and writes the audit
    /// lane (containment tag + unresolved allow-list + empty blocked).
    #[test]
    fn a_clean_enforced_run_writes_the_audit_lane() {
        let plan = egress_plan(true, vec!["${SERVICE_HOST}:443".into()]);
        let run = egress_run(vec![]);
        let lane = check_egress(&plan, &run, &Containment::Enforced)
            .expect("clean run passes")
            .expect("engaged feature writes a lane");
        assert_eq!(lane.containment, "enforced (linux seccomp)");
        // The allow-list travels UNRESOLVED.
        assert_eq!(lane.allowed, vec!["${SERVICE_HOST}:443".to_string()]);
        assert!(lane.blocked.is_empty());
    }

    /// A flow that never touches egress writes NO lane, so its trace is
    /// byte-identical to a pre-feature one.
    #[test]
    fn an_unused_feature_writes_no_lane() {
        let plan = egress_plan(false, vec![]);
        let run = egress_run(vec![]);
        let lane = check_egress(&plan, &run, &Containment::url_flow()).expect("nothing to certify");
        assert!(lane.is_none(), "no lane when the feature is unused");
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
                events: Vec::new(),
            },
        );
        let trace = AgentTrace {
            app: "agent".into(),
            mocks: BTreeMap::new(),
            cassette: neutral_cassette("hi", "there"),
            mcp,
            egress: None,
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

    // ---- MCP HTTP boundary (v3.2) ----
    //
    // These drive `McpContext` directly rather than the whole `record`/
    // `replay`, which also need a real model at the OTHER boundary. The MCP
    // HTTP boundary is exercised end to end with a fake real server and a
    // fake agent (the raw JSON-RPC POSTs below), no real MCP servers.

    /// A JSON-RPC request envelope.
    fn jsonrpc(id: i64, method: &str, params: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    /// The fake agent: read the `FLOWPROOF_MCP_URL_<NAME>` flowproof injected
    /// and POST one JSON-RPC message to it, returning (status, parsed body).
    fn mcp_post(url: &str, payload: serde_json::Value) -> (u16, serde_json::Value) {
        let rest = url.trim_start_matches("http://");
        let (addr, path) = rest
            .split_once('/')
            .map(|(a, p)| (a.to_string(), format!("/{p}")))
            .expect("url has a path");
        let body = payload.to_string();
        let mut stream = TcpStream::connect(&addr).expect("connect");
        let request = format!(
            "POST {path} HTTP/1.1\r\nhost: {addr}\r\ncontent-type: application/json\r\n\
             content-length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).expect("write");
        let mut raw = String::new();
        stream.read_to_string(&mut raw).expect("read");
        let status = raw
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .expect("status");
        let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default();
        (
            status,
            serde_json::from_str(body).unwrap_or(serde_json::Value::Null),
        )
    }

    /// Read one JSON-RPC request off a socket: (id, method, params).
    fn read_jsonrpc(stream: &mut TcpStream) -> (serde_json::Value, String, serde_json::Value) {
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
            if let Some((n, v)) = header.split_once(':') {
                if n.eq_ignore_ascii_case("content-length") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).ok();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        (
            json.get("id").cloned().unwrap_or(serde_json::Value::Null),
            json.get("method")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
            json.get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
    }

    /// The fake real server's answer for a method.
    fn fake_result(method: &str, params: &serde_json::Value) -> serde_json::Value {
        match method {
            "initialize" => {
                serde_json::json!({ "protocolVersion": "2024-11-05", "serverInfo": { "name": "fake" } })
            }
            "tools/list" => serde_json::json!({ "tools": [{ "name": "get_weather" }] }),
            "tools/call" => {
                let city = params
                    .get("arguments")
                    .and_then(|a| a.get("city"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("?");
                serde_json::json!({ "content": [{ "type": "text", "text": format!("sunny in {city}") }] })
            }
            _ => serde_json::json!({}),
        }
    }

    /// A fake REAL streamable-HTTP MCP server: answers initialize /
    /// tools/list / tools/call, one request per `Connection: close` socket.
    /// `sse` answers via a `text/event-stream` `data:` frame (to exercise the
    /// record SSE-read path); otherwise a plain `application/json` body.
    /// Serves exactly `count` requests, then exits so the test can join it.
    fn spawn_fake_mcp(sse: bool, count: usize) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind mcp");
        let addr = listener.local_addr().expect("addr");
        let url = format!("http://{addr}/mcp");
        let handle = std::thread::spawn(move || {
            for _ in 0..count {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let (id, method, params) = read_jsonrpc(&mut stream);
                let result = fake_result(&method, &params);
                let message =
                    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
                let response = if sse {
                    let frame = format!("event: message\ndata: {message}\n\n");
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                         mcp-session-id: real-123\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{frame}",
                        frame.len()
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                         mcp-session-id: real-123\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{message}",
                        message.len()
                    )
                };
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (url, handle)
    }

    /// An `app: agent` spec whose single MCP server is HTTP (`url:`).
    fn http_mcp_spec(url: &str, mock_send_alert: bool) -> FlowSpec {
        let tools = if mock_send_alert {
            "\n    tools:\n      - name: send_alert\n        result: { delivered: true }"
        } else {
            ""
        };
        FlowSpec::parse(&format!(
            "name: n\napp: agent\nagent:\n  command: x\nmcp:\n  - name: remote\n    url: {url}{tools}\nsteps:\n  - prompt: hi\n"
        ))
        .expect("spec parses")
    }

    fn mcp_url_env(ctx: &McpContext) -> String {
        ctx.env_vars().expect("env")["FLOWPROOF_MCP_URL_REMOTE"].clone()
    }

    /// A fake REAL streamable-HTTP MCP server that emits a server-initiated
    /// NOTIFICATION inline in the `tools/call` SSE body, BEFORE the response
    /// frame. Serves exactly `count` requests, then exits.
    fn spawn_fake_mcp_notifying(count: usize) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind mcp");
        let addr = listener.local_addr().expect("addr");
        let url = format!("http://{addr}/mcp");
        let handle = std::thread::spawn(move || {
            for _ in 0..count {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let (id, method, params) = read_jsonrpc(&mut stream);
                let result = fake_result(&method, &params);
                let message =
                    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
                let body = if method == "tools/call" {
                    let notif = serde_json::json!({ "jsonrpc": "2.0",
                        "method": "notifications/message",
                        "params": { "level": "info", "data": "weather ready" } });
                    format!("event: message\ndata: {notif}\n\nevent: message\ndata: {message}\n\n")
                } else {
                    format!("event: message\ndata: {message}\n\n")
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                     mcp-session-id: real-123\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (url, handle)
    }

    /// The fake agent opens the standalone GET SSE stream on the listener:
    /// send the GET, read the status and drain the head, return a reader on
    /// the body with a read timeout.
    fn mcp_get(url: &str) -> (u16, std::io::BufReader<TcpStream>) {
        let rest = url.trim_start_matches("http://");
        let (addr, path) = rest
            .split_once('/')
            .map(|(a, p)| (a.to_string(), format!("/{p}")))
            .expect("url has a path");
        let stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_millis(800)))
            .ok();
        let mut writer = stream.try_clone().expect("clone");
        let request =
            format!("GET {path} HTTP/1.1\r\nhost: {addr}\r\naccept: text/event-stream\r\n\r\n");
        writer.write_all(request.as_bytes()).expect("write");
        let mut reader = std::io::BufReader::new(stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).ok();
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                break;
            }
            if header == "\r\n" || header == "\n" {
                break;
            }
        }
        (status, reader)
    }

    /// Read up to `want` SSE notification frames off a stream, giving up on a
    /// read timeout.
    fn read_get_notifications(
        reader: &mut std::io::BufReader<TcpStream>,
        want: usize,
    ) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        let mut data = String::new();
        let mut line = String::new();
        while out.len() < want {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                if !data.is_empty() {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                        out.push(v);
                    }
                    data.clear();
                }
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("data:") {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest);
            }
        }
        out
    }

    /// End to end: record against a fake HTTP server captures the lane, then
    /// replay serves it with the fake server GONE - zero network.
    #[test]
    fn http_mcp_records_then_replays_with_zero_network() {
        let (real_url, handle) = spawn_fake_mcp(false, 3);
        let spec = http_mcp_spec(&real_url, false);

        // RECORD: the listener forwards to the fake real server.
        let ctx = McpContext::setup(&spec, "record", &BTreeMap::new())
            .expect("setup")
            .expect("some");
        let url = mcp_url_env(&ctx);
        let (s, _) = mcp_post(
            &url,
            jsonrpc(
                1,
                "initialize",
                serde_json::json!({ "protocolVersion": "2024-11-05", "clientInfo": { "name": "a" } }),
            ),
        );
        assert_eq!(s, 200);
        mcp_post(&url, jsonrpc(2, "tools/list", serde_json::json!({})));
        let (_, call) = mcp_post(
            &url,
            jsonrpc(
                3,
                "tools/call",
                serde_json::json!({ "name": "get_weather", "arguments": { "city": "Paris" } }),
            ),
        );
        assert_eq!(
            call["result"]["content"][0]["text"], "sunny in Paris",
            "the real server answered at record"
        );
        handle.join().ok();

        let recorded = ctx.collect_record(&spec).expect("record captured");
        drop(ctx);
        let lane = &recorded["remote"].calls;
        assert_eq!(lane.len(), 3, "initialize, tools/list, tools/call");
        assert_eq!(lane[0].method, "initialize");
        assert_eq!(lane[2].method, "tools/call");

        // REPLAY: no fake server anywhere. The listener answers from the lane.
        let mut trace = BTreeMap::new();
        trace.insert("remote".to_string(), recorded["remote"].clone());
        let ctx = McpContext::setup(&spec, "replay", &trace)
            .expect("setup")
            .expect("some");
        let url = mcp_url_env(&ctx);
        let (s, init) = mcp_post(
            &url,
            jsonrpc(
                1,
                "initialize",
                serde_json::json!({ "protocolVersion": "2024-11-05", "clientInfo": { "name": "z" } }),
            ),
        );
        assert_eq!(s, 200);
        assert_eq!(
            init["result"]["serverInfo"]["name"], "fake",
            "served from the recording, no server running"
        );
        mcp_post(&url, jsonrpc(2, "tools/list", serde_json::json!({})));
        let (_, call) = mcp_post(
            &url,
            jsonrpc(
                3,
                "tools/call",
                serde_json::json!({ "name": "get_weather", "arguments": { "city": "Paris" } }),
            ),
        );
        assert_eq!(call["result"]["content"][0]["text"], "sunny in Paris");
        ctx.check_replay(&spec, &trace).expect("replay reproduced");
    }

    /// The record SSE-read path: a fake server answering `text/event-stream`
    /// is parsed into the captured lane.
    #[test]
    fn http_mcp_records_over_sse() {
        let (real_url, handle) = spawn_fake_mcp(true, 1);
        let spec = http_mcp_spec(&real_url, false);
        let ctx = McpContext::setup(&spec, "record", &BTreeMap::new())
            .expect("setup")
            .expect("some");
        let url = mcp_url_env(&ctx);
        let (s, body) = mcp_post(
            &url,
            jsonrpc(
                1,
                "tools/call",
                serde_json::json!({ "name": "get_weather", "arguments": { "city": "Berlin" } }),
            ),
        );
        assert_eq!(s, 200);
        assert_eq!(
            body["result"]["content"][0]["text"], "sunny in Berlin",
            "the SSE data frame was parsed at record"
        );
        handle.join().ok();
        let recorded = ctx.collect_record(&spec).expect("captured");
        assert_eq!(recorded["remote"].calls.len(), 1);
        assert_eq!(recorded["remote"].calls[0].method, "tools/call");
    }

    /// A mocked tool is answered locally and NEVER forwarded (the real server
    /// address points nowhere, and is never contacted); replay serves the
    /// mock from the lane.
    #[test]
    fn http_mcp_mocked_tool_is_never_forwarded() {
        // Port 9 (discard) would refuse a connection; a forward would fail.
        let spec = http_mcp_spec("http://127.0.0.1:9/mcp", true);
        let ctx = McpContext::setup(&spec, "record", &BTreeMap::new())
            .expect("setup")
            .expect("some");
        let url = mcp_url_env(&ctx);
        let (s, body) = mcp_post(
            &url,
            jsonrpc(
                1,
                "tools/call",
                serde_json::json!({ "name": "send_alert", "arguments": { "to": "ops" } }),
            ),
        );
        assert_eq!(s, 200);
        assert_eq!(
            body["result"]["content"][0]["text"], r#"{"delivered":true}"#,
            "answered locally from the mock"
        );
        let recorded = ctx.collect_record(&spec).expect("captured");
        drop(ctx);
        assert_eq!(recorded["remote"].calls.len(), 1);
        assert_eq!(
            recorded["remote"].mocks["send_alert"],
            serde_json::json!({ "delivered": true })
        );

        // REPLAY serves the mock from the lane, still with no server.
        let mut trace = BTreeMap::new();
        trace.insert("remote".to_string(), recorded["remote"].clone());
        let ctx = McpContext::setup(&spec, "replay", &trace)
            .expect("setup")
            .expect("some");
        let url = mcp_url_env(&ctx);
        let (_, body) = mcp_post(
            &url,
            jsonrpc(
                1,
                "tools/call",
                serde_json::json!({ "name": "send_alert", "arguments": { "to": "ops" } }),
            ),
        );
        assert_eq!(
            body["result"]["content"][0]["text"],
            r#"{"delivered":true}"#
        );
        ctx.check_replay(&spec, &trace).expect("replay reproduced");
    }

    /// A replay whose `tools/call` argument changed gets an in-band JSON-RPC
    /// error (HTTP 200, no 409) and fails the run naming the path.
    #[test]
    fn http_mcp_replay_divergence_names_the_path() {
        let lane = vec![McpCall {
            method: "tools/call".into(),
            params: serde_json::json!({ "name": "get_weather", "arguments": { "city": "Paris" } }),
            result: serde_json::json!({ "content": [] }),
        }];
        let mut trace = BTreeMap::new();
        trace.insert(
            "remote".to_string(),
            McpServerTrace {
                mocks: BTreeMap::new(),
                calls: lane,
                events: Vec::new(),
            },
        );
        let spec = http_mcp_spec("http://unused/mcp", false);
        let ctx = McpContext::setup(&spec, "replay", &trace)
            .expect("setup")
            .expect("some");
        let url = mcp_url_env(&ctx);
        let (s, body) = mcp_post(
            &url,
            jsonrpc(
                1,
                "tools/call",
                serde_json::json!({ "name": "get_weather", "arguments": { "city": "Berlin" } }),
            ),
        );
        assert_eq!(s, 200, "in-band JSON-RPC error, not a 409");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("arguments.city"),
            "{body}"
        );
        let why = ctx.check_replay(&spec, &trace).expect_err("must fail");
        assert!(why.contains("diverged at call 1"), "{why}");
        assert!(why.contains("arguments.city"), "{why}");
    }

    /// The record wiring guard: a declared HTTP server the agent never
    /// contacts fails the record with the named message.
    #[test]
    fn http_mcp_uncontacted_server_fails_the_record() {
        let spec = http_mcp_spec("http://127.0.0.1:9/mcp", false);
        let ctx = McpContext::setup(&spec, "record", &BTreeMap::new())
            .expect("setup")
            .expect("some");
        // The agent never contacts the listener.
        let why = ctx
            .collect_record(&spec)
            .expect_err("uncontacted must fail");
        assert!(
            why.contains("never contacted flowproof's MCP listener for `remote`"),
            "{why}"
        );
        assert!(why.contains("FLOWPROOF_MCP_URL_REMOTE"), "{why}");
    }

    /// Transport-blind matching: a lane recorded via STDIO replays through an
    /// HTTP-declared server. Nothing in the lane names a transport.
    #[test]
    fn a_stdio_recorded_lane_replays_via_an_http_server() {
        let lane = vec![
            McpCall {
                method: "initialize".into(),
                params: serde_json::json!({ "protocolVersion": "2024-11-05" }),
                result: serde_json::json!({ "protocolVersion": "2024-11-05" }),
            },
            McpCall {
                method: "tools/call".into(),
                params: serde_json::json!({ "name": "get_weather", "arguments": { "city": "Paris" } }),
                result: serde_json::json!({ "content": [{ "type": "text", "text": "sunny" }] }),
            },
        ];
        let mut trace = BTreeMap::new();
        trace.insert(
            "remote".to_string(),
            McpServerTrace {
                mocks: BTreeMap::new(),
                calls: lane,
                events: Vec::new(),
            },
        );
        // Declared as HTTP, though a stdio record produced the lane.
        let spec = http_mcp_spec("http://unused/mcp", false);
        let ctx = McpContext::setup(&spec, "replay", &trace)
            .expect("setup")
            .expect("some");
        let url = mcp_url_env(&ctx);
        let (s, init) = mcp_post(
            &url,
            jsonrpc(
                1,
                "initialize",
                serde_json::json!({ "protocolVersion": "2024-11-05", "clientInfo": { "name": "x" } }),
            ),
        );
        assert_eq!(s, 200);
        assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
        let (_, call) = mcp_post(
            &url,
            jsonrpc(
                2,
                "tools/call",
                serde_json::json!({ "name": "get_weather", "arguments": { "city": "Paris" } }),
            ),
        );
        assert_eq!(call["result"]["content"][0]["text"], "sunny");
        ctx.check_replay(&spec, &trace)
            .expect("a stdio-recorded lane replays over http");
    }

    /// End to end (v3.3): record captures a server notification into the mcp
    /// lane at the right anchor, and replay re-emits it over the standalone
    /// GET stream at that point - a second GET is a 409, and the run passes.
    #[test]
    fn http_mcp_records_a_notification_then_replays_it_over_the_get_stream() {
        // RECORD: two POSTs (initialize, tools/call); the tools/call SSE body
        // carries a notification before its response.
        let (real_url, handle) = spawn_fake_mcp_notifying(2);
        let spec = http_mcp_spec(&real_url, false);
        let ctx = McpContext::setup(&spec, "record", &BTreeMap::new())
            .expect("setup")
            .expect("some");
        let url = mcp_url_env(&ctx);
        mcp_post(
            &url,
            jsonrpc(
                1,
                "initialize",
                serde_json::json!({ "protocolVersion": "2024-11-05", "clientInfo": { "name": "a" } }),
            ),
        );
        let (_, call) = mcp_post(
            &url,
            jsonrpc(
                2,
                "tools/call",
                serde_json::json!({ "name": "get_weather", "arguments": { "city": "Paris" } }),
            ),
        );
        assert_eq!(call["result"]["content"][0]["text"], "sunny in Paris");
        handle.join().ok();

        let recorded = ctx.collect_record(&spec).expect("record captured");
        drop(ctx);
        let lane = &recorded["remote"];
        assert_eq!(lane.calls.len(), 2, "initialize, tools/call");
        assert_eq!(lane.events.len(), 1, "the notification was captured");
        assert_eq!(lane.events[0].method, "notifications/message");
        assert_eq!(
            lane.events[0].after, 1,
            "anchored after initialize, before tools/call"
        );

        // REPLAY: no fake server. The agent opens the GET push stream, drives
        // the two calls, and the notification arrives after `initialize`.
        let mut trace = BTreeMap::new();
        trace.insert("remote".to_string(), recorded["remote"].clone());
        let ctx = McpContext::setup(&spec, "replay", &trace)
            .expect("setup")
            .expect("some");
        let url = mcp_url_env(&ctx);

        let (status, mut reader) = mcp_get(&url);
        assert_eq!(status, 200, "the push stream opens");
        let (status2, _second) = mcp_get(&url);
        assert_eq!(status2, 409, "a second GET stream is a 409");

        // Nothing is due before any call is answered (anchor is 1).
        assert!(
            read_get_notifications(&mut reader, 1).is_empty(),
            "not due before initialize"
        );
        mcp_post(
            &url,
            jsonrpc(
                1,
                "initialize",
                serde_json::json!({ "protocolVersion": "2024-11-05", "clientInfo": { "name": "z" } }),
            ),
        );
        let got = read_get_notifications(&mut reader, 1);
        assert_eq!(got.len(), 1, "the notification arrives after initialize");
        assert_eq!(got[0]["method"], "notifications/message");
        let (_, call) = mcp_post(
            &url,
            jsonrpc(
                2,
                "tools/call",
                serde_json::json!({ "name": "get_weather", "arguments": { "city": "Paris" } }),
            ),
        );
        assert_eq!(call["result"]["content"][0]["text"], "sunny in Paris");
        // The verdict judges calls only - a delivered notification is not an
        // assertion, and the run reproduces.
        ctx.check_replay(&spec, &trace).expect("replay reproduced");
    }

    /// Additivity: an mcp lane with calls but NO events serializes with no
    /// `events` key and round-trips byte-identical, and a lane written before
    /// v3.3 (no `events` key) deserializes with an empty events vec. The
    /// event-free trace is the byte-for-byte trace v3.2 wrote.
    #[test]
    fn an_event_free_mcp_lane_round_trips_byte_identical() {
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
                events: Vec::new(),
            },
        );
        let trace = AgentTrace {
            app: "agent".into(),
            mocks: BTreeMap::new(),
            cassette: neutral_cassette("hi", "there"),
            mcp,
            egress: None,
        };
        let json = serde_json::to_string_pretty(&trace).expect("serialize");
        assert!(
            !json.contains("events"),
            "no events key on an event-free lane: {json}"
        );

        // A hand-built pre-v3.3 lane (calls, no events) deserializes with an
        // empty events vec and re-serializes to the identical bytes.
        let back: AgentTrace = serde_json::from_str(&json).expect("deserialize");
        assert!(
            back.mcp["weather"].events.is_empty(),
            "events default empty"
        );
        let reencoded = serde_json::to_string_pretty(&back).expect("re-serialize");
        assert_eq!(
            json, reencoded,
            "event-free lane round-trips byte-identical"
        );
    }
}
