//! The LLM authoring loop: given a natural-language step and the live app's
//! scene graph, ask a model to choose ONE action against an element it can
//! see. The model must pick its target from the offered scene — it cannot
//! invent selectors — and the chosen action is then performed and verified
//! by the recorder exactly like a rules-authored one.

use serde::Deserialize;

use crate::rules::{ResolvedAction, Target};
use crate::{AgentError, ModelClient};

const SYSTEM_PROMPT: &str = "\
You are the authoring agent of flowproof, an end-to-end UI testing tool. \
You translate ONE natural-language test step into ONE concrete UI action \
against the app under test (a web page, a desktop window, ...). You are \
given the interactable elements of the current screen as JSON; each \
carries a `target` token. Rules:
- Respond with ONLY a JSON object, no prose, no code fences.
- The JSON shape is: {\"action\": \"click\"|\"type_text\"|\"assert_text\", \
\"target\": \"<target token of a listed element>\", \"text\": \"...\" (type_text only), \
\"expected\": \"...\" (assert_text only), \"contains\": true|false (assert_text only)}
- `target` MUST be copied verbatim from one of the listed elements. \
For assert_text you may also use \"surface\" to check everything readable \
on the current screen.
- Type exactly the text the step asks for; do not add anything.";

/// What the model must return. `target_css` is accepted as a legacy alias
/// for `target` so replies shaped for the old web-only contract still parse.
#[derive(Debug, Deserialize)]
struct AuthoredAction {
    action: String,
    #[serde(alias = "target_css")]
    target: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    expected: Option<String>,
    #[serde(default)]
    contains: Option<bool>,
}

/// Context for authoring one step.
pub struct AuthorContext<'a> {
    pub flow_name: &'a str,
    pub app: &'a str,
    pub url: Option<&'a str>,
    /// Intents of the steps already authored, in order.
    pub prior_steps: &'a [String],
    pub intent: &'a str,
    /// Scene JSON from the driver.
    pub scene: &'a str,
}

fn user_prompt(ctx: &AuthorContext<'_>) -> String {
    let prior = if ctx.prior_steps.is_empty() {
        "(none)".to_string()
    } else {
        ctx.prior_steps.join("; ")
    };
    format!(
        "Flow: {name}\nApp: {app}{url}\nSteps already performed: {prior}\n\
         Current step to perform: {intent}\n\nInteractable elements:\n{scene}",
        name = ctx.flow_name,
        app = ctx.app,
        url = ctx.url.map(|u| format!(" ({u})")).unwrap_or_default(),
        prior = prior,
        intent = ctx.intent,
        scene = ctx.scene,
    )
}

/// The grounding set: one TARGET TOKEN per scene element. Modern drivers
/// emit a `target` token directly (`css:…`, `id:…`, `text:…`); a legacy
/// web scene that only carries a `css` key is lifted into `css:<sel>`.
fn scene_targets(scene: &str) -> Vec<String> {
    serde_json::from_str::<Vec<serde_json::Value>>(scene)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            e["target"]
                .as_str()
                .map(str::to_string)
                .or_else(|| e["css"].as_str().map(|css| format!("css:{css}")))
        })
        .collect()
}

fn parse_and_ground(
    reply: &str,
    targets: &[String],
    intent: &str,
) -> Result<ResolvedAction, String> {
    // Tolerate models that wrap JSON in a code fence despite instructions.
    let trimmed = reply
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let authored: AuthoredAction =
        serde_json::from_str(trimmed).map_err(|e| format!("reply is not valid JSON: {e}"))?;

    let token = authored.target.trim();
    let asserting = authored.action == "assert_text";
    // "body" is the legacy web spelling of the whole readable surface.
    let target = if token == "surface" || (asserting && token == "body") {
        if !asserting {
            return Err("'surface' is only a valid target for assert_text".into());
        }
        Target::Surface
    } else if targets.iter().any(|t| t == token) {
        crate::rules::target_from_token(token)
            .ok_or_else(|| format!("listed target '{token}' is not a well-formed token"))?
    } else if targets.iter().any(|t| t == &format!("css:{token}")) {
        // Old-style reply echoing a bare css selector from a legacy scene.
        Target::css(token)
    } else {
        return Err(format!(
            "target '{token}' is not one of the listed elements"
        ));
    };
    match authored.action.as_str() {
        "click" => Ok(ResolvedAction::Press {
            target,
            label: intent.to_string(),
        }),
        "type_text" => {
            let text = authored
                .text
                .filter(|t| !t.is_empty())
                .ok_or("type_text needs a non-empty 'text'")?;
            Ok(ResolvedAction::TypeText { target, text })
        }
        "assert_text" => {
            let expected = authored
                .expected
                .filter(|t| !t.is_empty())
                .ok_or("assert_text needs a non-empty 'expected'")?;
            let matcher = if authored.contains.unwrap_or(true) {
                crate::rules::TextMatch::Contains
            } else {
                crate::rules::TextMatch::Equals
            };
            Ok(ResolvedAction::AssertText {
                target,
                expected,
                matcher,
                timeout_ms: crate::rules::ASSERT_TIMEOUT_MS,
            })
        }
        other => Err(format!("unknown action '{other}'")),
    }
}

/// Author one step. One retry with the failure appended, then a clear error.
pub fn author_step<C: ModelClient>(
    client: &mut C,
    ctx: &AuthorContext<'_>,
) -> Result<ResolvedAction, AgentError> {
    let targets = scene_targets(ctx.scene);
    if targets.is_empty() {
        return Err(AgentError::Authoring {
            step: ctx.intent.to_string(),
            reason: "scene has no interactable elements".into(),
        });
    }
    let prompt = user_prompt(ctx);
    let mut last_error = String::new();
    for attempt in 0..2 {
        let user = if attempt == 0 {
            prompt.clone()
        } else {
            format!("{prompt}\n\nYour previous reply was rejected: {last_error}. Reply again with ONLY the corrected JSON object.")
        };
        let reply = client.complete(SYSTEM_PROMPT, &user)?;
        match parse_and_ground(&reply, &targets, ctx.intent) {
            Ok(action) => return Ok(action),
            Err(reason) => last_error = reason,
        }
    }
    Err(AgentError::Authoring {
        step: ctx.intent.to_string(),
        reason: last_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted {
        replies: Vec<String>,
        calls: usize,
    }

    impl ModelClient for Scripted {
        fn complete(&mut self, _system: &str, _user: &str) -> Result<String, AgentError> {
            let reply = self.replies.get(self.calls).cloned().unwrap_or_default();
            self.calls += 1;
            Ok(reply)
        }

        fn identity(&self) -> (String, String) {
            ("scripted".into(), "test".into())
        }
    }

    const SCENE: &str = r##"[
        {"target":"css:#name","css":"#name","tag":"input","label":"Your name"},
        {"target":"css:#greet","css":"#greet","tag":"button","text":"Greet"}
    ]"##;

    /// A desktop (UIA) scene: native ids and text anchors, no css anywhere.
    const UIA_SCENE: &str = r##"[
        {"target":"id:15","control_type":"Edit","text":"Text editor"},
        {"target":"text:Close","control_type":"Button","text":"Close"}
    ]"##;

    fn ctx<'a>() -> AuthorContext<'a> {
        AuthorContext {
            flow_name: "Greet",
            app: "web",
            url: Some("file:///greeter.html"),
            prior_steps: &[],
            intent: "Put Ada into the box labelled name",
            scene: SCENE,
        }
    }

    #[test]
    fn happy_path_grounds_to_listed_element() {
        let mut client = Scripted {
            replies: vec![r##"{"action":"type_text","target":"css:#name","text":"Ada"}"##.into()],
            calls: 0,
        };
        let action = author_step(&mut client, &ctx()).expect("authored");
        assert_eq!(
            action,
            ResolvedAction::TypeText {
                target: Target::css("#name"),
                text: "Ada".into()
            }
        );
        assert_eq!(client.calls, 1);
    }

    #[test]
    fn uia_scene_grounds_native_id_and_text_tokens() {
        let mut client = Scripted {
            replies: vec![r##"{"action":"type_text","target":"id:15","text":"hello"}"##.into()],
            calls: 0,
        };
        let action = author_step(
            &mut client,
            &AuthorContext {
                app: "notepad",
                url: None,
                scene: UIA_SCENE,
                ..ctx()
            },
        )
        .expect("authored");
        assert_eq!(
            action,
            ResolvedAction::TypeText {
                target: Target::id("15"),
                text: "hello".into()
            }
        );

        let mut client = Scripted {
            replies: vec![r##"{"action":"click","target":"text:Close"}"##.into()],
            calls: 0,
        };
        let action = author_step(
            &mut client,
            &AuthorContext {
                app: "notepad",
                url: None,
                scene: UIA_SCENE,
                ..ctx()
            },
        )
        .expect("authored");
        assert!(
            matches!(action, ResolvedAction::Press { ref target, .. } if *target == Target::text("Close"))
        );
    }

    #[test]
    fn legacy_reply_and_scene_shapes_still_ground() {
        // Old-style scene (css key only) + old-style reply (target_css field,
        // bare selector): both sides of the legacy contract keep working.
        let mut client = Scripted {
            replies: vec![r##"{"action":"type_text","target_css":"#name","text":"Ada"}"##.into()],
            calls: 0,
        };
        let legacy_scene = r##"[{"css":"#name","tag":"input"}]"##;
        let action = author_step(
            &mut client,
            &AuthorContext {
                scene: legacy_scene,
                ..ctx()
            },
        )
        .expect("authored");
        assert_eq!(
            action,
            ResolvedAction::TypeText {
                target: Target::css("#name"),
                text: "Ada".into()
            }
        );
    }

    #[test]
    fn invalid_json_gets_one_retry() {
        let mut client = Scripted {
            replies: vec![
                "sure! here's the JSON you asked for".into(),
                r##"```json
{"action":"click","target":"css:#greet"}
```"##
                    .into(),
            ],
            calls: 0,
        };
        let action = author_step(&mut client, &ctx()).expect("authored on retry");
        assert!(matches!(action, ResolvedAction::Press { .. }));
        assert_eq!(client.calls, 2);
    }

    #[test]
    fn invented_selectors_are_rejected() {
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"click","target":"css:#made-up"}"##.into(),
                r##"{"action":"click","target":"id:not-listed"}"##.into(),
            ],
            calls: 0,
        };
        let err = author_step(&mut client, &ctx()).expect_err("ungrounded must fail");
        assert!(err.to_string().contains("not one of the listed elements"));
        assert_eq!(client.calls, 2, "exactly one retry");
    }

    #[test]
    fn assert_on_surface_is_allowed() {
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"assert_text","target":"surface","expected":"Hello, Ada","contains":true}"##
                    .into(),
            ],
            calls: 0,
        };
        let action = author_step(&mut client, &ctx()).expect("authored");
        assert_eq!(
            action,
            ResolvedAction::AssertText {
                target: Target::Surface,
                expected: "Hello, Ada".into(),
                matcher: crate::rules::TextMatch::Contains,
                timeout_ms: crate::rules::ASSERT_TIMEOUT_MS,
            }
        );
    }

    #[test]
    fn legacy_body_alias_maps_to_surface() {
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"assert_text","target_css":"body","expected":"Hello, Ada"}"##.into(),
            ],
            calls: 0,
        };
        let action = author_step(&mut client, &ctx()).expect("authored");
        assert!(
            matches!(action, ResolvedAction::AssertText { ref target, .. } if *target == Target::Surface)
        );
    }

    #[test]
    fn surface_is_assert_only() {
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"click","target":"surface"}"##.into(),
                r##"{"action":"click","target":"surface"}"##.into(),
            ],
            calls: 0,
        };
        let err = author_step(&mut client, &ctx()).expect_err("surface click must fail");
        assert!(err
            .to_string()
            .contains("only a valid target for assert_text"));
    }
}
