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
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use flowproof_trace::cassette::{Cassette, Divergence};
use flowproof_trace::substitution::Mocks;

use crate::agent_proxy::{AgentProxy, ProxyError};
use crate::egress::{AllowSet, Containment, EgressLog};
use crate::fs_observe::FsLog;

/// The environment variables an OpenAI-compatible client reads for its
/// base URL. All of them are set, because the system under test picks one
/// and a spec author should not have to know which.
///
/// `OPENAI_API_KEY` is set to a placeholder for the same reason: a client
/// that refuses to start without a key would fail before reaching the
/// proxy, and there is no real key to leak because there is no real
/// upstream.
const BASE_URL_VARS: [&str; 3] = ["OPENAI_BASE_URL", "OPENAI_API_BASE", "OPENAI_BASE"];

/// Every variable that can carry a REAL model credential, and which the agent
/// must therefore never see.
///
/// The agent is third-party code. flowproof holds the upstream key and attaches
/// it only on the outbound hop, so the agent is handed a placeholder — that was
/// always the design for `OPENAI_API_KEY` and `ANTHROPIC_API_KEY`, which are
/// overwritten below.
///
/// What was missing is that overwriting two names is not the same as masking
/// the credential. `Command` inherits the parent environment, and the *first*
/// name `flowproof record` looks in is `FLOWPROOF_AGENT_KEY` — so the
/// recommended way to supply the key was also the one way it reached the agent
/// verbatim. `FLOWPROOF_AI_API_KEY`, which the LLM-authoring backend reads, did
/// the same.
///
/// Removed rather than placeheld: a variable flowproof does not hand out has no
/// business having a value, and an agent reading one is asking a question it
/// should not get an answer to.
pub const CREDENTIAL_VARS: [&str; 4] = [
    "FLOWPROOF_AGENT_KEY",
    "FLOWPROOF_AI_API_KEY",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
];

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

/// How the system under test was driven, which is what makes `exit_code`
/// readable: flowproof either started a process (and `exit_code` is its exit
/// status) or POSTed to a service it did not start (and `exit_code` is the
/// trigger's HTTP status). Without the distinction a failure that wants to
/// say "the process exited 1" would also say "the process exited 200" for an
/// http trigger that answered perfectly well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// flowproof spawned the agent; `exit_code` is the process exit status
    /// and `stderr` is what it printed.
    Process,
    /// flowproof POSTed to an already-running service; `exit_code` is the
    /// trigger's HTTP status and there is no stderr to capture.
    Http,
}

/// What a run produced.
#[derive(Debug)]
pub struct AgentRun {
    /// Model calls the proxy served from the recording.
    pub served: usize,
    /// The first divergence, if the trajectory left its recording.
    pub divergence: Option<Divergence>,
    /// How the system under test was driven, and therefore how `exit_code`
    /// reads.
    pub trigger: Trigger,
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
    /// What the run DID to the filesystem, observed by the same seccomp
    /// filter and never prevented by it. Empty wherever there is no
    /// mechanism, which is everywhere the egress log is empty for the same
    /// reason: the traps are installed together or not at all.
    pub fs: FsLog,
    /// Whether the seccomp observation mechanism ran: true only on the
    /// Linux contained path. Distinct from `containment` - observing side
    /// effects is not containing egress - and load-bearing for the trace's
    /// side-effect lane, ABSENT when this is false: an unobserved run's
    /// empty `fs` is silence, not evidence.
    pub observed: bool,
    /// The containment tier this RUN achieved, when the run itself is what
    /// decides it.
    ///
    /// `None` means "ask the spec" - an uncontained path, a `url:` service, a
    /// flow that engages no egress. `Some` means the run determined its own
    /// tier and that answer wins, because a probe taken beforehand can only
    /// say what a host COULD do.
    ///
    /// On Linux the two agree by construction: the seccomp filter installs in
    /// the child's `pre_exec`, so reaching a finished run means it installed.
    /// Windows has several steps that can fail after a probe says yes, which
    /// is why this field exists at all - reporting the probe's answer there
    /// would let a run whose filters never installed still say "enforced".
    pub containment: Option<Containment>,
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
            return Err(self.with_stderr(format!(
                "the agent did not finish in time; it made {} of {expected_turns} model calls",
                self.served
            )));
        }
        // Zero served is its own failure, not a small case of "fewer than
        // recorded". "the agent made 0 model calls" is a symptom that reads
        // as *flowproof could not replay*, when the truth is usually *the
        // agent never started* - and the process said exactly why on a
        // stderr that used to be captured and then thrown away.
        if self.served == 0 && expected_turns > 0 {
            // Diagnosis, then the evidence, then what to do about it - the
            // hint goes LAST because the stderr is usually the whole answer.
            let mut out = self.with_stderr(self.never_called(expected_turns));
            if let Some(hint) = self.start_hint() {
                out.push_str("\n  hint: ");
                out.push_str(hint);
            }
            return Err(out);
        }
        if self.served != expected_turns {
            return Err(format!(
                "the agent made {} model calls, the recording has {expected_turns}",
                self.served
            ));
        }
        Ok(())
    }

    /// The message for a run where nothing reached the proxy at all.
    ///
    /// A process that exited non-zero is a DIFFERENT diagnosis from one that
    /// ran to completion without calling a model: the first died (a missing
    /// dependency, a bad argument, an unset variable of its own), the second
    /// is the wiring failure the http hint and `flowproof doctor` address.
    /// Only the process driver may talk about an exit code, because an http
    /// trigger's `exit_code` is an HTTP status.
    fn never_called(&self, expected_turns: usize) -> String {
        let recorded = format!("the recording has {expected_turns}");
        match (self.trigger, self.exit_code) {
            (Trigger::Process, Some(code)) if code != 0 => {
                format!("the agent process exited {code} without making any model call; {recorded}")
            }
            (Trigger::Process, None) => {
                format!("the agent process ended without making any model call; {recorded}")
            }
            _ => format!("the agent made 0 model calls, {recorded}"),
        }
    }

    /// What to do about a dead agent: flowproof started the command the spec
    /// gave it and nothing else, so the reproduction is one shell line away -
    /// and an agent's own dependencies remain its own.
    fn start_hint(&self) -> Option<&'static str> {
        match (self.trigger, self.exit_code) {
            (Trigger::Process, Some(code)) if code != 0 => Some(
                "flowproof runs `agent.command` exactly as written - run that command \
                 yourself to see the same failure, and check the agent's own dependencies \
                 are installed.",
            ),
            _ => None,
        }
    }

    /// Append what the agent printed on its way out.
    ///
    /// The last lines are the ones that explain the failure - a traceback
    /// names its cause on the final line - so a chatty agent is tailed rather
    /// than dropped or dumped whole.
    fn with_stderr(&self, message: String) -> String {
        let lines: Vec<&str> = self
            .stderr
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty())
            .collect();
        if lines.is_empty() {
            return message;
        }
        let skipped = lines.len().saturating_sub(STDERR_TAIL_LINES);
        let mut out = format!("{message}\n  agent stderr:");
        if skipped > 0 {
            out.push_str(&format!("\n    ... {skipped} earlier line(s) omitted"));
        }
        for line in &lines[skipped..] {
            out.push_str(&format!("\n    {line}"));
        }
        out
    }
}

/// How many trailing stderr lines a failure message carries. Enough for a
/// Python traceback to arrive with its cause attached, short enough that an
/// agent logging every token cannot bury the verdict.
const STDERR_TAIL_LINES: usize = 20;

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
        trigger: Trigger::Http,
        exit_code,
        timed_out,
        stdout,
        stderr: String::new(),
        upstream_error: log.upstream_error.clone(),
        egress: EgressLog::default(),
        fs: FsLog::default(),
        observed: false,
        containment: None,
    };
    drop(log);
    Ok(run)
}

/// The variables flowproof sets on an agent, over an inherited environment.
///
/// Extracted from [`configure`] because Windows cannot use it. A contained
/// child there is launched with an explicit environment BLOCK rather than a
/// `Command`, so the same decisions have to be expressible as data: which
/// names to set, and which to leave out.
///
/// The credential names are NOT in the returned map. `Command::env_remove`
/// has no equivalent in a block — a block is a whole environment, not a delta
/// — so "remove" becomes "do not inherit", and the caller is told which names
/// to drop rather than being handed a removal it cannot perform.
pub fn agent_env(base: &str, env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for var in BASE_URL_VARS {
        out.insert(var.to_string(), base.to_string());
    }
    // Placeholders, so a client that refuses to start without a key still
    // reaches the proxy. There is no real upstream for it to leak to.
    out.insert(
        "OPENAI_API_KEY".into(),
        "flowproof-replay-no-key-needed".into(),
    );
    // The Anthropic SDK appends `/v1/messages` itself, so it wants the origin
    // WITHOUT the `/v1` the OpenAI vars keep.
    out.insert("ANTHROPIC_BASE_URL".into(), anthropic_base(base));
    out.insert(
        "ANTHROPIC_API_KEY".into(),
        "flowproof-replay-no-key-needed".into(),
    );
    // `${FLOWPROOF_LLM_PROXY}` is the documented handle for clients that take
    // the base URL as an argument rather than an env var.
    out.insert("FLOWPROOF_LLM_PROXY".into(), base.to_string());
    // The spec's own env goes LAST so a flow can override any of the above; it
    // knows its client better than this module does. Values may reference
    // runtime handles the spec could not know when it was written, because the
    // proxy binds an ephemeral port.
    for (key, value) in env {
        out.insert(key.clone(), substitute_runtime_handles(value, base, env));
    }
    out
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
    // Drop every credential name BEFORE handing out placeholders. `Command`
    // inherits this process's environment, so a name flowproof does not
    // explicitly set is passed through — and the record path runs with a real
    // key in exactly these variables.
    for var in CREDENTIAL_VARS {
        child.env_remove(var);
    }
    for (key, value) in agent_env(base, env) {
        child.env(key, value);
    }
    Ok(child)
}

/// Replace the `${flowproof.*}` handles with values only known at spawn time.
///
/// - `${flowproof.proxy_url}` - the model proxy, `/v1` included, the form
///   OpenAI-compatible clients expect.
/// - `${flowproof.proxy_url_no_v1}` - the same origin without `/v1`, for a
///   client that appends its own path (the Anthropic SDK does).
/// - `${flowproof.mcp_url.<name>}` - the stand-in URL for that MCP server,
///   for a client that builds its endpoints from a base rather than taking
///   one URL per server.
///
/// Anything else is left untouched: an unknown handle is far more likely to
/// be a value that happens to look like one than a typo worth failing on,
/// and `${VAR}` secret refs have already been resolved by this point.
fn substitute_runtime_handles(value: &str, base: &str, env: &BTreeMap<String, String>) -> String {
    if !value.contains("${flowproof.") {
        return value.to_string();
    }
    let mut out = value
        .replace("${flowproof.proxy_url_no_v1}", &anthropic_base(base))
        .replace("${flowproof.proxy_url}", base);
    // The MCP stand-in URLs are already in this same map, injected as
    // `FLOWPROOF_MCP_URL_<NAME>`; this is a friendlier spelling of the same
    // value, so the two can never disagree.
    for (key, url) in env {
        if let Some(name) = key.strip_prefix("FLOWPROOF_MCP_URL_") {
            out = out.replace(&format!("${{flowproof.mcp_url.{name}}}"), url);
            // Server names are lowercase in the spec; the env var is upper.
            out = out.replace(
                &format!("${{flowproof.mcp_url.{}}}", name.to_ascii_lowercase()),
                url,
            );
        }
    }
    out
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

/// How long to keep draining a pipe after the child is gone.
///
/// Only reached when the write end outlived the child (see [`PipeDrain`]);
/// a normal exit closes the pipe and the drain finishes at once.
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(5);

/// A child pipe being drained on its own thread.
///
/// TWO BUGS LIVE WHERE THIS USED TO BE A BLOCKING `read_to_string` AFTER
/// THE WAIT, and neither is guessable from the old four-line body:
///
/// 1. **The wait deadlocked against the pipe buffer.** Draining only after
///    the child exits means a child that writes more than the OS pipe
///    buffer (~64 KB) blocks in `write` forever, never exits, and is then
///    killed at the timeout - reported as a hung agent when it was really
///    a full pipe. Draining starts at spawn now, so the child always has
///    somewhere to write.
///
/// 2. **`read_to_string` waited for EOF, which a GRANDCHILD can withhold.**
///    EOF arrives when the last write end closes, not when the child dies.
///    An agent that spawns its own server (`opencode serve` under the
///    OpenCode SDK) hands that grandchild the same stdout, so killing the
///    child at the timeout closed nothing and the read blocked forever. A
///    flowproof run against DataMaker's agent suite sat for 26 minutes on a
///    300-second timeout, produced no output, and had to be killed by hand.
///    That is the failure the deadline above says it prevents: "an agent
///    that hangs ... would otherwise take the whole suite down with it".
///
/// So the drain is bounded and the partial output is kept: whatever arrived
/// before the grace expired is returned rather than discarded, because for
/// a run that already went wrong that text is the only diagnostic there is.
struct PipeDrain {
    buffer: Arc<Mutex<String>>,
    finished: mpsc::Receiver<()>,
}

impl PipeDrain {
    /// Start draining `pipe` immediately, before the caller waits on the child.
    fn start<R: Read + Send + 'static>(pipe: Option<R>) -> Self {
        let buffer = Arc::new(Mutex::new(String::new()));
        let (done, finished) = mpsc::channel();
        let sink = Arc::clone(&buffer);
        std::thread::spawn(move || {
            if let Some(mut pipe) = pipe {
                // Chunked rather than `read_to_string` so a partial read is
                // still visible in `buffer` when the grace expires.
                let mut chunk = [0u8; 8192];
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut sink) = sink.lock() {
                                sink.push_str(&String::from_utf8_lossy(&chunk[..n]));
                            }
                        }
                    }
                }
            }
            let _ = done.send(());
        });
        Self { buffer, finished }
    }

    /// Take what was drained, waiting at most `grace` for the pipe to close.
    ///
    /// The reader thread is deliberately left running when the grace expires:
    /// it is blocked on a pipe held open by a process flowproof does not own,
    /// it holds nothing but its own buffer, and the process exits shortly
    /// after. Killing it is not possible in safe Rust and not worth it.
    fn collect(self, grace: Duration) -> String {
        let _ = self.finished.recv_timeout(grace);
        self.buffer
            .lock()
            .map(|buffer| buffer.clone())
            .unwrap_or_default()
    }
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

    // Drain BEFORE waiting, not after: a child that fills the pipe buffer
    // blocks in `write` and never reaches the exit the wait is waiting for.
    let out_drain = PipeDrain::start(child.stdout.take());
    let err_drain = PipeDrain::start(child.stderr.take());
    let (status, timed_out) = wait_to_deadline(&mut child, timeout);
    let stdout = out_drain.collect(PIPE_DRAIN_GRACE);
    let stderr = err_drain.collect(PIPE_DRAIN_GRACE);

    let log = proxy.log();
    let run = AgentRun {
        served: log.served,
        divergence: log.divergence.clone(),
        trigger: Trigger::Process,
        exit_code: status.and_then(|s| s.code()),
        timed_out,
        stdout,
        stderr,
        upstream_error: log.upstream_error.clone(),
        egress: EgressLog::default(),
        fs: FsLog::default(),
        observed: false,
        containment: None,
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
///
/// `egress_engaged` says whether the FLOW declared an egress policy, or is
/// supervised for side-effect observation only under an allow-all set nobody
/// declared - and the latter must never report `Enforced`.
#[cfg(target_os = "linux")]
pub fn run_against_contained(
    proxy: &AgentProxy,
    command: &str,
    env: &BTreeMap<String, String>,
    timeout: Duration,
    allow: &AllowSet,
    egress_engaged: bool,
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
    // Start the supervisor: collect the notify fd the handoff thread acquired
    // out of the child while `spawn` ran, and service it for the run.
    let supervisor = prep
        .into_supervisor(spawned)
        .map_err(|source| RunError::Spawn {
            command: command.trim().to_string(),
            source,
        })?;

    // Same ordering as the uncontained path: drain from spawn, so the pipe
    // buffer can never be what stops the child from exiting.
    let out_drain = PipeDrain::start(child.stdout.take());
    let err_drain = PipeDrain::start(child.stderr.take());
    let (status, timed_out) = wait_to_deadline(&mut child, timeout);
    let stdout = out_drain.collect(PIPE_DRAIN_GRACE);
    let stderr = err_drain.collect(PIPE_DRAIN_GRACE);
    let (egress, fs) = supervisor.stop_and_collect();

    let log = proxy.log();
    let run = AgentRun {
        served: log.served,
        divergence: log.divergence.clone(),
        trigger: Trigger::Process,
        exit_code: status.and_then(|s| s.code()),
        timed_out,
        stdout,
        stderr,
        upstream_error: log.upstream_error.clone(),
        egress,
        fs,
        // The filter that enforced is the filter that watched, so `fs`
        // above is evidence here and silence everywhere else.
        observed: true,
        containment: Some(if egress_engaged {
            // Reaching here means the filter installed (it goes in via
            // `pre_exec`; a failure aborts the spawn) - and it enforced the
            // DECLARED policy.
            Containment::Enforced
        } else {
            // An allow-all policy nobody declared: observing side effects is
            // not containing egress, and the tier must not blur the two.
            Containment::observation_only()
        }),
    };
    drop(log);
    Ok(run)
}

/// Windows: a per-run identity behind WFP filters scoped to it.
///
/// Unlike Linux this cannot reuse `run_against`'s machinery at all — the child
/// runs as a DIFFERENT user, so `std::process::Command` cannot start it and
/// its pipes cannot reach it. `egress_windows::run` does the whole sequence
/// and reports what it ACHIEVED, which is why the tier travels back on the run
/// rather than being predicted by a probe.
#[cfg(windows)]
pub fn run_against_contained(
    proxy: &AgentProxy,
    command: &str,
    env: &BTreeMap<String, String>,
    timeout: Duration,
    allow: &AllowSet,
    egress_engaged: bool,
) -> Result<AgentRun, RunError> {
    // Defense in depth: no caller passes `false` here today, but a future
    // one must get the plain path, never WFP filters from a wildcard set.
    if !egress_engaged {
        return run_against(proxy, command, env, timeout);
    }
    let command = command.trim();
    if command.is_empty() {
        return Err(RunError::NoCommand);
    }
    let base = proxy.base_url();
    let outcome = crate::egress_windows::run::run_contained(
        command,
        &agent_env(&base, env),
        &CREDENTIAL_VARS,
        allow.entries(),
        timeout,
    );

    let log = proxy.log();
    let run = AgentRun {
        served: log.served,
        divergence: log.divergence.clone(),
        trigger: Trigger::Process,
        exit_code: outcome.exit_code,
        // `None` from the wait means the deadline passed, which is the same
        // thing `wait_to_deadline` reports as a timeout on the other paths.
        timed_out: outcome.exit_code.is_none(),
        stdout: outcome.stdout,
        stderr: outcome.stderr,
        upstream_error: log.upstream_error.clone(),
        egress: EgressLog {
            blocked: outcome.blocked,
            faults: outcome.faults,
            observed: Vec::new(),
        },
        // Filesystem observation is a seccomp mechanism; Windows has none.
        fs: FsLog::default(),
        observed: false,
        containment: Some(match outcome.not_contained {
            None => Containment::Enforced,
            Some(why) => Containment::NotContained(why),
        }),
    };
    drop(log);
    Ok(run)
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn run_against_contained(
    proxy: &AgentProxy,
    command: &str,
    env: &BTreeMap<String, String>,
    timeout: Duration,
    _allow: &AllowSet,
    egress_engaged: bool,
) -> Result<AgentRun, RunError> {
    // Defense in depth, mirroring the Windows variant.
    if !egress_engaged {
        return run_against(proxy, command, env, timeout);
    }
    // No mechanism here; the plain path, and the tier says "not contained".
    run_against(proxy, command, env, timeout)
}

#[cfg(test)]
mod tests {
    /// The 26-minute hang, reduced to its cause.
    ///
    /// A backgrounded `sleep` inherits the shell's stdout and holds the write
    /// end open long after the shell itself exits - structurally the same
    /// thing `opencode serve` does when an agent SDK spawns it. EOF therefore
    /// never arrives, and the `read_to_string` this replaced waited for EOF,
    /// so the drain outlived the process by however long the grandchild ran.
    #[cfg(unix)]
    #[test]
    fn a_grandchild_holding_the_pipe_cannot_outlast_the_grace() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30 & echo hi")
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn");
        let drain = PipeDrain::start(child.stdout.take());
        let _ = child.wait();

        let started = Instant::now();
        let out = drain.collect(Duration::from_millis(500));

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the drain must be bounded by the grace, not by the grandchild; took {:?}",
            started.elapsed()
        );
        // Bounded must not mean lossy: what did arrive is the only diagnostic
        // a hung run leaves behind.
        assert!(out.contains("hi"), "partial output must survive: {out:?}");
    }

    /// The second bug in the same place: draining only AFTER the wait means a
    /// child that outwrites the OS pipe buffer (~64 KB) blocks in `write`,
    /// never exits, and is killed at the timeout - reported as a hung agent
    /// when nothing was wrong with it. Draining from spawn keeps it moving.
    #[cfg(unix)]
    #[test]
    fn a_child_that_outwrites_the_pipe_buffer_still_exits() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("yes flowproof | head -c 200000")
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn");
        let drain = PipeDrain::start(child.stdout.take());
        let (_status, timed_out) = wait_to_deadline(&mut child, Duration::from_secs(20));
        let out = drain.collect(PIPE_DRAIN_GRACE);

        assert!(!timed_out, "a chatty child must not look like a hung one");
        assert_eq!(out.len(), 200_000, "every byte the child wrote is captured");
    }

    /// What `configure` decided for one variable: `Some` to set it, `None`
    /// if it is explicitly removed, and `None` from the outer option if it
    /// was never mentioned — which, since `Command` inherits, means the
    /// child gets whatever this process has.
    fn decided(cmd: &Command, key: &str) -> Option<Option<String>> {
        cmd.get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(key))
            .map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
    }

    /// The agent is third-party code and flowproof holds the real key, so the
    /// key must not be in the agent's environment. Two names were: the record
    /// path reads `FLOWPROOF_AGENT_KEY` FIRST, so the recommended way to
    /// supply the credential was also the one way it reached the agent intact.
    ///
    /// Asserted on the `Command` rather than by spawning. Proving it by
    /// spawning means putting a real value in this process's environment, and
    /// `set_var` is process-global — with tests running in parallel that is a
    /// race, which is exactly what #236 was. `env_remove` guarantees the child
    /// sees nothing regardless of the parent, so reading the decision is both
    /// deterministic and a stronger claim than one spawn happening to be clean.
    #[test]
    fn a_real_credential_is_never_handed_to_the_agent() {
        let cmd =
            configure("echo hi", "http://127.0.0.1:9/v1", &BTreeMap::new()).expect("configure");
        for var in ["FLOWPROOF_AGENT_KEY", "FLOWPROOF_AI_API_KEY"] {
            assert_eq!(
                decided(&cmd, var),
                Some(None),
                "{var} must be REMOVED from the agent's environment; \
                 inheriting it hands third-party code the real upstream key"
            );
        }
    }

    /// The other direction, and it is not implied by the one above: a client
    /// that refuses to start without a key must still get one, or masking the
    /// credential would break every agent it protects. Placeholders, not
    /// removal, for exactly these two.
    #[test]
    fn the_agent_still_gets_a_key_shaped_placeholder() {
        let cmd =
            configure("echo hi", "http://127.0.0.1:9/v1", &BTreeMap::new()).expect("configure");
        for var in ["OPENAI_API_KEY", "ANTHROPIC_API_KEY"] {
            assert_eq!(
                decided(&cmd, var),
                Some(Some("flowproof-replay-no-key-needed".to_string())),
                "{var} must be a placeholder: present, and worthless"
            );
        }
    }

    /// Every name that can hold a real credential has to be covered, not just
    /// the two that were noticed. This fails when someone adds a fifth.
    #[test]
    fn every_credential_variable_is_accounted_for() {
        let cmd =
            configure("echo hi", "http://127.0.0.1:9/v1", &BTreeMap::new()).expect("configure");
        for var in CREDENTIAL_VARS {
            let d = decided(&cmd, var);
            assert!(
                d == Some(None) || d == Some(Some("flowproof-replay-no-key-needed".to_string())),
                "{var} is in CREDENTIAL_VARS but configure() neither removes it \
                 nor replaces it with the placeholder; it would be inherited"
            );
        }
    }

    /// The same claim for the map the Windows path uses. A `Command` gets
    /// removal and placeholders as two separate operations; an environment
    /// BLOCK gets one map plus an exclusion list, and this asserts the map's
    /// half — a credential name is in it only as the placeholder.
    ///
    /// Worth its own test because the two paths could drift: adding a fifth
    /// credential name to `CREDENTIAL_VARS` keeps `configure` correct for free
    /// (it iterates the constant) but would silently leave the map naming a
    /// real key if someone also added it to `agent_env`.
    #[test]
    fn the_windows_environment_map_names_no_real_credential() {
        let env = agent_env("http://127.0.0.1:9/v1", &BTreeMap::new());
        for var in CREDENTIAL_VARS {
            match env.get(var) {
                None => {}
                Some(v) => assert_eq!(
                    v, "flowproof-replay-no-key-needed",
                    "{var} is in the map handed to the contained child, and it is \
                     not the placeholder"
                ),
            }
        }
        // And the two that MUST be present still are, or a client that
        // refuses to start without a key never reaches the proxy.
        assert!(env.contains_key("OPENAI_API_KEY"));
        assert!(env.contains_key("ANTHROPIC_API_KEY"));
    }

    /// A spec that deliberately passes a credential through — `agent.env` with
    /// `ANTHROPIC_API_KEY: ${SECRET}` — is a documented escape hatch, and spec
    /// env is applied last precisely so it wins. Masking must not quietly take
    /// that away; an adopter relying on it would see their agent lose its key
    /// with no error.
    #[test]
    fn a_spec_can_still_pass_a_key_through_on_purpose() {
        let mut env = BTreeMap::new();
        env.insert("ANTHROPIC_API_KEY".to_string(), "sk-deliberate".to_string());
        let cmd = configure("echo hi", "http://127.0.0.1:9/v1", &env).expect("configure");
        assert_eq!(
            decided(&cmd, "ANTHROPIC_API_KEY"),
            Some(Some("sk-deliberate".to_string())),
            "the spec's own env is applied last and must still override"
        );
    }

    /// The gap a real adopter hit: their client reads AI_GATEWAY_URL, and
    /// the proxy's port is not known when the spec is written, so a static
    /// `agent.env` value could not reach it.
    #[test]
    fn the_proxy_url_is_addressable_from_a_spec_env_value() {
        let env = BTreeMap::new();
        let got =
            substitute_runtime_handles("${flowproof.proxy_url}", "http://127.0.0.1:51234/v1", &env);
        assert_eq!(got, "http://127.0.0.1:51234/v1");
    }

    /// Some clients append their own `/v1`, so handing them the suffixed
    /// form produces `/v1/v1`. The no-suffix handle is for those.
    #[test]
    fn the_no_v1_handle_strips_the_suffix() {
        let env = BTreeMap::new();
        let got = substitute_runtime_handles(
            "${flowproof.proxy_url_no_v1}",
            "http://127.0.0.1:51234/v1",
            &env,
        );
        assert_eq!(got, "http://127.0.0.1:51234");
    }

    /// A client that builds several endpoints off one base (`<base>/mcp`,
    /// `<base>/mcp-exec/sap`) has no per-server variable to override, so it
    /// needs the stand-in's base by name.
    #[test]
    fn an_mcp_stand_in_is_addressable_by_server_name() {
        let mut env = BTreeMap::new();
        env.insert(
            "FLOWPROOF_MCP_URL_DATAMAKER_EXEC".to_string(),
            "http://127.0.0.1:44100/mcp".to_string(),
        );
        // Upper (as the var is spelled) and lower (as the spec names it).
        for handle in [
            "${flowproof.mcp_url.DATAMAKER_EXEC}",
            "${flowproof.mcp_url.datamaker_exec}",
        ] {
            let got = substitute_runtime_handles(handle, "http://127.0.0.1:1/v1", &env);
            assert_eq!(got, "http://127.0.0.1:44100/mcp", "{handle}");
        }
    }

    /// Handles interpolate INTO a larger value, because that is how a base
    /// URL is actually used.
    #[test]
    fn a_handle_substitutes_inside_a_longer_value() {
        let env = BTreeMap::new();
        let got = substitute_runtime_handles(
            "${flowproof.proxy_url_no_v1}/custom/path",
            "http://127.0.0.1:9/v1",
            &env,
        );
        assert_eq!(got, "http://127.0.0.1:9/custom/path");
    }

    /// A value that is not a handle must survive untouched - including one
    /// that merely looks shell-ish. Failing on an unknown handle would be
    /// worse than passing it through.
    #[test]
    fn ordinary_values_and_unknown_handles_pass_through() {
        let env = BTreeMap::new();
        for value in ["plain", "$HOME/x", "${flowproof.not_a_handle}"] {
            assert_eq!(
                substitute_runtime_handles(value, "http://127.0.0.1:1/v1", &env),
                value
            );
        }
    }
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

    /// An agent that dies before it can talk to anything is the adoption
    /// failure of issue #188: on a machine missing the agent's OWN dependency
    /// the run used to report "0 model calls", which reads as a flowproof
    /// replay failure, while the traceback that explained everything was
    /// captured and then discarded. The message must name the dead process
    /// and carry its stderr.
    #[test]
    fn an_agent_that_never_starts_says_so_and_shows_its_stderr() {
        let dir = std::env::temp_dir().join("flowproof-agent-runner");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("agent_missing_dep.py");
        std::fs::write(&path, "import definitely_not_installed_pkg\n").expect("write");

        let run = run(
            &format!("python3 \"{}\"", path.display()),
            &BTreeMap::new(),
            cassette(2),
            Mocks::new(),
            Duration::from_secs(30),
        )
        .expect("runs");

        assert_eq!(run.served, 0);
        assert_eq!(run.exit_code, Some(1), "python exits 1 on an import error");
        let why = run
            .reproduced(2)
            .expect_err("a dead agent reproduced nothing");
        assert!(
            why.contains("exited 1 without making any model call"),
            "names the failure mode, not the symptom: {why}"
        );
        assert!(why.contains("the recording has 2"), "{why}");
        assert!(
            why.contains("definitely_not_installed_pkg"),
            "the stderr that explains it must survive: {why}"
        );
        assert!(why.contains("agent stderr:"), "{why}");
        assert!(
            why.find("agent stderr:") < why.find("hint:"),
            "the evidence comes before the advice: {why}"
        );
    }

    /// The other half of the same fork: an agent that ran to completion and
    /// simply never called a model is a WIRING failure, not a dead process,
    /// so it must not be told it "exited 0 without making any model call".
    #[test]
    fn an_agent_that_exits_clean_without_calling_is_not_reported_as_dead() {
        let dir = std::env::temp_dir().join("flowproof-agent-runner");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("agent_quiet.py");
        std::fs::write(&path, "print('did nothing')\n").expect("write");

        let run = run(
            &format!("python3 \"{}\"", path.display()),
            &BTreeMap::new(),
            cassette(1),
            Mocks::new(),
            Duration::from_secs(30),
        )
        .expect("runs");

        assert_eq!(run.exit_code, Some(0));
        let why = run.reproduced(1).expect_err("zero calls is not one");
        assert!(why.contains("made 0 model calls"), "{why}");
        assert!(!why.contains("exited"), "nothing died here: {why}");
    }

    /// A chatty agent must not bury the verdict: the stderr is TAILED, and
    /// the message says how much it dropped rather than pretending it had
    /// everything.
    #[test]
    fn a_flood_of_stderr_is_tailed_not_dumped() {
        let dir = std::env::temp_dir().join("flowproof-agent-runner");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("agent_chatty.py");
        std::fs::write(
            &path,
            "import sys\n\
             for i in range(200):\n\
             \x20   print(f'noise {i}', file=sys.stderr)\n\
             sys.exit(3)\n",
        )
        .expect("write");

        let run = run(
            &format!("python3 \"{}\"", path.display()),
            &BTreeMap::new(),
            cassette(1),
            Mocks::new(),
            Duration::from_secs(30),
        )
        .expect("runs");

        let why = run
            .reproduced(1)
            .expect_err("a dead agent reproduced nothing");
        assert!(why.contains("exited 3"), "{why}");
        assert!(why.contains("noise 199"), "the last line survives: {why}");
        assert!(!why.contains("noise 0\n"), "the flood does not: {why}");
        assert!(why.contains("earlier line(s) omitted"), "{why}");
        assert!(
            why.lines().count() < 30,
            "the message stays readable: {why}"
        );
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
        // An http trigger's `exit_code` is a STATUS. The zero-call message
        // may never spend it as if it were a process exit code, or a service
        // that answered perfectly would be reported as having "exited 200".
        assert!(!why.contains("exited"), "{why}");
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
