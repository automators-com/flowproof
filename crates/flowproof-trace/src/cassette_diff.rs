//! Diffing an old trajectory against a re-recorded one: the heal moment.
//!
//! Re-recording is how an agent test survives a deliberate change - a new
//! prompt template, a new tool, a model upgrade. It is also how a test
//! silently stops testing anything, if the reviewer accepts a new
//! recording without understanding what moved. So the diff's job is not
//! to list bytes. It is to sort what changed into what a reviewer must
//! defend and what they can wave through.
//!
//! Two axes decide that.
//!
//! **What the agent DID versus what it was TOLD.** A changed tool call is
//! a behavior change: the agent decided differently. A changed prompt is
//! an input change: someone edited a template, and the agent may have
//! behaved identically given it. Both are worth seeing, but only one is a
//! claim about the agent, and mixing them in one flat list is how the
//! important one gets lost among forty prompt edits.
//!
//! **Whether an assertion covered it.** An argument the spec asserts is
//! load-bearing; the cassette pins every other argument too, incidentally.
//! A reviewer should spend their attention on the first kind, so the
//! caller passes in which paths its expectations mention.

use crate::cassette::{Cassette, Message, ToolCall};

/// One difference between two recordings of the same flow.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// 0-based turn, rendered 1-based for humans.
    pub turn: usize,
    pub kind: ChangeKind,
    /// Does an assertion in the spec depend on this? A reviewer must
    /// defend these; the rest are incidental values the cassette happened
    /// to pin.
    pub asserted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChangeKind {
    /// The agent asked for a different set of tools. The strongest signal
    /// in a diff: its behavior changed.
    ToolsCalled {
        before: Vec<String>,
        after: Vec<String>,
    },
    /// Same tool, different arguments. `path` is the dotted argument that
    /// moved, so a reviewer sees `flight.id` rather than two JSON blobs.
    Argument {
        tool: String,
        path: String,
        before: String,
        after: String,
    },
    /// What the agent was told changed: a prompt edit, a new system
    /// message, a tool added to the request.
    Input { detail: String },
    /// The final reply changed.
    Reply { before: String, after: String },
    /// The trajectory got longer or shorter - the agent took a different
    /// number of steps.
    Length { before: usize, after: usize },
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let turn = self.turn + 1;
        let mark = if self.asserted { " [asserted]" } else { "" };
        match &self.kind {
            ChangeKind::ToolsCalled { before, after } => write!(
                f,
                "turn {turn}: called [{}] instead of [{}]{mark}",
                after.join(", "),
                before.join(", ")
            ),
            ChangeKind::Argument {
                tool,
                path,
                before,
                after,
            } => write!(f, "turn {turn}: {tool}.{path} {before} -> {after}{mark}"),
            ChangeKind::Input { detail } => write!(f, "turn {turn}: {detail}"),
            ChangeKind::Reply { before, after } => {
                write!(
                    f,
                    "reply changed{mark}\n  before: {before}\n  after:  {after}"
                )
            }
            ChangeKind::Length { before, after } => {
                write!(f, "the agent took {after} model calls, was {before}")
            }
        }
    }
}

/// A sorted view of what a re-record changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrajectoryDiff {
    /// What the agent decided differently. Review these.
    pub behavior: Vec<Change>,
    /// What the agent was told differently. Usually the cause.
    pub inputs: Vec<Change>,
}

impl TrajectoryDiff {
    pub fn is_empty(&self) -> bool {
        self.behavior.is_empty() && self.inputs.is_empty()
    }

    /// Does anything here touch something the spec asserts?
    ///
    /// The one question worth answering automatically. An unasserted
    /// behavior change still deserves eyes, but an ASSERTED one means the
    /// re-record moved a fact the spec claims, and accepting it silently
    /// would rewrite the test to match the code.
    pub fn touches_an_assertion(&self) -> bool {
        self.behavior
            .iter()
            .chain(&self.inputs)
            .any(|change| change.asserted)
    }
}

impl std::fmt::Display for TrajectoryDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return write!(f, "the trajectory is unchanged");
        }
        // Behavior first, always. It is the part that is about the agent.
        if !self.behavior.is_empty() {
            writeln!(f, "what the agent did differently:")?;
            for change in &self.behavior {
                writeln!(f, "  {change}")?;
            }
        }
        if !self.inputs.is_empty() {
            writeln!(f, "what it was told differently:")?;
            for change in &self.inputs {
                writeln!(f, "  {change}")?;
            }
        }
        Ok(())
    }
}

/// Compare two recordings of the same flow.
///
/// `asserted_paths` names the `tool.path` pairs the spec's expectations
/// mention, e.g. `create_booking.flight.id`. A change to one of those is
/// flagged, because it is a fact the spec claims rather than a value the
/// cassette merely pinned.
pub fn diff(before: &Cassette, after: &Cassette, asserted_paths: &[String]) -> TrajectoryDiff {
    let mut out = TrajectoryDiff::default();

    if before.len() != after.len() {
        out.behavior.push(Change {
            turn: before.len().min(after.len()),
            kind: ChangeKind::Length {
                before: before.len(),
                after: after.len(),
            },
            asserted: false,
        });
    }

    for (turn, (old, new)) in before.turns.iter().zip(&after.turns).enumerate() {
        // What it was told.
        if old.request.model != new.request.model {
            out.inputs.push(Change {
                turn,
                kind: ChangeKind::Input {
                    detail: format!("model {} -> {}", old.request.model, new.request.model),
                },
                asserted: false,
            });
        }
        if old.request.tools != new.request.tools {
            out.inputs.push(Change {
                turn,
                kind: ChangeKind::Input {
                    detail: format!(
                        "tools offered [{}] -> [{}]",
                        old.request.tools.join(", "),
                        new.request.tools.join(", ")
                    ),
                },
                asserted: false,
            });
        }
        if let Some(detail) = prompt_change(&old.request.messages, &new.request.messages) {
            out.inputs.push(Change {
                turn,
                kind: ChangeKind::Input { detail },
                asserted: false,
            });
        }

        // What it did.
        let (old_calls, new_calls) = (
            &old.response.message.tool_calls,
            &new.response.message.tool_calls,
        );
        let names = |calls: &[ToolCall]| calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        if names(old_calls) != names(new_calls) {
            out.behavior.push(Change {
                turn,
                kind: ChangeKind::ToolsCalled {
                    before: names(old_calls),
                    after: names(new_calls),
                },
                // A tool set that changed is worth a reviewer's attention
                // whether or not an expectation named it.
                asserted: true,
            });
            continue; // arguments of different calls are not comparable
        }
        for (old_call, new_call) in old_calls.iter().zip(new_calls) {
            for (path, before, after) in argument_changes(old_call, new_call) {
                let asserted = asserted_paths
                    .iter()
                    .any(|p| *p == format!("{}.{}", new_call.name, path));
                out.behavior.push(Change {
                    turn,
                    kind: ChangeKind::Argument {
                        tool: new_call.name.clone(),
                        path,
                        before,
                        after,
                    },
                    asserted,
                });
            }
        }
    }

    if before.reply() != after.reply() {
        out.behavior.push(Change {
            turn: after.len().saturating_sub(1),
            kind: ChangeKind::Reply {
                before: before.reply().unwrap_or("(none)").to_string(),
                after: after.reply().unwrap_or("(none)").to_string(),
            },
            asserted: asserted_paths.iter().any(|p| p == "reply"),
        });
    }
    out
}

fn prompt_change(before: &[Message], after: &[Message]) -> Option<String> {
    if before.len() != after.len() {
        let roles = |m: &[Message]| {
            m.iter()
                .map(|m| m.role.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Some(format!(
            "conversation [{}] -> [{}]",
            roles(before),
            roles(after)
        ));
    }
    for (i, (old, new)) in before.iter().zip(after).enumerate() {
        if old.content != new.content {
            return Some(format!("message {i} ({}) text changed", old.role));
        }
    }
    None
}

/// Which argument PATHS moved between two calls to the same tool.
///
/// Field-level on purpose. Two JSON blobs side by side make a reviewer do
/// the diffing; `flight.id KQ311 -> KQ999` is the finding itself.
fn argument_changes(before: &ToolCall, after: &ToolCall) -> Vec<(String, String, String)> {
    let (Some(old), Some(new)) = (before.arguments_json(), after.arguments_json()) else {
        // Unparseable arguments cannot be compared field by field. Say
        // the whole thing moved rather than pretending to be precise.
        return if before.arguments == after.arguments {
            Vec::new()
        } else {
            vec![(
                "arguments".into(),
                before.arguments.clone(),
                after.arguments.clone(),
            )]
        };
    };
    let mut changes = Vec::new();
    walk(&old, &new, String::new(), &mut changes);
    changes.sort();
    changes
}

fn walk(
    before: &serde_json::Value,
    after: &serde_json::Value,
    path: String,
    out: &mut Vec<(String, String, String)>,
) {
    use serde_json::Value;
    match (before, after) {
        (Value::Object(a), Value::Object(b)) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => walk(x, y, child, out),
                    (Some(x), None) => out.push((child, render(x), "(absent)".into())),
                    (None, Some(y)) => out.push((child, "(absent)".into(), render(y))),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                walk(x, y, format!("{path}.{i}"), out);
            }
        }
        (a, b) if a != b => out.push((path, render(a), render(b))),
        _ => {}
    }
}

fn render(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cassette::{Turn, TurnRequest, TurnResponse};

    fn call(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: format!("call_{name}"),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    fn turn(prompt: &str, calls: Vec<ToolCall>, reply: Option<&str>) -> Turn {
        Turn {
            request: TurnRequest {
                model: "gpt-4o".into(),
                messages: vec![Message::new("user", prompt)],
                tools: vec!["search_flights".into(), "create_booking".into()],
            },
            response: TurnResponse {
                message: Message {
                    role: "assistant".into(),
                    content: reply.map(str::to_string),
                    tool_calls: calls,
                    tool_call_id: None,
                },
            },
        }
    }

    fn booking(flight: &str) -> Cassette {
        Cassette {
            turns: vec![
                turn(
                    "Book a flight",
                    vec![call(
                        "create_booking",
                        &format!(r#"{{"flight":{{"id":"{flight}"}}}}"#),
                    )],
                    None,
                ),
                turn("Book a flight", vec![], Some("Booked.")),
            ],
        }
    }

    #[test]
    fn an_unchanged_re_record_says_so() {
        let diff = diff(&booking("KQ311"), &booking("KQ311"), &[]);
        assert!(diff.is_empty());
        assert_eq!(diff.to_string(), "the trajectory is unchanged");
        assert!(!diff.touches_an_assertion());
    }

    /// The heal moment: an argument moved. The reviewer needs the PATH,
    /// not two JSON blobs, and needs to know the spec depends on it.
    #[test]
    fn a_moved_argument_is_reported_by_path_and_flagged_when_asserted() {
        let asserted = vec!["create_booking.flight.id".to_string()];
        let diff = diff(&booking("KQ311"), &booking("KQ999"), &asserted);

        assert_eq!(diff.behavior.len(), 1, "{diff}");
        assert!(diff.inputs.is_empty(), "nothing about the input moved");
        let rendered = diff.behavior[0].to_string();
        assert!(rendered.contains("create_booking.flight.id"), "{rendered}");
        assert!(rendered.contains("KQ311 -> KQ999"), "{rendered}");
        assert!(rendered.contains("[asserted]"), "{rendered}");
        assert!(diff.touches_an_assertion());

        // The same change, unasserted, is still reported - just not
        // flagged as a fact the spec claims.
        let diff = diff_unasserted();
        assert_eq!(diff.behavior.len(), 1);
        assert!(!diff.touches_an_assertion());
        assert!(!diff.behavior[0].to_string().contains("[asserted]"));
    }

    fn diff_unasserted() -> TrajectoryDiff {
        diff(&booking("KQ311"), &booking("KQ999"), &[])
    }

    /// The separation that makes a diff readable: a prompt edit is an
    /// INPUT change, and must not be listed among the agent's decisions.
    #[test]
    fn inputs_and_behavior_are_separated() {
        let mut edited = booking("KQ311");
        edited.turns[0].request.messages = vec![Message::new("user", "Book a flight, please")];
        let diff = diff(&booking("KQ311"), &edited, &[]);

        assert!(diff.behavior.is_empty(), "the agent did the same thing");
        assert_eq!(diff.inputs.len(), 1);
        assert!(diff.inputs[0].to_string().contains("text changed"));

        let rendered = diff.to_string();
        assert!(
            rendered.contains("what it was told differently"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("what the agent did differently"),
            "no empty section: {rendered}"
        );
    }

    /// A changed tool SET is the strongest signal in a diff, and always
    /// worth a reviewer's attention.
    #[test]
    fn a_changed_tool_set_is_behavior_and_always_flagged() {
        let mut different = booking("KQ311");
        different.turns[0].response.message.tool_calls = vec![call("charge_card", "{}")];
        let diff = diff(&booking("KQ311"), &different, &[]);

        assert_eq!(diff.behavior.len(), 1, "{diff}");
        let rendered = diff.behavior[0].to_string();
        assert!(rendered.contains("called [charge_card]"), "{rendered}");
        assert!(
            rendered.contains("instead of [create_booking]"),
            "{rendered}"
        );
        assert!(diff.touches_an_assertion(), "always worth defending");
    }

    /// Behavior is printed before inputs, because it is the part that is
    /// about the agent.
    #[test]
    fn behavior_is_reported_before_inputs() {
        let mut changed = booking("KQ999");
        changed.turns[0].request.messages = vec![Message::new("user", "Book a flight, please")];
        let rendered = diff(&booking("KQ311"), &changed, &[]).to_string();
        let did = rendered
            .find("what the agent did")
            .expect("behavior section");
        let told = rendered.find("what it was told").expect("inputs section");
        assert!(did < told, "{rendered}");
    }

    #[test]
    fn a_different_number_of_steps_is_behavior() {
        let mut shorter = booking("KQ311");
        shorter.turns.pop();
        let diff = diff(&booking("KQ311"), &shorter, &[]);
        let rendered = diff.to_string();
        assert!(rendered.contains("took 1 model calls, was 2"), "{rendered}");
    }

    #[test]
    fn a_changed_reply_is_reported_with_both_sides() {
        let mut changed = booking("KQ311");
        changed.turns[1].response.message.content = Some("All set.".into());
        let diff = diff(&booking("KQ311"), &changed, &["reply".to_string()]);
        let rendered = diff.to_string();
        assert!(rendered.contains("Booked."), "{rendered}");
        assert!(rendered.contains("All set."), "{rendered}");
        assert!(diff.touches_an_assertion());
    }

    /// Added and removed argument keys are changes too, and naming them
    /// "(absent)" beats omitting them.
    #[test]
    fn added_and_removed_arguments_are_named() {
        let before = Cassette {
            turns: vec![turn("go", vec![call("book", r#"{"seat":"12A"}"#)], None)],
        };
        let after = Cassette {
            turns: vec![turn("go", vec![call("book", r#"{"meal":"veg"}"#)], None)],
        };
        let rendered = diff(&before, &after, &[]).to_string();
        assert!(rendered.contains("book.meal (absent) -> veg"), "{rendered}");
        assert!(rendered.contains("book.seat 12A -> (absent)"), "{rendered}");
    }

    /// Arguments that will not parse cannot be compared field by field.
    /// Report the whole thing rather than pretending to be precise.
    #[test]
    fn unparseable_arguments_are_reported_whole() {
        let before = Cassette {
            turns: vec![turn("go", vec![call("book", "{broken")], None)],
        };
        let after = Cassette {
            turns: vec![turn("go", vec![call("book", "{also broken")], None)],
        };
        let diff = diff(&before, &after, &[]);
        assert_eq!(diff.behavior.len(), 1, "{diff}");
        assert!(diff.behavior[0].to_string().contains("book.arguments"));
    }
}
