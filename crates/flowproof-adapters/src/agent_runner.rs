//! Running the system under test against the proxy: spawn the process,
//! point its model API at localhost, wait, and collect what happened at
//! the boundary.
//!
//! The agent is a black box and stays one. flowproof does not import it,
//! instrument it, or ask it to adopt a testing mode - it starts the same
//! command a developer would, with one environment variable pointing
//! somewhere else. Everything the run is judged on comes from the
//! boundary, not from the process.
//!
//! Which is also why the VERDICT comes from the proxy rather than the
//! exit code. An agent that catches the 409 a divergence returns and
//! exits 0 must not turn a divergence into a pass, and plenty of
//! frameworks swallow HTTP errors by default. The exit code is reported
//! because it is useful context, never because it decides anything.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use flowproof_trace::cassette::{Cassette, Divergence};
use flowproof_trace::substitution::Mocks;

use crate::agent_proxy::{AgentProxy, ProxyError};
use crate::egress::{AllowSet, EgressLog};

/// The environment variables an OpenAI-compatible client reads for its
/// base URL. All of them are set, because the system under test picks one
/// and a spec author should not have to know which.
///
/// `OPENAI_API_KEY` is set to a placeholder for the same reason: a client
/// that refuses to start without a key would fail before reaching the
/// proxy, and there is no real key to leak because there is no real
/// upstream.
const BASE_URL_VARS: [&str; 3] = ["OPENAI_BASE_URL", "OPENAI_API_BASE", "OPENAI_BASE"];

/// Split a command line into argv, honouring double quotes so a path
/// with spaces survives.
///
/// `split_command_line` in the driver crate hands the remainder back as
/// ONE string, which is what `CreateProcess` wants and what an `app:
/// {command}` flow passes to a Windows program verbatim. Spawning a
/// process here needs argv instead, so the same quoting rule is applied
/// to every argument rather than only to the program.
pub(crate) fn argv(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in command.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

/// The Anthropic base URL: the proxy origin with the trailing `/v1`
/// removed, because the Anthropic SDK appends `/v1/messages` itself and
/// would otherwise call `/v1/v1/messages`. The OpenAI vars keep the `/v1`.
fn anthropic_base(base: &str) -> String {
    base.strip_suffix("/v1").unwrap_or(base).to_string()
}

/// What a run produced.
#[derive(Debug)]
pub struct AgentRun {
    /// Model calls the proxy served from the recording.
    pub served: usize,
    /// The first divergence, if the trajectory left its recording.
    pub divergence: Option<Divergence>,
    /// Exit status, `None` if the process had to be killed at the
    /// deadline. Context, never the verdict.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    /// A real-model call that failed, in record mode. `None` in replay.
    pub upstream_error: Option<String>,
    /// What egress containment denied during the run. Empty on an
    /// uncontained run, and empty on a contained run that attempted nothing
    /// undeclared. Surfaced beside `divergence`, like [`ProxyLog`].
    ///
    /// [`ProxyLog`]: crate::agent_proxy::ProxyLog
    pub egress: EgressLog,
}

impl AgentRun {
    /// Did the trajectory match its recording all the way through?
    ///
    /// Deliberately not "did the process succeed". A run that diverged,
    /// served nothing, or had to be killed did not reproduce the
    /// recording, whatever the process thought of itself.
    pub fn reproduced(&self, expected_turns: usize) -> Result<(), String> {
        if let Some(divergence) = &self.divergence {
            return Err(divergence.to_string());
        }
        if self.timed_out {
            return Err(format!(
                "the agent did not finish in time; it made {} of {expected_turns} model calls",
                self.served
            ));
        }
        if self.served != expected_turns {
            return Err(format!(
                "the agent made {} model calls, the recording has {expected_turns}",
                self.served
            ));
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("starting the agent ({command}): {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("starting the model proxy: {0}")]
    Proxy(ProxyError),
    #[error("a spec is code, but an empty command is not: `agent.command` is blank")]
    NoCommand,
    #[error("a spec is code, but an empty url is not: `agent.url` is blank")]
    NoUrl,
    /// The trigger POST could not reach the service. A SETUP error, the
    /// http analogue of [`RunError::Spawn`] - never a verdict, because a
    /// service flowproof cannot reach never diverged; it never ran.
    #[error("could not reach the agent service at {url}: {reason} - is it running?")]
    Unreachable { url: String, reason: String },
}

/// Launch `command`, serve `cassette` to it, and wait for it to finish.
///
/// `env` is the spec's own variables, applied on top of the proxy's, so a
/// flow can pass an API base a client reads under some other name without
/// this module having to know every framework's spelling.
pub fn run(
    command: &str,
    env: &BTreeMap<String, String>,
    cassette: Cassette,
    mocks: Mocks,
    timeout: Duration,
) -> Result<AgentRun, RunError> {
    let proxy = AgentProxy::start(cassette, mocks, 0).map_err(RunError::Proxy)?;
    run_against(&proxy, command, env, timeout)
}

/// Drive an already-running HTTP service against `proxy` and collect what
/// happened at the boundary - the parallel to [`run_against`] for a system
/// under test flowproof did NOT start.
///
/// flowproof does a synchronous `POST <url>` with `content-type:
/// application/json` and body `{"prompt": "<joined prompt steps>"}`, plus
/// the resolved `headers`. The service, which already points its model calls
/// at the proxy, makes those calls while answering the POST. Everything the
/// run is judged on still comes from [`AgentProxy::log`], NOT the HTTP
/// response: the trigger status is context (it lands where a process exit
/// code would), the response body is context (where stdout would), and a
/// service that swallows the proxy's 409 and answers 200 must not turn a
/// divergence into a pass.
///
/// `timeout` is the request timeout; hitting it maps to `timed_out` exactly
/// as the process driver's kill-at-deadline does. A connection that cannot
/// be made is [`RunError::Unreachable`] - a setup error, not a verdict.
pub fn run_http(
    proxy: &AgentProxy,
    url: &str,
    headers: &BTreeMap<String, String>,
    prompt: &str,
    timeout: Duration,
) -> Result<AgentRun, RunError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(RunError::NoUrl);
    }
    let body = serde_json::json!({ "prompt": prompt }).to_string();

    // `http_status_as_error(false)`: a 4xx/5xx is a real answer whose status
    // and body are context, not a transport failure - the proxy log, not the
    // status, decides the verdict. `timeout_global` bounds the whole request
    // so a hung service maps to `timed_out` like a killed process.
    let config = ureq::config::Config::builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build();
    let agent = config.new_agent();
    let mut request = agent.post(url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }

    let (exit_code, timed_out, stdout) = match request.send(body.as_bytes()) {
        Ok(mut response) => {
            let status = i32::from(response.status().as_u16());
            let text = response.body_mut().read_to_string().unwrap_or_default();
            (Some(status), false, text)
        }
        // A request-timeout is the http kill-at-deadline: the flag, not a
        // setup error, so the verdict still comes from what was served.
        Err(ureq::Error::Timeout(_)) => (None, true, String::new()),
        // Any other transport failure means the trigger never landed: the
        // service is not reachable, which is setup, not a divergence.
        Err(e) => {
            return Err(RunError::Unreachable {
                url: url.to_string(),
                reason: e.to_string(),
            });
        }
    };

    let log = proxy.log();
    let run = AgentRun {
        served: log.served,
        divergence: log.divergence.clone(),
        exit_code,
        timed_out,
        stdout,
        stderr: String::new(),
        upstream_error: log.upstream_error.clone(),
        egress: EgressLog::default(),
    };
    drop(log);
    Ok(run)
}

/// Build the agent's [`Command`] with the proxy pointed at `base` and the
/// spec's env applied on top. Shared by the plain and contained spawn paths.
fn configure(
    command: &str,
    base: &str,
    env: &BTreeMap<String, String>,
) -> Result<Command, RunError> {
    let command = command.trim();
    if command.is_empty() {
        return Err(RunError::NoCommand);
    }
    let parts = argv(command);
    let (program, args) = parts.split_first().ok_or(RunError::NoCommand)?;

    let mut child = Command::new(program);
    child
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for var in BASE_URL_VARS {
        child.env(var, base);
    }
    child.env("OPENAI_API_KEY", "flowproof-replay-no-key-needed");
    // The Anthropic SDK reads its own base URL and appends `/v1/messages`
    // itself, so it wants the origin WITHOUT the `/v1` the OpenAI vars keep;
    // hand it the suffix-free form. A placeholder key for the same reason
    // OPENAI_API_KEY gets one: a client that refuses to start without a key
    // must still reach the proxy, and there is no real upstream to leak to.
    child.env("ANTHROPIC_BASE_URL", anthropic_base(base));
    child.env("ANTHROPIC_API_KEY", "flowproof-replay-no-key-needed");
    // `${FLOWPROOF_LLM_PROXY}` is the documented handle for the base URL,
    // for clients that take it as an argument rather than an env var.
    child.env("FLOWPROOF_LLM_PROXY", base);
    // The spec's own env goes on LAST so a flow can override any of the
    // above; it knows its client better than this module does.
    for (key, value) in env {
        child.env(key, value);
    }
    Ok(child)
}

/// Wait for `child` to the deadline, killing it at the timeout. Returns the
/// exit status (`None` if killed or unwaitable) and whether it timed out.
fn wait_to_deadline(
    child: &mut std::process::Child,
    timeout: Duration,
) -> (Option<std::process::ExitStatus>, bool) {
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if Instant::now() >= deadline {
            // Kill rather than wait forever: an agent that hangs waiting
            // for a turn the recording does not have would otherwise take
            // the whole suite down with it.
            let _ = child.kill();
            let _ = child.wait();
            timed_out = true;
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    (status, timed_out)
}

/// Drain a child's stdout and stderr pipes to strings.
fn read_pipes(child: &mut std::process::Child) -> (String, String) {
    let read = |pipe: Option<&mut dyn Read>| {
        let mut buffer = String::new();
        if let Some(pipe) = pipe {
            let _ = pipe.read_to_string(&mut buffer);
        }
        buffer
    };
    let stdout = read(child.stdout.as_mut().map(|p| p as &mut dyn Read));
    let stderr = read(child.stderr.as_mut().map(|p| p as &mut dyn Read));
    (stdout, stderr)
}

/// Spawn the agent against an ALREADY-STARTED proxy and wait for it to
/// finish. The orchestration uses this to drive a RECORD proxy (which
/// forwards to a real model) as easily as a replay one - the process does
/// not know or care which mode the endpoint it was handed is in.
///
/// UNcontained: no egress filter. [`run_against_contained`] is the path an
/// `app: agent` flow takes, so record and replay share a denial environment.
pub fn run_against(
    proxy: &AgentProxy,
    command: &str,
    env: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<AgentRun, RunError> {
    let base = proxy.base_url();
    let mut cmd = configure(command, &base, env)?;
    let mut child = cmd.spawn().map_err(|source| RunError::Spawn {
        command: command.trim().to_string(),
        source,
    })?;

    let (status, timed_out) = wait_to_deadline(&mut child, timeout);
    let (stdout, stderr) = read_pipes(&mut child);

    let log = proxy.log();
    let run = AgentRun {
        served: log.served,
        divergence: log.divergence.clone(),
        exit_code: status.and_then(|s| s.code()),
        timed_out,
        stdout,
        stderr,
        upstream_error: log.upstream_error.clone(),
        egress: EgressLog::default(),
    };
    drop(log);
    Ok(run)
}

/// Spawn the agent against `proxy` with egress CONTAINED to `allow`. Live in
/// both record and replay (a determinism requirement: the same denial
/// environment both phases reproduces the same trajectory). On Linux this
/// installs the real seccomp filter and services it for the run; on every
/// other platform it is exactly [`run_against`] with an empty egress log,
/// since the mechanism is Linux-only and the tier is reported "not
/// contained" independently.
#[cfg(target_os = "linux")]
pub fn run_against_contained(
    proxy: &AgentProxy,
    command: &str,
    env: &BTreeMap<String, String>,
    timeout: Duration,
    allow: &AllowSet,
) -> Result<AgentRun, RunError> {
    let base = proxy.base_url();
    let mut cmd = configure(command, &base, env)?;
    // Install the filter into the child's pre_exec BEFORE spawn; the parent
    // keeps its socket end to receive the notify fd once the child installs.
    let prep = crate::egress_linux::install(&mut cmd, allow).map_err(|source| RunError::Spawn {
        command: command.trim().to_string(),
        source,
    })?;
    let spawned = Instant::now();
    let mut child = cmd.spawn().map_err(|source| RunError::Spawn {
        command: command.trim().to_string(),
        source,
    })?;
    // Start the supervisor: receive the notify fd and service it for the run.
    let supervisor = prep
        .into_supervisor(spawned)
        .map_err(|source| RunError::Spawn {
            command: command.trim().to_string(),
            source,
        })?;

    let (status, timed_out) = wait_to_deadline(&mut child, timeout);
    let (stdout, stderr) = read_pipes(&mut child);
    let egress = supervisor.stop_and_collect();

    let log = proxy.log();
    let run = AgentRun {
        served: log.served,
        divergence: log.divergence.clone(),
        exit_code: status.and_then(|s| s.code()),
        timed_out,
        stdout,
        stderr,
        upstream_error: log.upstream_error.clone(),
        egress,
    };
    drop(log);
    Ok(run)
}

#[cfg(not(target_os = "linux"))]
pub fn run_against_contained(
    proxy: &AgentProxy,
    command: &str,
    env: &BTreeMap<String, String>,
    timeout: Duration,
    _allow: &AllowSet,
) -> Result<AgentRun, RunError> {
    // The seccomp mechanism is Linux-only; elsewhere this is the plain path,
    // and the report's tier line says "not contained".
    run_against(proxy, command, env, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowproof_trace::cassette::{Message, ToolCall, Turn, TurnRequest, TurnResponse};

    /// A fake system under test: a real process that speaks the
    /// chat-completions API, so the runner is exercised end to end
    /// without pulling in an agent framework.
    ///
    /// Python because every machine that runs this suite already has it
    /// (the SAP simulator makes the same bet) and because it needs no
    /// build step.
    const FAKE_AGENT: &str = r#"
import json, os, sys, urllib.request

base = os.environ["OPENAI_BASE_URL"]
turns = int(os.environ.get("FAKE_TURNS", "1"))
prompt = os.environ.get("FAKE_PROMPT", "Book a flight to Nairobi")
messages = [{"role": "user", "content": prompt}]

for _ in range(turns):
    payload = json.dumps({
        "model": "gpt-4o",
        "messages": messages,
        "tools": [{"type": "function", "function": {"name": "search_flights"}}],
    }).encode()
    request = urllib.request.Request(
        base + "/chat/completions", data=payload,
        headers={"content-type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request) as response:
            body = json.load(response)
    except urllib.error.HTTPError as e:
        # Swallow it on purpose, exactly like a framework that treats any
        # HTTP error as a retryable blip. The run must still fail.
        print("swallowed", e.code)
        sys.exit(0)
    message = body["choices"][0]["message"]
    if message.get("content"):
        print(message["content"])
    messages.append({"role": "tool", "content": '{"id":"KQ311"}'})
"#;

    fn write_fake_agent(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("flowproof-agent-runner");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(name);
        std::fs::write(&path, FAKE_AGENT).expect("write agent");
        path
    }

    fn user(prompt: &str) -> Message {
        Message::new("user", prompt)
    }

    fn cassette(turns: usize) -> Cassette {
        let mut messages = vec![user("Book a flight to Nairobi")];
        let mut out = Vec::new();
        for i in 0..turns {
            let last = i + 1 == turns;
            out.push(Turn {
                protocol: flowproof_trace::cassette::default_protocol(),
                request: TurnRequest {
                    model: "gpt-4o".into(),
                    messages: messages.clone(),
                    tools: vec!["search_flights".into()],
                },
                response: TurnResponse {
                    message: if last {
                        Message::new("assistant", "Booked KQ311.")
                    } else {
                        Message {
                            role: "assistant".into(),
                            content: None,
                            tool_calls: vec![ToolCall {
                                id: "call_1".into(),
                                name: "search_flights".into(),
                                arguments: r#"{"destination":"NBO"}"#.into(),
                            }],
                            tool_call_id: None,
                        }
                    },
                    stop_reason: None,
                },
            });
            messages.push(Message::new("tool", r#"{"id":"KQ311"}"#));
        }
        Cassette { turns: out }
    }

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The whole point: a real process, unmodified, reproduces a recorded
    /// trajectory with no model and no network.
    #[test]
    fn a_real_process_replays_a_trajectory_offline() {
        let agent = write_fake_agent("agent_ok.py");
        let run = run(
            &format!("python3 \"{}\"", agent.display()),
            &env(&[("FAKE_TURNS", "2")]),
            cassette(2),
            Mocks::new(),
            Duration::from_secs(30),
        )
        .expect("runs");

        assert_eq!(run.reproduced(2), Ok(()), "{run:#?}");
        assert_eq!(run.served, 2);
        assert_eq!(run.exit_code, Some(0));
        assert!(run.stdout.contains("Booked KQ311."), "{run:#?}");
    }

    /// The verdict comes from the PROXY, not the exit code. This fake
    /// swallows the 409 and exits 0, which is what a framework treating
    /// HTTP errors as retryable blips does. The run must still fail.
    #[test]
    fn an_agent_that_swallows_a_divergence_still_fails_the_run() {
        let agent = write_fake_agent("agent_drift.py");
        let run = run(
            &format!("python3 \"{}\"", agent.display()),
            &env(&[("FAKE_PROMPT", "Book a flight to Mombasa")]),
            cassette(1),
            Mocks::new(),
            Duration::from_secs(30),
        )
        .expect("runs");

        assert_eq!(run.exit_code, Some(0), "the process reported success");
        assert!(run.stdout.contains("swallowed 409"), "{run:#?}");
        let why = run.reproduced(1).expect_err("the run must not pass");
        assert!(why.contains("content changed"), "{why}");
        assert!(why.starts_with("turn 1:"), "{why}");
    }

    /// An agent that stops early has not reproduced the recording, even
    /// though every call it DID make matched.
    #[test]
    fn stopping_early_is_a_failure_with_both_counts() {
        let agent = write_fake_agent("agent_short.py");
        let run = run(
            &format!("python3 \"{}\"", agent.display()),
            &env(&[("FAKE_TURNS", "1")]),
            cassette(2),
            Mocks::new(),
            Duration::from_secs(30),
        )
        .expect("runs");

        assert_eq!(run.served, 1);
        let why = run.reproduced(2).expect_err("one call is not two");
        assert!(why.contains("made 1 model calls"), "{why}");
        assert!(why.contains("has 2"), "{why}");
    }

    /// A hung agent must not take the suite down with it.
    #[test]
    fn a_hanging_agent_is_killed_at_the_deadline() {
        let dir = std::env::temp_dir().join("flowproof-agent-runner");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("agent_hang.py");
        std::fs::write(&path, "import time\ntime.sleep(600)\n").expect("write");

        let started = Instant::now();
        let run = run(
            &format!("python3 \"{}\"", path.display()),
            &BTreeMap::new(),
            cassette(1),
            Mocks::new(),
            Duration::from_millis(700),
        )
        .expect("runs");

        assert!(run.timed_out, "{run:#?}");
        assert!(run.exit_code.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "killed promptly, took {:?}",
            started.elapsed()
        );
        assert!(run.reproduced(1).is_err());
    }

    #[test]
    fn a_command_that_does_not_exist_says_so_with_the_command() {
        let err = run(
            "definitely-not-a-real-program --go",
            &BTreeMap::new(),
            cassette(1),
            Mocks::new(),
            Duration::from_secs(5),
        )
        .expect_err("cannot spawn");
        let message = err.to_string();
        assert!(
            message.contains("definitely-not-a-real-program"),
            "{message}"
        );
        assert!(message.contains("starting the agent"), "{message}");

        assert!(matches!(
            run(
                "   ",
                &BTreeMap::new(),
                cassette(1),
                Mocks::new(),
                Duration::from_secs(5)
            ),
            Err(RunError::NoCommand)
        ));
    }

    /// The spec's own env wins: it knows its client's spelling better
    /// than this module's list of guesses does.
    #[test]
    fn spec_env_overrides_the_injected_defaults() {
        let dir = std::env::temp_dir().join("flowproof-agent-runner");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("agent_env.py");
        std::fs::write(
            &path,
            "import os\nprint(os.environ['OPENAI_BASE_URL'])\nprint(os.environ['MY_LLM_URL'])\n",
        )
        .expect("write");

        let run = run(
            &format!("python3 \"{}\"", path.display()),
            &env(&[
                ("OPENAI_BASE_URL", "http://overridden.invalid/v1"),
                ("MY_LLM_URL", "http://custom.invalid/v1"),
            ]),
            cassette(1),
            Mocks::new(),
            Duration::from_secs(30),
        )
        .expect("runs");

        assert!(
            run.stdout.contains("http://overridden.invalid/v1"),
            "{run:#?}"
        );
        assert!(run.stdout.contains("http://custom.invalid/v1"), "{run:#?}");
    }

    // ---- http driver ----

    use std::io::{BufRead, Write};
    use std::net::{TcpListener, TcpStream};

    /// Read the `prompt` out of the trigger POST's JSON body, so a fake
    /// service can echo/act on the exact prompt flowproof sent it.
    fn read_prompt(stream: &mut TcpStream) -> String {
        let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
        let mut request_line = String::new();
        reader.read_line(&mut request_line).ok();
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

    /// Answer the trigger connection with a small 200 JSON body.
    fn answer_trigger(stream: &mut TcpStream, body: &str) {
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }

    /// A fake system under test flowproof did NOT start: an HTTP service
    /// that, on the trigger POST, makes `turns` chat-completions calls to the
    /// proxy (exactly as a real SUT would) and then answers the trigger. The
    /// message sequence mirrors `FAKE_AGENT` so it matches `cassette(turns)`.
    fn spawn_fake_service(
        proxy_base: String,
        turns: usize,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind service");
        let addr = listener.local_addr().expect("addr");
        let url = format!("http://{addr}/run");
        let handle = std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let prompt = read_prompt(&mut stream);
            let mut messages = vec![serde_json::json!({"role": "user", "content": prompt})];
            let mut made = 0;
            for _ in 0..turns {
                let payload = serde_json::json!({
                    "model": "gpt-4o",
                    "messages": messages,
                    "tools": [{"type": "function", "function": {"name": "search_flights"}}],
                })
                .to_string();
                match call_proxy(&proxy_base, &payload) {
                    Some(_) => {
                        made += 1;
                        messages.push(
                            serde_json::json!({"role": "tool", "content": r#"{"id":"KQ311"}"#}),
                        );
                    }
                    None => break,
                }
            }
            answer_trigger(&mut stream, &format!("{{\"turns\":{made}}}"));
        });
        (url, handle)
    }

    /// A mispointed service: it answers the trigger but never calls the
    /// proxy, so the trajectory is never reproduced.
    fn spawn_idle_service() -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind service");
        let addr = listener.local_addr().expect("addr");
        let url = format!("http://{addr}/run");
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = read_prompt(&mut stream);
                answer_trigger(&mut stream, "{\"ok\":true}");
            }
        });
        (url, handle)
    }

    /// One chat-completions POST to the proxy over a raw socket, returning
    /// the body on a 200 (so the fake service does not depend on any client's
    /// status-error semantics). `None` on anything but a 200.
    fn call_proxy(base: &str, payload: &str) -> Option<String> {
        let addr = base
            .trim_start_matches("http://")
            .trim_end_matches("/v1")
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
        if status != 200 {
            return None;
        }
        raw.split("\r\n\r\n").nth(1).map(str::to_string)
    }

    /// The http analogue of `a_real_process_replays_a_trajectory_offline`: an
    /// already-running service, driven by a trigger POST, reproduces a
    /// recorded trajectory with no model and no network.
    #[test]
    fn an_http_service_replays_a_trajectory_offline() {
        let proxy = AgentProxy::start(cassette(2), Mocks::new(), 0).expect("proxy");
        let (url, handle) = spawn_fake_service(proxy.base_url(), 2);

        let run = run_http(
            &proxy,
            &url,
            &BTreeMap::new(),
            "Book a flight to Nairobi",
            Duration::from_secs(30),
        )
        .expect("runs");
        handle.join().ok();

        assert_eq!(run.reproduced(2), Ok(()), "{run:#?}");
        assert_eq!(run.served, 2);
        // The trigger's HTTP status is context, landing where an exit code
        // would - never the verdict.
        assert_eq!(run.exit_code, Some(200));
    }

    /// A service pointed nowhere near the proxy makes zero model calls: the
    /// verdict is a reproduction failure (served != expected), not a pass,
    /// and not the trigger's 200.
    #[test]
    fn a_mispointed_http_service_reproduces_nothing() {
        let proxy = AgentProxy::start(cassette(2), Mocks::new(), 0).expect("proxy");
        let (url, handle) = spawn_idle_service();

        let run = run_http(
            &proxy,
            &url,
            &BTreeMap::new(),
            "Book a flight to Nairobi",
            Duration::from_secs(30),
        )
        .expect("runs");
        handle.join().ok();

        assert_eq!(run.served, 0);
        assert_eq!(run.exit_code, Some(200), "the trigger itself succeeded");
        let why = run.reproduced(2).expect_err("zero calls is not two");
        assert!(why.contains("made 0 model calls"), "{why}");
    }

    /// A service that cannot be reached is a SETUP error (the http analogue
    /// of a spawn failure), never a verdict: naming the url and asking if it
    /// is running.
    #[test]
    fn an_unreachable_service_is_a_named_setup_error() {
        let proxy = AgentProxy::start(cassette(1), Mocks::new(), 0).expect("proxy");
        // Port 9 (discard) refuses the connection.
        let err = run_http(
            &proxy,
            "http://127.0.0.1:9/run",
            &BTreeMap::new(),
            "hi",
            Duration::from_secs(5),
        )
        .expect_err("cannot reach");
        assert!(matches!(err, RunError::Unreachable { .. }), "{err:?}");
        assert!(err.to_string().contains("could not reach"), "{err}");
        assert!(err.to_string().contains("127.0.0.1:9"), "{err}");
    }

    /// An empty url is a spec-is-code error, exactly like an empty command.
    #[test]
    fn a_blank_url_is_rejected() {
        let proxy = AgentProxy::start(cassette(1), Mocks::new(), 0).expect("proxy");
        assert!(matches!(
            run_http(
                &proxy,
                "   ",
                &BTreeMap::new(),
                "hi",
                Duration::from_secs(5)
            ),
            Err(RunError::NoUrl)
        ));
    }
}
