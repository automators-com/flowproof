//! The ACP wire: JSON-RPC 2.0, one message per line, over any writer and a
//! channel of the lines read back. Only what a client that uses an agent as
//! a model needs: send a request, answer the agent's own requests while
//! waiting for the reply (refusing all of them), and pick the answer out of
//! the session updates.

use std::io::Write;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Instant;

use serde_json::{json, Value};

/// ACP's JSON-RPC code for "this agent needs you to sign in first".
pub const AUTH_REQUIRED: i64 = -32000;

/// Why a request got no result.
#[derive(Debug, PartialEq)]
pub enum Failure {
    Rpc { code: i64, message: String },
    TimedOut,
    Closed,
}

/// One JSON-RPC conversation: our requests out, one line each; the agent's
/// replies, requests and notifications in, one line each.
pub struct Connection<W: Write> {
    writer: W,
    lines: Receiver<String>,
    next_id: u64,
}

impl<W: Write> Connection<W> {
    pub fn new(writer: W, lines: Receiver<String>) -> Self {
        Self {
            writer,
            lines,
            next_id: 0,
        }
    }

    fn send(&mut self, message: &Value) -> Result<(), Failure> {
        let line = format!("{message}\n");
        self.writer
            .write_all(line.as_bytes())
            .and_then(|()| self.writer.flush())
            .map_err(|_| Failure::Closed)
    }

    /// Send `method` and wait for its answer, answering whatever the agent
    /// asks in the meantime and handing its notifications to `on_update`.
    pub fn request(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
        on_update: &mut dyn FnMut(&Value),
    ) -> Result<Value, Failure> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(wait) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => return Err(Failure::TimedOut),
                Err(RecvTimeoutError::Disconnected) => return Err(Failure::Closed),
            };
            // An adapter may log to stdout; only JSON is the protocol.
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match (message["method"].as_str(), message.get("id")) {
                (Some(asked), Some(their_id)) => {
                    let mut reply = answer(asked, &message["params"]);
                    reply["jsonrpc"] = json!("2.0");
                    reply["id"] = their_id.clone();
                    self.send(&reply)?;
                }
                (Some(_), None) => on_update(&message["params"]),
                (None, Some(their_id)) if their_id.as_u64() == Some(id) => {
                    if let Some(error) = message.get("error") {
                        return Err(Failure::Rpc {
                            code: error["code"].as_i64().unwrap_or_default(),
                            message: error["message"].as_str().unwrap_or_default().to_string(),
                        });
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
                _ => {}
            }
        }
    }
}

/// Our side of a request the agent makes. A permission is answered with the
/// agent's own "reject" option; anything else (files, terminals) was never
/// offered in `initialize`, so it is not a method we have.
pub fn answer(method: &str, params: &Value) -> Value {
    if method == "session/request_permission" {
        let options = params["options"].as_array().cloned().unwrap_or_default();
        let reject = ["reject_once", "reject_always"].iter().find_map(|kind| {
            options
                .iter()
                .find(|o| o["kind"] == *kind)
                .and_then(|o| o["optionId"].as_str())
        });
        let outcome = match reject {
            Some(option) => json!({"outcome": "selected", "optionId": option}),
            None => json!({"outcome": "cancelled"}),
        };
        return json!({"result": {"outcome": outcome}});
    }
    json!({"error": {"code": -32601, "message": format!("flowproof does not offer {method}")}})
}

/// What we tell the agent we are: a client with no files and no terminal
/// to offer it.
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "fs": {"readTextFile": false, "writeTextFile": false},
            "terminal": false,
        },
        "clientInfo": {"name": "flowproof", "version": env!("CARGO_PKG_VERSION")},
    })
}

/// The model the agent says this session runs, from its `model` config
/// option, when it offers one.
pub fn model_of(session: &Value) -> Option<String> {
    session["configOptions"]
        .as_array()?
        .iter()
        .find(|o| o["category"] == "model" || o["id"] == "model")?["currentValue"]
        .as_str()
        .map(str::to_string)
}

/// The answer of one prompt turn: the agent's last message. Text written
/// before a tool call ("let me look at the file first") is narration, not
/// the answer, so a tool call or a new message id starts it over.
#[derive(Default)]
pub struct Reply {
    pub text: String,
    message_id: Option<String>,
    fresh: bool,
}

impl Reply {
    pub fn take(&mut self, params: &Value, session: &str) {
        if params["sessionId"] != session {
            return;
        }
        let update = &params["update"];
        match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => {
                let id = update["messageId"].as_str();
                if self.fresh || (id.is_some() && id != self.message_id.as_deref()) {
                    self.text.clear();
                    self.fresh = false;
                }
                if let Some(id) = id {
                    self.message_id = Some(id.to_string());
                }
                if let Some(text) = update["content"]["text"].as_str() {
                    self.text.push_str(text);
                }
            }
            Some("tool_call") => self.fresh = true,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Sender};
    use std::time::Duration;

    /// Lines written to the connection, as the agent reads them.
    struct Wire(Sender<String>, Vec<u8>);

    impl Write for Wire {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.1.extend_from_slice(buf);
            while let Some(end) = self.1.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.1.drain(..=end).collect();
                let _ = self
                    .0
                    .send(String::from_utf8_lossy(&line).trim().to_string());
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A connection and a fake agent that answers each request we send with
    /// the lines `script` returns for it.
    fn with_agent(
        script: impl Fn(&Value) -> Vec<Value> + Send + 'static,
    ) -> (Connection<Wire>, std::thread::JoinHandle<Vec<Value>>) {
        let (to_agent, agent_reads) = mpsc::channel::<String>();
        let (agent_writes, lines) = mpsc::channel::<String>();
        let agent = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for line in agent_reads {
                let message: Value = serde_json::from_str(&line).expect("json from flowproof");
                for out in script(&message) {
                    let _ = agent_writes.send(out.to_string());
                }
                seen.push(message);
            }
            seen
        });
        let conn = Connection::new(Wire(to_agent, Vec::new()), lines);
        (conn, agent)
    }

    fn chunk(text: &str, message: &str) -> Value {
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1",
            "update": {"sessionUpdate": "agent_message_chunk", "messageId": message,
                "content": {"type": "text", "text": text}}}})
    }

    #[test]
    fn a_turn_refuses_the_agent_and_answers_with_its_last_message() {
        let (mut conn, agent) = with_agent(|m| match m["method"].as_str() {
            Some("session/prompt") => vec![
                chunk("Let me read the file.", "m1"),
                json!({"jsonrpc": "2.0", "id": 90, "method": "session/request_permission",
                    "params": {"sessionId": "s1", "toolCall": {"toolCallId": "t1"}, "options": [
                        {"optionId": "yes", "name": "Allow", "kind": "allow_once"},
                        {"optionId": "no", "name": "Reject", "kind": "reject_once"}]}}),
                json!({"jsonrpc": "2.0", "id": 91, "method": "fs/read_text_file",
                    "params": {"sessionId": "s1", "path": "/etc/passwd"}}),
                json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1",
                    "update": {"sessionUpdate": "tool_call", "toolCallId": "t1"}}}),
                chunk("{\"action\": ", "m1"),
                chunk("\"click\"}", "m1"),
                json!({"jsonrpc": "2.0", "id": m["id"], "result": {"stopReason": "end_turn"}}),
            ],
            _ => vec![],
        });
        let mut reply = Reply::default();
        let done = conn
            .request(
                "session/prompt",
                json!({}),
                Instant::now() + Duration::from_secs(5),
                &mut |p| reply.take(p, "s1"),
            )
            .expect("turn ends");
        assert_eq!(done["stopReason"], "end_turn");
        assert_eq!(reply.text, "{\"action\": \"click\"}");
        drop(conn);
        let seen = agent.join().expect("agent thread");
        let permission = seen
            .iter()
            .find(|m| m["id"] == 90)
            .expect("permission answered");
        assert_eq!(
            permission["result"]["outcome"],
            json!({"outcome": "selected", "optionId": "no"})
        );
        let file = seen
            .iter()
            .find(|m| m["id"] == 91)
            .expect("file request answered");
        assert_eq!(file["error"]["code"], -32601);
    }

    #[test]
    fn an_error_reply_and_a_silent_agent_are_failures() {
        let (mut conn, _agent) = with_agent(|m| match m["method"].as_str() {
            Some("session/new") => vec![json!({"jsonrpc": "2.0", "id": m["id"],
                "error": {"code": AUTH_REQUIRED, "message": "Authentication required"}})],
            _ => vec![],
        });
        // Generous for the answered request, so a busy CI machine scheduling
        // the agent thread late is not a failure; short for the silent one.
        let answered = Instant::now() + Duration::from_secs(5);
        assert_eq!(
            conn.request("session/new", json!({}), answered, &mut |_| {}),
            Err(Failure::Rpc {
                code: AUTH_REQUIRED,
                message: "Authentication required".into()
            })
        );
        assert_eq!(
            conn.request(
                "session/prompt",
                json!({}),
                Instant::now() + Duration::from_millis(200),
                &mut |_| {}
            ),
            Err(Failure::TimedOut)
        );
    }

    #[test]
    fn a_permission_with_no_reject_option_is_cancelled() {
        let params = json!({"options": [{"optionId": "ok", "kind": "allow_always"}]});
        assert_eq!(
            answer("session/request_permission", &params)["result"]["outcome"],
            json!({"outcome": "cancelled"})
        );
    }

    #[test]
    fn the_model_comes_from_the_sessions_model_option() {
        let session = json!({"sessionId": "s1", "configOptions": [
            {"id": "mode", "category": "mode", "currentValue": "auto"},
            {"id": "model", "category": "model", "currentValue": "opus"}]});
        assert_eq!(model_of(&session).as_deref(), Some("opus"));
        assert_eq!(model_of(&json!({"sessionId": "s1"})), None);
    }
}
