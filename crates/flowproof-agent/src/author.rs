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
against the current page. You are given the interactable elements of the \
page as JSON (each with a `css` selector). Rules:
- Respond with ONLY a JSON object, no prose, no code fences.
- The JSON shape is: {\"action\": \"click\"|\"type_text\"|\"assert_text\", \
\"target_css\": \"<css of a listed element>\", \"text\": \"...\" (type_text only), \
\"expected\": \"...\" (assert_text only), \"contains\": true|false (assert_text only)}
- target_css MUST be copied verbatim from one of the listed elements. \
For assert_text you may also use \"body\" to check the whole page.
- Type exactly the text the step asks for; do not add anything.";

/// What the model must return.
#[derive(Debug, Deserialize)]
struct AuthoredAction {
    action: String,
    target_css: String,
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

fn scene_selectors(scene: &str) -> Vec<String> {
    serde_json::from_str::<Vec<serde_json::Value>>(scene)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| e["css"].as_str().map(str::to_string))
        .collect()
}

fn parse_and_ground(
    reply: &str,
    selectors: &[String],
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

    let grounded = selectors.iter().any(|s| s == &authored.target_css)
        || (authored.action == "assert_text" && authored.target_css == "body");
    if !grounded {
        return Err(format!(
            "target_css '{}' is not one of the listed elements",
            authored.target_css
        ));
    }

    let target = Target::css(authored.target_css);
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
            Ok(ResolvedAction::AssertText {
                target,
                expected,
                contains: authored.contains.unwrap_or(true),
                numeric: false,
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
    let selectors = scene_selectors(ctx.scene);
    if selectors.is_empty() {
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
        match parse_and_ground(&reply, &selectors, ctx.intent) {
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
        {"css":"#name","tag":"input","label":"Your name"},
        {"css":"#greet","tag":"button","text":"Greet"}
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
            replies: vec![r##"{"action":"type_text","target_css":"#name","text":"Ada"}"##.into()],
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
    fn invalid_json_gets_one_retry() {
        let mut client = Scripted {
            replies: vec![
                "sure! here's the JSON you asked for".into(),
                r##"```json
{"action":"click","target_css":"#greet"}
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
                r##"{"action":"click","target_css":"#made-up"}"##.into(),
                r##"{"action":"click","target_css":"#also-made-up"}"##.into(),
            ],
            calls: 0,
        };
        let err = author_step(&mut client, &ctx()).expect_err("ungrounded must fail");
        assert!(err.to_string().contains("not one of the listed elements"));
        assert_eq!(client.calls, 2, "exactly one retry");
    }

    #[test]
    fn assert_on_body_is_allowed() {
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"assert_text","target_css":"body","expected":"Hello, Ada","contains":true}"##
                    .into(),
            ],
            calls: 0,
        };
        let action = author_step(&mut client, &ctx()).expect("authored");
        assert_eq!(
            action,
            ResolvedAction::AssertText {
                target: Target::css("body"),
                expected: "Hello, Ada".into(),
                contains: true,
                numeric: false,
                timeout_ms: crate::rules::ASSERT_TIMEOUT_MS,
            }
        );
    }
}
