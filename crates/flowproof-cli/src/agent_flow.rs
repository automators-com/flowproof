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
use flowproof_adapters::agent_runner::{run_against, AgentRun};
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

/// Everything a phase needs pulled off the spec once.
struct Plan {
    command: String,
    env: BTreeMap<String, String>,
    mocks: Mocks,
    strict: bool,
    tool_calls: Vec<ToolCallExpectation>,
    forbidden: Vec<ToolCallExpectation>,
    reply_contains: Vec<String>,
}

fn plan(spec: &FlowSpec) -> Result<Plan, String> {
    let agent = spec
        .agent
        .as_ref()
        .ok_or("an app: agent flow needs an agent: block")?;
    let mut env = agent_env(spec)?;
    env.insert(PROMPT_VAR.to_string(), prompt_of(spec));

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
        command: flowproof_trace::secret::resolve_refs(&agent.command)
            .map_err(|e| e.to_string())?,
        env,
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
fn require_progress(run: &AgentRun, cassette: &Cassette) -> Result<(), String> {
    if let Some(err) = &run.upstream_error {
        return Err(format!(
            "recording touched the real model and it failed: {err}"
        ));
    }
    if cassette.is_empty() {
        return Err(format!(
            "the agent made no model calls; it exited {} without talking to the proxy.\n\
             stderr:\n{}",
            run.exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "with no code".into()),
            run.stderr.trim()
        ));
    }
    Ok(())
}

/// Record an `app: agent` flow: run it against a real model, capture the
/// trajectory, check the assertions, and write the cassette to `out`.
pub fn record(spec: &FlowSpec, out: &Path) -> Result<(), String> {
    let plan = plan(spec)?;
    let upstream = upstream()?;
    let auth = upstream_auth();
    let proxy = AgentProxy::record(&upstream, auth, plan.mocks.clone())
        .map_err(|e| format!("starting the record proxy: {e}"))?;
    let run =
        run_against(&proxy, &plan.command, &plan.env, AGENT_TIMEOUT).map_err(|e| e.to_string())?;
    let cassette = proxy.captured();
    drop(proxy);

    require_progress(&run, &cassette)?;
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
    let proxy = AgentProxy::start(trace.cassette, mocks)
        .map_err(|e| format!("starting the replay proxy: {e}"))?;
    let run =
        run_against(&proxy, &plan.command, &plan.env, AGENT_TIMEOUT).map_err(|e| e.to_string())?;
    let cassette = proxy.captured(); // empty in replay
    drop(proxy);
    let _ = cassette;

    run.reproduced(expected)?;
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
