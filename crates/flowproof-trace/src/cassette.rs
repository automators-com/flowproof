//! The model-boundary cassette: one recorded exchange with a chat
//! completions API per turn, replayed back to the system under test so a
//! trajectory reruns offline, deterministically, at zero model cost.
//!
//! This is the deterministic spine of agent-boundary testing (#60). The
//! agent under test is a black box; the ONE place its nondeterminism
//! enters is the model call, so that is the only place flowproof has to
//! record. Everything an assertion needs - which tools were called, with
//! what arguments, what the model finally replied - is visible there.
//!
//! Three rules decide how a replay matches its recording, all chosen for
//! the same reason: a test that quietly tolerates drift stops being a
//! test.
//!
//! 1. **Strict, by position.** Turn K of the replay must match turn K of
//!    the recording. No searching forward for a turn that fits, and no
//!    tolerance holes: a prompt template that changed is exactly the
//!    thing this feature exists to catch, so matching it loosely would
//!    defeat the purpose.
//! 2. **Fail at the first divergent turn.** A trajectory that has already
//!    diverged tells you nothing about its later turns, and continuing
//!    would report cascading failures whose only real cause was the first.
//! 3. **Envelope first when reporting.** A byte diff of two 8000-token
//!    prompts is unreadable. The envelope - model, message count, roles,
//!    tool names - is compared and reported BEFORE any message body, so
//!    the common failures ("you added a tool", "you added a system
//!    message") are one line instead of a wall of text.

use serde::{Deserialize, Serialize};

/// One message in a chat completion, in the shape the OpenAI-compatible
/// wire format uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Calls the model asked for. Present on assistant messages only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Which call this message answers. Present on tool messages only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn new(role: &str, content: &str) -> Self {
        Self {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

/// A tool invocation the model asked for. `arguments` stays a STRING
/// because that is what the wire carries: re-encoding it as JSON would
/// silently reorder keys and lose the exact bytes an assertion may care
/// about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl ToolCall {
    /// The arguments as JSON, for path matchers. `None` when the model
    /// emitted something that is not valid JSON, which is a real thing
    /// models do and which an assertion should report rather than panic on.
    pub fn arguments_json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.arguments).ok()
    }
}

/// What the system under test sent to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnRequest {
    pub model: String,
    pub messages: Vec<Message>,
    /// Tool names offered, in the order the request listed them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
}

/// What the model answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnResponse {
    pub message: Message,
    /// The wire-level stop reason the model reported (`end_turn`,
    /// `tool_use`, ...). Recorded verbatim so replay can hand it straight
    /// back, and SERVED but NEVER MATCHED: it is an output of the turn, not
    /// part of the request identity, so a change in it must not fail replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

/// The default protocol for a turn: the OpenAI chat-completions wire shape,
/// which is what every v1 recording spoke. Kept as a free function so both
/// the serde default and the code that stamps a protocol name it once.
pub fn default_protocol() -> String {
    "openai".to_string()
}

fn protocol_is_default(protocol: &str) -> bool {
    protocol == "openai"
}

/// One request/response exchange at the model boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    /// The API dialect this exchange was recorded in: `"openai"` (chat
    /// completions) or `"anthropic"` (Messages). Defaults to `"openai"` and
    /// is omitted from the JSON when it equals that default, so every v1
    /// trace round-trips byte-identical and matching stays protocol-aware.
    #[serde(
        default = "default_protocol",
        skip_serializing_if = "protocol_is_default"
    )]
    pub protocol: String,
    pub request: TurnRequest,
    pub response: TurnResponse,
}

/// A recorded trajectory: every model call the system under test made,
/// in order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cassette {
    pub turns: Vec<Turn>,
}

/// The comparable shape of a request, without any message bodies. Two
/// requests whose envelopes differ have diverged in a way worth naming on
/// its own line.
#[derive(Debug, Clone, PartialEq)]
struct Envelope<'a> {
    model: &'a str,
    roles: Vec<&'a str>,
    tools: Vec<&'a str>,
}

impl<'a> Envelope<'a> {
    fn of(request: &'a TurnRequest) -> Self {
        Self {
            model: &request.model,
            roles: request.messages.iter().map(|m| m.role.as_str()).collect(),
            tools: request.tools.iter().map(String::as_str).collect(),
        }
    }
}

/// Why a replay turn did not match its recording. Carries the turn index
/// so a caller can say WHERE without recomputing it.
#[derive(Debug, Clone, PartialEq)]
pub struct Divergence {
    /// 0-based turn index, as stored. Rendered 1-based for humans.
    pub turn: usize,
    pub detail: String,
}

impl std::fmt::Display for Divergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "turn {}: {}", self.turn + 1, self.detail)
    }
}

/// Truncate a message body for a diff line. A prompt can be thousands of
/// tokens; the first divergent stretch is what identifies it.
fn abbreviate(text: &str) -> String {
    const LIMIT: usize = 160;
    let one_line: String = text
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    if one_line.chars().count() <= LIMIT {
        return one_line;
    }
    let head: String = one_line.chars().take(LIMIT).collect();
    format!("{head}...")
}

/// Describe how two message lists differ, envelope having already matched
/// (so the roles line up and only bodies can differ).
fn message_divergence(recorded: &[Message], incoming: &[Message]) -> Option<String> {
    for (i, (want, got)) in recorded.iter().zip(incoming).enumerate() {
        if want == got {
            continue;
        }
        if want.content != got.content {
            return Some(format!(
                "message {i} ({}) content changed\n  recorded: {}\n  replayed: {}",
                want.role,
                abbreviate(want.content.as_deref().unwrap_or("")),
                abbreviate(got.content.as_deref().unwrap_or("")),
            ));
        }
        if want.tool_calls != got.tool_calls {
            let names = |calls: &[ToolCall]| {
                calls
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            return Some(format!(
                "message {i} ({}) tool calls changed\n  recorded: [{}]\n  replayed: [{}]",
                want.role,
                names(&want.tool_calls),
                names(&got.tool_calls),
            ));
        }
        return Some(format!("message {i} ({}) changed", want.role));
    }
    None
}

impl Cassette {
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    pub fn len(&self) -> usize {
        self.turns.len()
    }

    /// Serve turn `index`, or say precisely how the request diverged.
    ///
    /// Position is the whole contract: this does NOT scan for a turn that
    /// happens to fit. If the system under test made a different call than
    /// it made while recording, the recording no longer describes it, and
    /// the honest answer is to say so at the exact turn it stopped being
    /// true.
    pub fn turn(
        &self,
        index: usize,
        incoming: &TurnRequest,
        protocol: &str,
    ) -> Result<&TurnResponse, Divergence> {
        let Some(turn) = self.turns.get(index) else {
            return Err(Divergence {
                turn: index,
                detail: format!(
                    "the system under test made {} model calls, the recording has {}",
                    index + 1,
                    self.turns.len()
                ),
            });
        };
        let recorded = &turn.request;

        // Protocol is the FIRST thing compared: a turn recorded in one API
        // dialect and replayed in another is not the same conversation at
        // all, and saying so plainly beats a body diff between two shapes.
        if turn.protocol != protocol {
            return Err(Divergence {
                turn: index,
                detail: format!(
                    "protocol changed: recorded {}, replayed {}",
                    turn.protocol, protocol
                ),
            });
        }

        // Envelope first: these are the differences a human can act on
        // immediately, and reporting them alongside a body diff would bury
        // them.
        let (want, got) = (Envelope::of(recorded), Envelope::of(incoming));
        if want.model != got.model {
            return Err(Divergence {
                turn: index,
                detail: format!(
                    "model changed: recorded {}, replayed {}",
                    want.model, got.model
                ),
            });
        }
        if want.tools != got.tools {
            return Err(Divergence {
                turn: index,
                detail: format!(
                    "tools offered changed\n  recorded: [{}]\n  replayed: [{}]",
                    want.tools.join(", "),
                    got.tools.join(", ")
                ),
            });
        }
        if want.roles != got.roles {
            return Err(Divergence {
                turn: index,
                detail: format!(
                    "conversation shape changed: recorded {} messages [{}], replayed {} [{}]",
                    want.roles.len(),
                    want.roles.join(", "),
                    got.roles.len(),
                    got.roles.join(", ")
                ),
            });
        }

        if let Some(detail) = message_divergence(&recorded.messages, &incoming.messages) {
            return Err(Divergence {
                turn: index,
                detail,
            });
        }
        Ok(&turn.response)
    }

    /// Every tool call in the trajectory, in order, paired with the turn
    /// that produced it. This is what `assert_tool_call` matches against.
    pub fn tool_calls(&self) -> Vec<(usize, &ToolCall)> {
        self.turns
            .iter()
            .enumerate()
            .flat_map(|(i, turn)| turn.response.message.tool_calls.iter().map(move |c| (i, c)))
            .collect()
    }

    /// The reply: the content of the LAST assistant message in the
    /// trajectory.
    ///
    /// Fable's ruling, and it beats the alternative the design doc
    /// sketched (a process's stdout). Stdout is whatever the harness chose
    /// to print - a banner, a spinner, nothing at all - and differs per
    /// driver. The final assistant message is the same fact for every
    /// driver, and it is the thing the agent actually decided to say.
    ///
    /// A trajectory whose last turn is a tool call has no reply yet, which
    /// is a real state and returns `None` rather than an empty string.
    pub fn reply(&self) -> Option<&str> {
        self.turns
            .iter()
            .rev()
            .map(|turn| &turn.response.message)
            .find(|m| m.role == "assistant" && m.content.is_some())
            .and_then(|m| m.content.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: format!("call_{name}"),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    fn assistant_with(calls: Vec<ToolCall>) -> Message {
        Message {
            role: "assistant".into(),
            content: None,
            tool_calls: calls,
            tool_call_id: None,
        }
    }

    fn request(messages: Vec<Message>, tools: &[&str]) -> TurnRequest {
        TurnRequest {
            model: "gpt-4o".into(),
            messages,
            tools: tools.iter().map(|t| t.to_string()).collect(),
        }
    }

    /// An OpenAI turn, the default protocol, so the booking helpers stay
    /// terse while the struct carries its new fields.
    fn openai_turn(request: TurnRequest, message: Message) -> Turn {
        Turn {
            protocol: default_protocol(),
            request,
            response: TurnResponse {
                message,
                stop_reason: None,
            },
        }
    }

    /// A two-turn booking trajectory: the model asks for a tool, the tool
    /// result comes back, the model replies.
    fn booking() -> Cassette {
        Cassette {
            turns: vec![
                openai_turn(
                    request(
                        vec![Message::new("user", "Book me a flight to Nairobi")],
                        &["search_flights"],
                    ),
                    assistant_with(vec![call("search_flights", r#"{"destination":"NBO"}"#)]),
                ),
                openai_turn(
                    request(
                        vec![
                            Message::new("user", "Book me a flight to Nairobi"),
                            Message::new("tool", r#"{"flights":[{"id":"KQ311"}]}"#),
                        ],
                        &["search_flights"],
                    ),
                    Message::new("assistant", "Booked KQ311 to Nairobi."),
                ),
            ],
        }
    }

    #[test]
    fn an_identical_trajectory_replays() {
        let cassette = booking();
        for (i, turn) in cassette.turns.iter().enumerate() {
            let served = cassette
                .turn(i, &turn.request, &turn.protocol)
                .expect("turn matches");
            assert_eq!(served, &turn.response);
        }
    }

    /// The headline failure this feature exists to catch: someone edited a
    /// prompt template. It must be named, and named at the turn it
    /// happened.
    #[test]
    fn a_changed_prompt_diverges_and_says_so() {
        let cassette = booking();
        let drifted = request(
            vec![Message::new("user", "Book me a flight to Mombasa")],
            &["search_flights"],
        );
        let err = cassette
            .turn(0, &drifted, "openai")
            .expect_err("must diverge");
        assert_eq!(err.turn, 0);
        assert!(err.detail.contains("content changed"), "{err}");
        assert!(err.detail.contains("Nairobi"), "shows the recording: {err}");
        assert!(err.detail.contains("Mombasa"), "shows the replay: {err}");
        assert!(err.to_string().starts_with("turn 1:"), "1-based: {err}");
    }

    /// Envelope differences are reported on their own, WITHOUT a body
    /// diff: "you added a tool" is a one-line answer and burying it under
    /// eight thousand tokens of prompt would be a worse report.
    #[test]
    fn envelope_differences_are_reported_before_bodies() {
        let cassette = booking();

        // A new tool, and a changed prompt at the same time. The tool is
        // the more actionable fact, so it is what gets reported.
        let mut both = request(
            vec![Message::new("user", "something else entirely")],
            &["search_flights", "create_booking"],
        );
        let err = cassette.turn(0, &both, "openai").expect_err("must diverge");
        assert!(err.detail.contains("tools offered changed"), "{err}");
        assert!(!err.detail.contains("content changed"), "{err}");

        // An extra message: the shape line names both counts and roles.
        both.tools = vec!["search_flights".into()];
        both.messages = vec![
            Message::new("system", "You are helpful"),
            Message::new("user", "Book me a flight to Nairobi"),
        ];
        let err = cassette.turn(0, &both, "openai").expect_err("must diverge");
        assert!(err.detail.contains("conversation shape changed"), "{err}");
        assert!(err.detail.contains("system"), "{err}");

        // A different model is its own line.
        let mut swapped = booking().turns[0].request.clone();
        swapped.model = "gpt-4o-mini".into();
        let err = cassette
            .turn(0, &swapped, "openai")
            .expect_err("must diverge");
        assert!(err.detail.contains("model changed"), "{err}");
    }

    /// Position is the contract. Turn 1's request is a perfectly valid
    /// recorded request - just not at turn 0 - and matching it there would
    /// be exactly the "search forward" tolerance v1 rejects.
    #[test]
    fn a_turn_is_matched_by_position_not_by_search() {
        let cassette = booking();
        let turn_two = cassette.turns[1].request.clone();
        let err = cassette
            .turn(0, &turn_two, "openai")
            .expect_err("a later turn must not satisfy an earlier one");
        assert_eq!(err.turn, 0);
    }

    /// An agent that makes MORE calls than it did while recording has run
    /// off the end. Say that plainly, with both counts.
    #[test]
    fn running_past_the_recording_is_named_with_both_counts() {
        let cassette = booking();
        let extra = cassette.turns[1].request.clone();
        let err = cassette
            .turn(2, &extra, "openai")
            .expect_err("past the end");
        assert!(err.detail.contains("3 model calls"), "{err}");
        assert!(err.detail.contains("has 2"), "{err}");
    }

    /// Tool calls are the trajectory, flattened in order, which is what an
    /// ordered-subsequence assertion needs.
    #[test]
    fn tool_calls_come_out_in_trajectory_order() {
        let mut cassette = booking();
        cassette.turns[1].response.message = assistant_with(vec![
            call("create_booking", r#"{"flight":"KQ311"}"#),
            call("notify", "{}"),
        ]);
        let names: Vec<&str> = cassette
            .tool_calls()
            .iter()
            .map(|(_, c)| c.name.as_str())
            .collect();
        assert_eq!(names, ["search_flights", "create_booking", "notify"]);
        // The turn index rides along, so a failure can say where.
        assert_eq!(cassette.tool_calls()[2].0, 1);
    }

    #[test]
    fn arguments_parse_as_json_and_survive_nonsense() {
        let good = call("search_flights", r#"{"destination":"NBO"}"#);
        assert_eq!(
            good.arguments_json().and_then(|v| v
                .get("destination")
                .and_then(|d| d.as_str())
                .map(str::to_string)),
            Some("NBO".into())
        );
        // Models do emit broken JSON. That is a finding for an assertion
        // to report, not a panic.
        assert_eq!(call("x", "{not json").arguments_json(), None);
    }

    /// `reply` is the final assistant message, per Fable's ruling.
    #[test]
    fn the_reply_is_the_last_assistant_message() {
        assert_eq!(booking().reply(), Some("Booked KQ311 to Nairobi."));

        // A trajectory still mid-tool-call has not replied yet. That is a
        // real state, and None says it better than "".
        let mut unfinished = booking();
        unfinished.turns[1].response.message = assistant_with(vec![call("create_booking", "{}")]);
        assert_eq!(unfinished.reply(), None);

        assert_eq!(Cassette::default().reply(), None);
    }

    /// The cassette rides in the trace, so it has to survive the trip.
    #[test]
    fn a_cassette_round_trips_through_json() {
        let cassette = booking();
        let json = serde_json::to_string(&cassette).expect("serializes");
        let back: Cassette = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, cassette);
        // Absent optional fields stay absent rather than serializing as
        // nulls and empty arrays, which keeps old readers happy.
        assert!(!json.contains("null"), "{json}");
        assert!(!json.contains("tool_call_id"), "{json}");
        // The v2 additions are absent on an OpenAI turn with no stop reason,
        // so a v1 trace round-trips byte-identical: no `protocol` key (it
        // equals the default) and no `stop_reason` key (it is None).
        assert!(!json.contains("protocol"), "{json}");
        assert!(!json.contains("stop_reason"), "{json}");
    }

    /// An anthropic turn carries its protocol through the round trip, and an
    /// openai turn beside it still omits the key - the two coexist in one
    /// cassette without either leaking into the other.
    #[test]
    fn protocol_and_stop_reason_survive_the_round_trip() {
        let mut anthropic = openai_turn(
            request(
                vec![Message::new("user", "Book me a flight to Nairobi")],
                &["search_flights"],
            ),
            Message::new("assistant", "Booked."),
        );
        anthropic.protocol = "anthropic".into();
        anthropic.response.stop_reason = Some("end_turn".into());
        let openai = openai_turn(
            request(vec![Message::new("user", "hi")], &[]),
            Message::new("assistant", "hello"),
        );
        let cassette = Cassette {
            turns: vec![anthropic, openai],
        };

        let json = serde_json::to_string(&cassette).expect("serializes");
        assert!(json.contains("\"protocol\":\"anthropic\""), "{json}");
        assert!(json.contains("\"stop_reason\":\"end_turn\""), "{json}");
        // The openai turn beside it still omits both keys, so the marker is
        // present exactly once - on the turn that needs it.
        assert_eq!(json.matches("\"protocol\"").count(), 1, "{json}");
        assert_eq!(json.matches("\"stop_reason\"").count(), 1, "{json}");

        let back: Cassette = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, cassette);
        assert_eq!(back.turns[1].protocol, "openai");
    }
}
