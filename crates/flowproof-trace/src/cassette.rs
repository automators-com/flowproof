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
//! 1. **Strict by body, consumed exactly once.** A replayed call must
//!    match a recorded turn BYTE-FOR-BYTE, and every recorded turn is
//!    served at most once, so an extra call still fails. No tolerance
//!    holes: a prompt template that changed is exactly the thing this
//!    feature exists to catch.
//!
//!    Position was the contract in v1, and it assumed a strictly
//!    sequential trajectory. Real agents break that: goose issues its
//!    task call and a session-title call concurrently, so record sees one
//!    order and replay sees the other, and a positional matcher called
//!    that a divergence when nothing about the agent had changed. Order
//!    BETWEEN CONCURRENT CALLS is therefore no longer asserted - the
//!    agent does not guarantee it, so a recording cannot either. A
//!    sequential trajectory still matches turn-for-turn, because the
//!    earliest unconsumed match wins.
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
            // When the SAME tools were called and only their arguments
            // moved, the two name lists are identical and printing them
            // says nothing at all - the reader is told something changed
            // and left to find it. Name the argument PATH instead.
            let (want_names, got_names) = (names(&want.tool_calls), names(&got.tool_calls));
            if want_names == got_names {
                let changes: Vec<String> = want
                    .tool_calls
                    .iter()
                    .zip(&got.tool_calls)
                    .flat_map(|(a, b)| {
                        crate::cassette_diff::argument_changes(a, b)
                            .into_iter()
                            .map(move |(path, before, after)| {
                                format!("{}.{path}: recorded {before}, replayed {after}", a.name)
                            })
                    })
                    .collect();
                if !changes.is_empty() {
                    return Some(format!(
                        "message {i} ({}) tool call arguments changed\n  {}",
                        want.role,
                        changes.join("\n  "),
                    ));
                }
            }
            return Some(format!(
                "message {i} ({}) tool calls changed\n  recorded: [{}]\n  replayed: [{}]",
                want.role, want_names, got_names,
            ));
        }
        return Some(format!("message {i} ({}) changed", want.role));
    }
    None
}

/// How `incoming` differs from a recorded `turn`, or `None` if they are the
/// same call. Factored out of positional lookup so the same byte-exact
/// comparison can both SELECT a turn and EXPLAIN a mismatch.
fn turn_divergence(turn: &Turn, incoming: &TurnRequest, protocol: &str) -> Option<String> {
let recorded = &turn.request;

    // Protocol is the FIRST thing compared: a turn recorded in one API
    // dialect and replayed in another is not the same conversation at
    // all, and saying so plainly beats a body diff between two shapes.
    if turn.protocol != protocol {
        return Some(format!(
                "protocol changed: recorded {}, replayed {}",
                turn.protocol, protocol
        ));
    }

    // Envelope first: these are the differences a human can act on
    // immediately, and reporting them alongside a body diff would bury
    // them.
    let (want, got) = (Envelope::of(recorded), Envelope::of(incoming));
    if want.model != got.model {
        return Some(format!(
                "model changed: recorded {}, replayed {}",
                want.model, got.model
        ));
    }
    if want.tools != got.tools {
        return Some(format!(
                "tools offered changed\n  recorded: [{}]\n  replayed: [{}]",
                want.tools.join(", "),
                got.tools.join(", ")
        ));
    }
    if want.roles != got.roles {
        return Some(format!(
                "conversation shape changed: recorded {} messages [{}], replayed {} [{}]",
                want.roles.len(),
                want.roles.join(", "),
                got.roles.len(),
                got.roles.join(", ")
        ));
    }

if let Some(detail) = message_divergence(&recorded.messages, &incoming.messages) {
    return Some(detail);
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
        turn_divergence(turn, incoming, protocol)
            .map(|detail| Divergence {
                turn: index,
                detail,
            })
            .map_or(Ok(&turn.response), Err)
    }

    /// Serve the recorded turn that matches `incoming`, consuming it.
    ///
    /// Position was the whole contract in v1, and that assumed a strictly
    /// sequential trajectory. Real agents break it: goose issues its task
    /// call and a session-title call CONCURRENTLY, so which arrives first
    /// is a race. Record sees one order (the slow upstream call lands
    /// second), replay sees the other (the cassette answers instantly), and
    /// a positional matcher calls that a divergence when nothing about the
    /// agent changed.
    ///
    /// What stays strict is the part that catches regressions: bodies are
    /// still compared byte-for-byte, every recorded turn must still be
    /// consumed exactly once, and an extra call is still a failure. Only
    /// the ORDER between concurrent calls is no longer asserted, because
    /// the agent does not guarantee it and neither can a recording.
    ///
    /// `consumed` is the caller's per-run state, one flag per recorded
    /// turn; it is resized on first use.
    pub fn match_turn(
        &self,
        consumed: &mut Vec<bool>,
        incoming: &TurnRequest,
        protocol: &str,
    ) -> Result<(usize, &TurnResponse), Divergence> {
        if consumed.len() < self.turns.len() {
            consumed.resize(self.turns.len(), false);
        }
        let served = consumed.iter().filter(|c| **c).count();
        let Some(first_open) = consumed.iter().position(|c| !*c) else {
            return Err(Divergence {
                turn: served,
                detail: format!(
                    "the system under test made {} model calls, the recording has {}",
                    served + 1,
                    self.turns.len()
                ),
            });
        };
        // Earliest unconsumed exact match wins, so a strictly sequential
        // trajectory still matches turn-for-turn exactly as it did in v1.
        for (i, turn) in self.turns.iter().enumerate().skip(first_open) {
            if consumed[i] {
                continue;
            }
            if turn_divergence(turn, incoming, protocol).is_none() {
                consumed[i] = true;
                return Ok((i, &turn.response));
            }
        }
        // Nothing matched. Report against the first unconsumed turn: for a
        // sequential trajectory that IS the turn the agent diverged at, so
        // the message is unchanged from v1.
        let detail = turn_divergence(&self.turns[first_open], incoming, protocol)
            .unwrap_or_else(|| "the call did not match any unconsumed recorded turn".to_string());
        Err(Divergence {
            turn: first_open,
            detail,
        })
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
        let primary = self.primary_thread();
        self.turns
            .iter()
            .enumerate()
            .filter(|(i, _)| primary.as_ref().is_none_or(|p| p.contains(i)))
            .map(|(_, turn)| &turn.response.message)
            .rev()
            .find(|m| m.role == "assistant" && m.content.is_some())
            .and_then(|m| m.content.as_deref())
    }

    /// Which turns belong to the conversation the flow is actually about,
    /// or `None` when they all do (the ordinary case, where this changes
    /// nothing).
    ///
    /// An agent may talk to the model about something other than the task:
    /// goose asks it to name the session, in a call with its OWN system
    /// prompt, issued concurrently with the real one and not waited for. Its
    /// answer ("France's capital city") is an assistant message, and taking
    /// the trajectory's last one made `reply` a coin flip - whichever call
    /// happened to land second won.
    ///
    /// A side conversation is recognisable by its system prompt: turns that
    /// continue one conversation share one, and a housekeeping call brings a
    /// different one. So group by system prompt and keep the thread with the
    /// most turns; ties go to the thread carrying the most request text,
    /// because the conversation doing the work carries the agent's real
    /// system prompt and its tool schemas, and a housekeeping call is small.
    ///
    /// Both halves are order-independent, which is the point: the answer
    /// cannot depend on which concurrent call happened to arrive first.
    ///
    /// The limit, stated plainly: this is a heuristic, and an agent whose
    /// side conversation is BIGGER than its real one would defeat the
    /// tie-break. Nothing here fabricates a reply - a single-thread cassette
    /// takes the identical path it always did.
    fn primary_thread(&self) -> Option<Vec<usize>> {
        let system_of = |turn: &Turn| -> String {
            turn.request
                .messages
                .iter()
                .find(|m| m.role == "system")
                .and_then(|m| m.content.clone())
                .unwrap_or_default()
        };
        let mut threads: Vec<(String, Vec<usize>, usize)> = Vec::new();
        for (i, turn) in self.turns.iter().enumerate() {
            let key = system_of(turn);
            let weight: usize = turn
                .request
                .messages
                .iter()
                .map(|m| m.content.as_deref().unwrap_or("").len())
                .sum();
            match threads.iter_mut().find(|(k, _, _)| *k == key) {
                Some((_, idx, w)) => {
                    idx.push(i);
                    *w += weight;
                }
                None => threads.push((key, vec![i], weight)),
            }
        }
        if threads.len() < 2 {
            return None;
        }
        threads
            .into_iter()
            .max_by_key(|(_, idx, weight)| (idx.len(), *weight))
            .map(|(_, idx, _)| idx)
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

    fn msg(role: &str, content: &str) -> Message {
        Message {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn said(content: &str) -> Message {
        msg("assistant", content)
    }

    /// goose asks the model to name the session, with its OWN system prompt,
    /// concurrently with the real call and without waiting for it. Whichever
    /// landed second used to become `reply`, so a passing record was luck.
    /// The real conversation wins regardless of arrival order.
    #[test]
    fn a_side_conversation_does_not_become_the_reply() {
        let agent_system = "You are a general-purpose AI agent called goose, \
            with tools and a long standing system prompt that carries the work.";
        let task = openai_turn(
            request(
                vec![msg("system", agent_system), msg("user", "capital of France?")],
                &[],
            ),
            said("Paris"),
        );
        let title = openai_turn(
            request(
                vec![
                    msg("system", "Generate a short title."),
                    msg("user", "capital of France?"),
                ],
                &[],
            ),
            said("France's capital city"),
        );

        // Recorded task-first, then title-first: the answer must not move.
        for turns in [
            vec![task.clone(), title.clone()],
            vec![title.clone(), task.clone()],
        ] {
            let cassette = Cassette { turns };
            assert_eq!(
                cassette.reply(),
                Some("Paris"),
                "the side conversation must never supply the reply"
            );
        }
    }

    /// The ordinary case is untouched: one conversation, the reply is its
    /// last assistant message, exactly as before.
    #[test]
    fn a_single_conversation_still_replies_with_its_last_message() {
        let sys = "You are a helpful assistant.";
        let cassette = Cassette {
            turns: vec![
                openai_turn(
                    request(vec![msg("system", sys), msg("user", "hi")], &[]),
                    said("hello"),
                ),
                openai_turn(
                    request(
                        vec![msg("system", sys), msg("user", "hi"), msg("user", "and now?")],
                        &[],
                    ),
                    said("goodbye"),
                ),
            ],
        };
        assert_eq!(cassette.reply(), Some("goodbye"));
    }

    /// Concurrent calls arrive in either order, so a cassette recorded in one
    /// order must replay against the other. Bodies still have to match
    /// byte-for-byte, and each recorded turn is consumed exactly once.
    #[test]
    fn a_cassette_replays_when_concurrent_calls_swap_order() {
        let a = openai_turn(
            request(vec![msg("system", "A"), msg("user", "one")], &[]),
            said("first"),
        );
        let b = openai_turn(
            request(vec![msg("system", "B"), msg("user", "two")], &[]),
            said("second"),
        );
        let cassette = Cassette {
            turns: vec![a.clone(), b.clone()],
        };
        let mut consumed = Vec::new();

        // Replay hands them over in the OPPOSITE order to the recording.
        let (i, got) = cassette
            .match_turn(&mut consumed, &b.request, "openai")
            .expect("the later-recorded turn still matches");
        assert_eq!((i, got.message.content.as_deref()), (1, Some("second")));
        let (i, got) = cassette
            .match_turn(&mut consumed, &a.request, "openai")
            .expect("the earlier-recorded turn still matches");
        assert_eq!((i, got.message.content.as_deref()), (0, Some("first")));

        // Every turn consumed exactly once: a third call is still a failure.
        let err = cassette
            .match_turn(&mut consumed, &a.request, "openai")
            .expect_err("an extra call must not be served twice");
        assert!(
            err.detail.contains("made 3 model calls"),
            "counts the extra call: {}",
            err.detail
        );
    }

    /// An argument-only change used to print two IDENTICAL tool-name lists
    /// ("recorded: [book] / replayed: [book]"), telling the reader that
    /// something moved but not what. It names the PATH now.
    #[test]
    fn an_argument_only_change_names_the_path_that_moved() {
        let recorded = assistant_with(vec![call("book", r#"{"flight":{"id":"KQ311"},"seats":2}"#)]);
        let replayed = assistant_with(vec![call("book", r#"{"flight":{"id":"KQ999"},"seats":2}"#)]);
        let detail =
            message_divergence(&[recorded], &[replayed]).expect("a change is a divergence");
        assert!(
            detail.contains("book.flight.id"),
            "names the path: {detail}"
        );
        assert!(
            detail.contains("KQ311"),
            "shows what was recorded: {detail}"
        );
        assert!(
            detail.contains("KQ999"),
            "shows what was replayed: {detail}"
        );
        // The unchanged argument must not be listed as noise.
        assert!(!detail.contains("seats"), "only the moved path: {detail}");
    }

    /// An argument that APPEARS is drift too, and the most likely shape of
    /// a real regression: an agent starts passing something extra.
    #[test]
    fn an_added_argument_is_reported_as_absent_before() {
        let recorded = assistant_with(vec![call("book", r#"{"id":"KQ311"}"#)]);
        let replayed = assistant_with(vec![call("book", r#"{"id":"KQ311","override":true}"#)]);
        let detail =
            message_divergence(&[recorded], &[replayed]).expect("an added argument diverges");
        assert!(detail.contains("book.override"), "{detail}");
        assert!(detail.contains("absent"), "{detail}");
    }

    /// A DIFFERENT tool is a different failure and must keep the name lists,
    /// which are the useful thing in that case.
    #[test]
    fn a_different_tool_still_reports_the_names() {
        let recorded = assistant_with(vec![call("book", "{}")]);
        let replayed = assistant_with(vec![call("cancel", "{}")]);
        let detail =
            message_divergence(&[recorded], &[replayed]).expect("a different tool diverges");
        assert!(detail.contains("tool calls changed"), "{detail}");
        assert!(
            detail.contains("book") && detail.contains("cancel"),
            "{detail}"
        );
    }

    /// Arguments that are not JSON cannot be diffed field by field. Say the
    /// whole thing moved rather than pretending to be precise about it.
    #[test]
    fn unparseable_arguments_report_the_whole_payload() {
        let recorded = assistant_with(vec![call("book", "not json")]);
        let replayed = assistant_with(vec![call("book", "also not json")]);
        let detail = message_divergence(&[recorded], &[replayed]).expect("still a divergence");
        assert!(detail.contains("book.arguments"), "{detail}");
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
