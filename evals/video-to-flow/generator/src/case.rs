//! A case file (`evals/video-to-flow/cases/*.yaml`): which fixture, how it is
//! deformed, and the actions performed on it.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    /// What the case is called, and the expected flow's `name:`.
    pub name: String,
    /// What the case exists to catch, for the README and the scoreboard.
    pub catches: String,
    /// A page in `evals/video-to-flow/fixture/`.
    pub fixture: String,
    #[serde(default = "web")]
    pub app: String,
    #[serde(default)]
    pub viewport: Viewport,
    /// A menu bar along the top, drawn by `chrome.html`.
    #[serde(default)]
    pub menubar: bool,
    /// A taskbar along the bottom with a clock that ticks every second.
    #[serde(default)]
    pub taskbar: bool,
    /// Whether the pointer is drawn: a slideshow has none.
    #[serde(default = "yes")]
    pub pointer: bool,
    /// Still time after every action.
    #[serde(default = "gap")]
    pub gap_ms: u64,
    /// Time between two keys.
    #[serde(default = "key")]
    pub key_ms: u64,
    /// The flow starts with `Go to` the fixture, which the draft is given
    /// as `--url`.
    #[serde(default = "yes")]
    pub go_to: bool,
    /// The draft refuses the recording as not showing an app.
    #[serde(default)]
    pub not_app: bool,
    /// `- click: "#id"` rather than YAML's `- !click "#id"`.
    #[serde(with = "serde_yaml::with::singleton_map_recursive")]
    pub actions: Vec<Action>,
}

#[derive(Debug, Deserialize, Default, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    /// Device pixels per CSS pixel: 2 records a Retina screen.
    #[serde(default = "one")]
    pub scale: f64,
}

impl Viewport {
    pub fn or_default(self) -> Self {
        if self.width == 0 {
            Self {
                width: 1280,
                height: 800,
                scale: 1.0,
            }
        } else {
            self
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Click a button, a tab, anything with visible text.
    Click(String),
    /// Click a field and type into it.
    Type(TypeInto),
    /// Type the start of a value into a field, then click the entry the
    /// list under it offers.
    Pick(Pick),
    /// Press a key (`Enter`).
    Press(String),
    /// Type into whatever field has the focus, with no click: a
    /// keyboard-only recording.
    Keys(Keys),
    /// Move the focus to the next field. Not a step: the next Type names
    /// its field, and recording finds it.
    Tab,
    /// Nothing happens for this long.
    Wait(u64),
    /// The fake mail client covers the app, something is clicked in it,
    /// and it goes again: an action in another app.
    Mail(Mail),
    /// The slideshow moves on by itself.
    Slide,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeInto {
    pub field: String,
    pub text: String,
    /// A masked field: the draft must write `${<secret>}`, and the typed
    /// text must appear nowhere in it.
    #[serde(default)]
    pub secret: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keys {
    pub text: String,
    #[serde(default)]
    pub secret: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pick {
    pub field: String,
    pub text: String,
    pub entry: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mail {
    pub click: String,
    pub for_ms: u64,
}

fn web() -> String {
    "web".into()
}
fn yes() -> bool {
    true
}
fn gap() -> u64 {
    1200
}
fn key() -> u64 {
    200
}
fn one() -> f64 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_case_reads_with_defaults() {
        let case: Case = serde_yaml::from_str(
            "name: smoke\ncatches: x\nfixture: app.html\nactions:\n  - click: \"#sign-in\"\n  \
             - type: {field: \"#password\", text: abc, secret: PASSWORD}\n  - wait: 500\n  - slide\n",
        )
        .expect("parses");
        assert_eq!(case.viewport.or_default().width, 1280);
        assert!(case.pointer && case.go_to && !case.not_app);
        assert_eq!(case.actions.len(), 4);
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let err = serde_yaml::from_str::<Case>(
            "name: x\ncatches: x\nfixture: app.html\nidle: 3\nactions: []\n",
        );
        assert!(err.is_err());
    }
}
