//! Structured "this step could not be authored" payloads.
//!
//! flowproof deliberately has no in-loop clarification: when neither the
//! rules nor the model can turn a step into a grounded action, recording
//! stops and the *driving* agent (a human, or an MCP caller like DataMaker's
//! agent) resolves the ambiguity — by consulting an external source of
//! truth, rewriting the vague step into concrete grammar, and re-recording.
//! This module is the machine-readable half of that loop: everything the
//! driving agent needs to know *what* was ambiguous and *what the live
//! screen offered* at the moment authoring gave up.

use serde::{Deserialize, Serialize};

/// Where authoring gave up on the step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClarifyStage {
    /// The rules couldn't parse the step and no model backend is
    /// configured — the payload carries the rules diagnostic.
    NoModel,
    /// The model was consulted (with one self-correcting retry) and still
    /// couldn't ground the step to a listed element.
    Model,
}

impl std::fmt::Display for ClarifyStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ClarifyStage::NoModel => "no model backend",
            ClarifyStage::Model => "model could not ground",
        })
    }
}

/// One interactable element from the live scene at the moment of failure —
/// the same grounding set the author saw, in structured form so the driving
/// agent can enumerate fields ("which of these should the vague step
/// change?") instead of re-scraping the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneElement {
    /// Neutral target token (`css:…`, `id:…`, `text:…`) — usable verbatim
    /// as a quoted target in a rewritten step.
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Associated label / aria-label / placeholder, when the driver saw one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Input type (`text`, `password`, …), when applicable.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
}

/// The full clarification payload. Text fields keep `${VAR}` references
/// raw — nothing here ever holds a resolved secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clarification {
    /// The spec step that could not be authored, verbatim.
    pub step: String,
    /// 0-based index of that step in the spec.
    pub step_index: usize,
    pub stage: ClarifyStage,
    /// Human-readable diagnostic from the stage that gave up.
    pub reason: String,
    /// The rules diagnostic, when the model stage was reached (the rules
    /// failed first — their error often names the accepted forms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_error: Option<String>,
    /// Intents already performed, in order — the app is left in the state
    /// AFTER these, which is the state `scene` describes.
    pub completed_steps: Vec<String>,
    /// Interactable inventory of the live screen (may be empty when the
    /// driver has no scene support).
    pub scene: Vec<SceneElement>,
    /// What to do next, for agents that don't know the loop.
    pub hint: String,
}

impl Clarification {
    pub const HINT: &'static str = "Rewrite the step using the grammar in docs/authoring.md, \
         targeting a listed element (quote its label, or use its target token verbatim), \
         then re-record. Consult your data source for domain questions the scene cannot \
         answer (e.g. which fields are required).";
}

/// Parse a driver scene JSON into the structured inventory. Mirrors the
/// author's grounding rules (`author.rs::scene_targets`): a modern element
/// carries a `target` token; a legacy web element with only a `css` key is
/// lifted to `css:<sel>`. Anything unparseable yields an empty inventory —
/// a clarification without a scene is still useful.
pub fn scene_inventory(scene: &str) -> Vec<SceneElement> {
    serde_json::from_str::<Vec<serde_json::Value>>(scene)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            let target = e["target"]
                .as_str()
                .map(str::to_string)
                .or_else(|| e["css"].as_str().map(|css| format!("css:{css}")))?;
            let field = |key: &str| e[key].as_str().map(str::to_string);
            Some(SceneElement {
                target,
                tag: field("tag"),
                label: field("label"),
                text: field("text"),
                input_type: field("type"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_web_shaped_scene() {
        let scene = r##"[
            {"target": "css:#price", "css": "#price", "tag": "input",
             "type": "text", "label": "Net Price"},
            {"target": "text:Save", "tag": "button", "text": "Save"}
        ]"##;
        let inv = scene_inventory(scene);
        assert_eq!(inv.len(), 2);
        assert_eq!(inv[0].target, "css:#price");
        assert_eq!(inv[0].label.as_deref(), Some("Net Price"));
        assert_eq!(inv[0].input_type.as_deref(), Some("text"));
        assert_eq!(inv[1].target, "text:Save");
        assert_eq!(inv[1].text.as_deref(), Some("Save"));
        assert!(inv[1].label.is_none());
    }

    #[test]
    fn lifts_legacy_bare_css_entries() {
        let inv = scene_inventory(r##"[{"css": "#total", "tag": "span"}]"##);
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0].target, "css:#total");
    }

    #[test]
    fn garbage_scene_yields_empty_inventory() {
        assert!(scene_inventory("not json").is_empty());
        assert!(scene_inventory("[{\"no_target\": 1}]").is_empty());
    }

    #[test]
    fn payload_serializes_to_the_documented_field_names() {
        let c = Clarification {
            step: "make required field changes".into(),
            step_index: 4,
            stage: ClarifyStage::Model,
            reason: "target 'invented' is not one of the listed elements".into(),
            rules_error: Some("no rule matches".into()),
            completed_steps: vec!["Press the \"Edit\" button".into()],
            scene: vec![SceneElement {
                target: "css:#price".into(),
                tag: Some("input".into()),
                label: Some("Net Price".into()),
                text: None,
                input_type: Some("text".into()),
            }],
            hint: Clarification::HINT.into(),
        };
        let v = serde_json::to_value(&c).expect("serializes");
        assert_eq!(v["stage"], "model");
        assert_eq!(v["step_index"], 4);
        assert_eq!(v["scene"][0]["type"], "text");
        assert_eq!(v["scene"][0]["label"], "Net Price");
        // Optional fields absent when None.
        assert!(v["scene"][0].get("text").is_none());
        // Round-trips.
        let back: Clarification = serde_json::from_value(v).expect("deserializes");
        assert_eq!(back, c);
    }
}
