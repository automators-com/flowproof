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

use crate::agent_proxy::AgentProxy;

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
fn argv(command: &str) -> Vec<String> {
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
    Proxy(std::io::Error),
    #[error("a spec is code, but an empty command is not: `agent.command` is blank")]
    NoCommand,
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
    let proxy = AgentProxy::start(cassette, mocks).map_err(RunError::Proxy)?;
    run_against(&proxy, command, env, timeout)
}

/// Spawn the agent against an ALREADY-STARTED proxy and wait for it to
/// finish. The orchestration uses this to drive a RECORD proxy (which
/// forwards to a real model) as easily as a replay one - the process does
/// not know or care which mode the endpoint it was handed is in.
pub fn run_against(
    proxy: &AgentProxy,
    command: &str,
    env: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<AgentRun, RunError> {
    let command = command.trim();
    if command.is_empty() {
        return Err(RunError::NoCommand);
    }
    let base = proxy.base_url();

    let parts = argv(command);
    let (program, args) = parts.split_first().ok_or(RunError::NoCommand)?;

    let mut child = Command::new(program);
    child
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for var in BASE_URL_VARS {
        child.env(var, &base);
    }
    child.env("OPENAI_API_KEY", "flowproof-replay-no-key-needed");
    // The Anthropic SDK reads its own base URL and appends `/v1/messages`
    // itself, so it wants the origin WITHOUT the `/v1` the OpenAI vars keep;
    // hand it the suffix-free form. A placeholder key for the same reason
    // OPENAI_API_KEY gets one: a client that refuses to start without a key
    // must still reach the proxy, and there is no real upstream to leak to.
    child.env("ANTHROPIC_BASE_URL", anthropic_base(&base));
    child.env("ANTHROPIC_API_KEY", "flowproof-replay-no-key-needed");
    // `${FLOWPROOF_LLM_PROXY}` is the documented handle for the base URL,
    // for clients that take it as an argument rather than an env var.
    child.env("FLOWPROOF_LLM_PROXY", &base);
    // The spec's own env goes on LAST so a flow can override any of the
    // above; it knows its client better than this module does.
    for (key, value) in env {
        child.env(key, value);
    }

    let mut child = child.spawn().map_err(|source| RunError::Spawn {
        command: command.to_string(),
        source,
    })?;

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

    let read = |pipe: Option<&mut dyn Read>| {
        let mut buffer = String::new();
        if let Some(pipe) = pipe {
            let _ = pipe.read_to_string(&mut buffer);
        }
        buffer
    };
    let stdout = read(child.stdout.as_mut().map(|p| p as &mut dyn Read));
    let stderr = read(child.stderr.as_mut().map(|p| p as &mut dyn Read));

    let log = proxy.log();
    let run = AgentRun {
        served: log.served,
        divergence: log.divergence.clone(),
        exit_code: status.and_then(|s| s.code()),
        timed_out,
        stdout,
        stderr,
        upstream_error: log.upstream_error.clone(),
    };
    drop(log);
    Ok(run)
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
}
