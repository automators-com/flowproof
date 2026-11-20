//! The user's own coding agent as the model: Claude Code or Codex, spoken to
//! over the Agent Client Protocol (ACP, agentclientprotocol.com) on its
//! stdin/stdout.
//!
//! This is how a Claude or ChatGPT subscription drives flowproof without an
//! API key. flowproof never holds a credential: it starts the agent's ACP
//! adapter, the adapter runs the agent under the login the user already has,
//! and each `complete` is one prompt turn in a fresh session.
//!
//! The agent is used as a model, not as an agent. It starts in an empty
//! folder, with an environment that carries none of flowproof's secrets, and
//! every request it makes of us (a permission, a file, a terminal) is
//! refused. The subset of ACP spoken here ([`wire`]) is small enough to
//! write by hand, which keeps the engine synchronous: the official SDK needs
//! tokio.

pub mod wire;

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::llm::{base64_png, ImagePart, ModelClient};
use crate::AgentError;
use wire::{initialize_params, model_of, Connection, Failure, Reply, AUTH_REQUIRED};

/// The same ceiling the HTTP client puts on one model call, and for the same
/// reason: a recording that fails is recoverable, one that hangs is not.
const TURN_TIMEOUT: Duration = Duration::from_secs(180);
/// Said ahead of every prompt. The agent's own system prompt describes a
/// coding assistant with tools; this turn wants one answer in one format.
const PREAMBLE: &str = "You are being used as a model by flowproof, not as a coding agent. \
Answer from this message alone, in exactly the format the instructions below ask for. \
Do not use tools, read files or run commands: none are available.";
/// What the agent's process may see of ours. Enough to find its login, its
/// config and the network; never `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`
/// (the agent would bill them instead of the subscription), and never the
/// SAP, Fiori or flow secrets flowproof itself holds in its environment.
#[rustfmt::skip]
const PASSED_ENV: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "TMPDIR", "TMP", "TEMP", "TERM",
    "LANG", "LC_ALL", "LC_CTYPE", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME",
    // Windows: where a user's profile, npm and the shell live.
    "USERPROFILE", "APPDATA", "LOCALAPPDATA", "SYSTEMROOT", "COMSPEC", "PATHEXT",
    "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "no_proxy",
    "NODE_EXTRA_CA_CERTS",
    // The agents' own config and login, wherever the user moved them.
    "CLAUDE_CONFIG_DIR", "CLAUDE_CODE_OAUTH_TOKEN", "CODEX_HOME",
];

/// Which agent stands in for the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
}

impl AgentKind {
    pub fn name(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::Codex => "codex",
        }
    }

    fn label(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::Codex => "Codex",
        }
    }

    /// The adapter's binary, and the pinned package `npx` fetches when that
    /// binary is not installed.
    fn adapter(self) -> (&'static str, &'static str) {
        match self {
            AgentKind::ClaudeCode => (
                "claude-agent-acp",
                "@agentclientprotocol/claude-agent-acp@0.101.0",
            ),
            AgentKind::Codex => ("codex-acp", "@agentclientprotocol/codex-acp@2.5.0"),
        }
    }

    fn sign_in(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "run `claude` in a terminal once and sign in",
            AgentKind::Codex => "run `codex login` in a terminal once",
        }
    }
}

/// A [`ModelClient`] backed by the user's coding agent. The agent starts on
/// the first call, not here: building a client is also how callers ask
/// whether a model is configured at all.
pub struct AcpModelClient {
    kind: AgentKind,
    command: Option<String>,
    live: Option<Live>,
    model: Option<String>,
}

impl AcpModelClient {
    /// `command` is the adapter program to run instead of the one flowproof
    /// would find for `kind`: a path, not a command line, so a folder with a
    /// space in it stays one argument (the desktop app passes the one it
    /// found, since an app started from the Dock has a shorter PATH).
    pub fn new(kind: AgentKind, command: Option<String>) -> Self {
        Self {
            kind,
            command: command.filter(|c| !c.trim().is_empty()),
            live: None,
            model: None,
        }
    }

    fn argv(&self) -> Result<Vec<String>, AgentError> {
        if let Some(command) = &self.command {
            return Ok(vec![command.trim().to_string()]);
        }
        let (bin, package) = self.kind.adapter();
        if let Some(path) = on_path(bin) {
            return Ok(vec![path]);
        }
        if let Some(npx) = on_path("npx") {
            return Ok(vec![npx, "-y".into(), package.into()]);
        }
        Err(AgentError::Config(format!(
            "{} is used through its ACP adapter, which is not installed: install Node so \
             flowproof can fetch it, or run `npm install -g {package}`",
            self.kind.label()
        )))
    }

    fn start(&self) -> Result<Live, AgentError> {
        let argv = self.argv()?;
        let folder = std::env::temp_dir().join(format!("flowproof-agent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder)
            .map_err(|e| AgentError::Config(format!("creating {}: {e}", folder.display())))?;
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(&folder)
            .env_clear()
            .envs(
                PASSED_ENV
                    .iter()
                    .filter_map(|k| Some((*k, std::env::var_os(k)?))),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| {
            let _ = std::fs::remove_dir_all(&folder);
            AgentError::Config(format!(
                "could not start {} (`{}`): {e}",
                self.kind.label(),
                argv[0]
            ))
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(AgentError::Config("agent started without pipes".into()));
        };
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        let tail = Arc::clone(&stderr_tail);
        std::thread::spawn(move || keep_tail(stderr, &tail));
        let mut live = Live {
            child,
            conn: Connection::new(stdin, lines),
            folder,
            stderr: stderr_tail,
            sees: false,
        };
        let init = live
            .conn
            .request(
                "initialize",
                initialize_params(),
                Instant::now() + TURN_TIMEOUT,
                &mut |_| {},
            )
            .map_err(|f| self.error(f, &live.stderr))?;
        live.sees = init["agentCapabilities"]["promptCapabilities"]["image"] == true;
        Ok(live)
    }

    fn started(&mut self) -> Result<&mut Live, AgentError> {
        if self.live.is_none() {
            self.live = Some(self.start()?);
        }
        self.live
            .as_mut()
            .ok_or_else(|| AgentError::Config("agent did not start".into()))
    }

    /// One prompt turn in a fresh session. Fresh because each call stands
    /// alone, as an API call does: one session for the whole recording would
    /// carry every earlier screenshot into every later turn.
    fn turn(&mut self, prompt: Vec<Value>) -> Result<String, AgentError> {
        let kind = self.kind;
        let outcome = session_turn(self.started()?, prompt);
        let (model, done, text) = match outcome {
            Ok(ok) => ok,
            Err(failure) => {
                let err = match &self.live {
                    Some(live) => self.error(failure, &live.stderr),
                    None => self.error(failure, &Mutex::default()),
                };
                // Whatever state the agent is in, the next call starts it over.
                self.live = None;
                return Err(err);
            }
        };
        if model.is_some() {
            self.model = model;
        }
        match done["stopReason"].as_str() {
            Some("end_turn") if !text.trim().is_empty() => Ok(text),
            Some("end_turn") => Err(AgentError::Config(format!(
                "{} ended its turn without an answer",
                kind.label()
            ))),
            other => Err(AgentError::Config(format!(
                "{} stopped without an answer ({})",
                kind.label(),
                other.unwrap_or("no stop reason")
            ))),
        }
    }

    fn error(&self, failure: Failure, stderr: &Mutex<String>) -> AgentError {
        let label = self.kind.label();
        let tail = stderr
            .lock()
            .map(|t| t.trim().to_string())
            .unwrap_or_default();
        let tail = if tail.is_empty() {
            String::new()
        } else {
            format!("; it said: {tail}")
        };
        AgentError::Config(match failure {
            Failure::Rpc { code, .. } if code == AUTH_REQUIRED => format!(
                "{label} is not signed in: {} and try again",
                self.kind.sign_in()
            ),
            Failure::Rpc { code, message } => {
                format!("{label} refused the call ({code}): {message}")
            }
            Failure::TimedOut => {
                format!("{label} did not answer within {}s", TURN_TIMEOUT.as_secs())
            }
            Failure::Closed => format!("{label} exited{tail}"),
        })
    }

    fn text(system: &str, user: &str) -> Value {
        json!({"type": "text", "text": format!("{PREAMBLE}\n\n{system}\n\n{user}")})
    }
}

impl ModelClient for AcpModelClient {
    fn complete(&mut self, system: &str, user: &str) -> Result<String, AgentError> {
        self.turn(vec![Self::text(system, user)])
    }

    /// The picture first, then the question about it. An agent that cannot
    /// see gets the text alone, as a text-only HTTP backend does.
    fn complete_with_image(
        &mut self,
        system: &str,
        user: &str,
        png: &[u8],
    ) -> Result<String, AgentError> {
        if !self.started()?.sees {
            return self.complete(system, user);
        }
        let image = json!({"type": "image", "mimeType": "image/png", "data": base64_png(png)});
        self.turn(vec![image, Self::text(system, user)])
    }

    fn complete_with_images(
        &mut self,
        system: &str,
        user: &str,
        images: &[ImagePart],
    ) -> Result<String, AgentError> {
        if !self.started()?.sees {
            return Err(AgentError::Config(format!(
                "{} did not offer to read images, which drafting from a video needs",
                self.kind.label()
            )));
        }
        // Each caption, then its picture, in order; the question after them
        // all, as the HTTP backends send it.
        let mut prompt = Vec::with_capacity(images.len() * 2 + 1);
        for image in images {
            prompt.push(json!({"type": "text", "text": image.caption}));
            prompt.push(json!({"type": "image", "mimeType": image.media_type,
                "data": base64_png(&image.data)}));
        }
        prompt.push(Self::text(system, user));
        self.turn(prompt)
    }

    /// The model is whatever the agent reported for its last session, or
    /// `default` before the first one: the agent picks it, not flowproof.
    fn identity(&self) -> (String, String) {
        let model = self.model.clone().unwrap_or_else(|| "default".into());
        (self.kind.name().to_string(), model)
    }
}

/// `session/new`, then `session/prompt` in it: the session's model, how the
/// turn stopped, and the answer.
fn session_turn(
    live: &mut Live,
    prompt: Vec<Value>,
) -> Result<(Option<String>, Value, String), Failure> {
    let deadline = Instant::now() + TURN_TIMEOUT;
    let cwd = live.folder.to_string_lossy().into_owned();
    let session = live.conn.request(
        "session/new",
        json!({"cwd": cwd, "mcpServers": []}),
        deadline,
        &mut |_| {},
    )?;
    let id = session["sessionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut reply = Reply::default();
    let done = live.conn.request(
        "session/prompt",
        json!({"sessionId": id, "prompt": prompt}),
        deadline,
        &mut |params| reply.take(params, &id),
    )?;
    Ok((model_of(&session), done, reply.text))
}

/// A running agent. Dropping it ends the process and removes its folder.
struct Live {
    child: Child,
    conn: Connection<ChildStdin>,
    folder: PathBuf,
    stderr: Arc<Mutex<String>>,
    sees: bool,
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

/// Keep the last part of the agent's stderr, for the error that quotes it.
fn keep_tail(stderr: impl Read, tail: &Mutex<String>) {
    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
        let Ok(mut tail) = tail.lock() else { return };
        tail.push_str(&line);
        tail.push('\n');
        if tail.len() > 2000 {
            let cut = tail.len() - 2000;
            let cut = (cut..tail.len())
                .find(|i| tail.is_char_boundary(*i))
                .unwrap_or(0);
            tail.drain(..cut);
        }
    }
}

/// `name` as found on PATH; on Windows the `.exe` or the npm `.cmd` shim.
fn on_path(name: &str) -> Option<String> {
    let exts: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd"]
    } else {
        &[""]
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| exts.iter().map(move |ext| dir.join(format!("{name}{ext}"))))
        .find(|path| path.is_file())
        .map(|path| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_never_sees_an_api_key_or_flowproofs_own_secrets() {
        for name in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "FLOWPROOF_AI_API_KEY",
            "SAP_PASSWORD",
            "FIORI_PASSWORD",
        ] {
            assert!(
                !PASSED_ENV.contains(&name),
                "{name} must not reach the agent"
            );
        }
    }

    #[test]
    fn a_given_command_replaces_the_adapter_flowproof_would_find() {
        let client = AcpModelClient::new(
            AgentKind::ClaudeCode,
            Some("/Users/me/Node Tools/bin/claude-agent-acp".into()),
        );
        assert_eq!(
            client.argv().expect("argv"),
            ["/Users/me/Node Tools/bin/claude-agent-acp"]
        );
        let blank = AcpModelClient::new(AgentKind::Codex, Some("  ".into()));
        assert_eq!(blank.command, None);
    }

    #[test]
    fn a_failure_names_the_agent_and_says_how_to_sign_in() {
        let client = AcpModelClient::new(AgentKind::ClaudeCode, None);
        let quiet = Mutex::default();
        let auth = Failure::Rpc {
            code: AUTH_REQUIRED,
            message: "Authentication required".into(),
        };
        assert!(client
            .error(auth, &quiet)
            .to_string()
            .contains("Claude Code is not signed in: run `claude` in a terminal once"));
        let said = Mutex::new("Error: node not found\n".to_string());
        assert!(client
            .error(Failure::Closed, &said)
            .to_string()
            .ends_with("Claude Code exited; it said: Error: node not found"));
    }

    #[test]
    fn the_model_is_the_agents_default_until_a_session_names_it() {
        let client = AcpModelClient::new(AgentKind::Codex, None);
        assert_eq!(
            client.identity(),
            ("codex".to_string(), "default".to_string())
        );
    }
}
