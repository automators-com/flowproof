//! SPIKE: draft `.flow.yaml` authoring from a screen-recording video.
//!
//! docs/recording.md and issue #227 both say video should never be a
//! machine-parsed input - video is the human surface, not the machine
//! surface. This exists anyway, as a scoped experiment: extract key
//! frames, ask a vision model what single action bridges each pair (in
//! flowproof's existing step vocabulary), and write a DRAFT. It never
//! infers `assert:` steps - a recording shows behaviour, not a belief
//! about correctness - so a human adds at least one before the draft
//! can survive the live `flowproof record` pass every spec still needs.
//!
//! A transition video cannot explain (most often an invisible keypress
//! like Enter) is never silently dropped and never guessed at either -
//! it becomes a flagged, freeform step (see [`FLAGGED_MARKER`]) that the
//! *existing* live authoring agent resolves against the real screen
//! during that same `record` pass, the same way it already resolves any
//! other freeform step. Two OS-level ways to detect the keypress
//! directly instead (`GetAsyncKeyState` polling, then a `WH_KEYBOARD_LL`
//! hook — see [`flowproof_driver::input_log`]) were tried first and both
//! failed to see a real physical keystroke in a UTM VM, apparently
//! because that hypervisor's virtual keyboard bypasses the input layer
//! both depend on; [`record_session`] and [`flowproof_driver::input_log`]
//! stay in as a
//! best-effort enhancement for real hardware, where they should work.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use base64::Engine;
use serde_json::json;

use crate::{AgentError, BackendConfig, BackendKind, FlowSpec};

#[derive(Debug, thiserror::Error)]
pub enum VideoAuthorError {
    #[error("ffmpeg not found on PATH: install ffmpeg to extract frames from a video")]
    FfmpegMissing,
    #[error("ffmpeg failed extracting frames: {0}")]
    FfmpegFailed(String),
    #[error("no steps were inferred from the video's frames — nothing to draft")]
    NoStepsInferred,
    #[error("key event capture failed: {0}")]
    KeyCapture(#[from] flowproof_driver::DriverError),
    #[error("model produced a draft that does not parse as a flow spec: {0}")]
    DraftInvalid(#[from] crate::spec::SpecError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("io error at '{path}': {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

fn io_err(path: &Path, source: std::io::Error) -> VideoAuthorError {
    VideoAuthorError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// One frame every `interval_ms` from `video` into `out_dir`, via a
/// system `ffmpeg` (never bundled, never run in CI — local only).
pub fn extract_keyframes(
    video: &Path,
    interval_ms: u64,
    out_dir: &Path,
) -> Result<Vec<PathBuf>, VideoAuthorError> {
    check_ffmpeg_available()?;
    std::fs::create_dir_all(out_dir).map_err(|e| io_err(out_dir, e))?;

    let fps = 1000.0 / interval_ms.max(1) as f64;
    let pattern = out_dir.join("frame_%05d.png");
    let output = Command::new("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(video)
        .args(["-vf", &format!("fps={fps}")])
        .arg(&pattern)
        .output()
        .map_err(|e| VideoAuthorError::FfmpegFailed(e.to_string()))?;
    if !output.status.success() {
        return Err(VideoAuthorError::FfmpegFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    // Empty (too-short video) is not an error here; assemble_draft_spec
    // refuses an empty result downstream, with one message for that.
    let mut frames: Vec<PathBuf> = std::fs::read_dir(out_dir)
        .map_err(|e| io_err(out_dir, e))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("png"))
        .collect();
    frames.sort();
    Ok(frames)
}

/// Record a fresh screen-and-keypress session for `seconds`, writing the
/// video to `out_video`. The two capture concurrently (ffmpeg as a
/// spawned child process, key polling on this thread), so a keypress's
/// logged offset lines up with the same instant in the video. Requires
/// ffmpeg on PATH; only ever runs locally, interactively — never in CI.
pub fn record_session(
    out_video: &Path,
    seconds: u64,
) -> Result<Vec<flowproof_driver::input_log::KeyEvent>, VideoAuthorError> {
    check_ffmpeg_available()?;
    let mut child = Command::new("ffmpeg")
        .arg("-y")
        .args(["-f", "gdigrab"])
        .args(["-framerate", "10"])
        .args(["-i", "desktop"])
        .args(["-t", &seconds.to_string()])
        .arg(out_video)
        .spawn()
        .map_err(|e| VideoAuthorError::FfmpegFailed(e.to_string()))?;
    let events = flowproof_driver::input_log::capture_for(Duration::from_secs(seconds))?;
    let status = child
        .wait()
        .map_err(|e| VideoAuthorError::FfmpegFailed(e.to_string()))?;
    if !status.success() {
        return Err(VideoAuthorError::FfmpegFailed(format!(
            "ffmpeg exited with {status}"
        )));
    }
    Ok(events)
}

fn check_ffmpeg_available() -> Result<(), VideoAuthorError> {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|_| ())
        .map_err(|_| VideoAuthorError::FfmpegMissing)
}

const TRANSITION_SYSTEM_PROMPT: &str = "\
Two frames from a screen recording: FIRST is before an action, SECOND is \
after. You also see the flow's steps already inferred so far, in order.

Describe the action(s) that bridge FIRST to SECOND, each on its own \
line, using ONLY these forms:
- \"Go to /nVA01\" (a transaction code in the command field)
- \"Type <value> into the <label> field\"
- \"Press Enter\"
- \"Press the <label> button\"
- \"Select <option> from the <label> field\"

A keypress like Enter leaves no pixel of its own behind - only its \
effect does. So: if the LAST prior step typed a value into a field, and \
this transition shows an entirely different screen (a new title, a new \
layout) rather than the same screen with that value still sitting \
unconfirmed, put a \"Press Enter\" line FIRST, then a second line ONLY \
if the transition ALSO shows some SEPARATE change Enter does not \
account for (e.g. a different, unrelated field elsewhere became \
populated too). Do not add a second line that just re-describes or \
re-explains the SAME screen change the \"Press Enter\" line already \
covers - one action produced one transition, so a fully-explained \
transition gets exactly one line.

Do not report \"Press the <label> button\" unless you have POSITIVE \
evidence a click landed there - the cursor arrow visibly over that \
button in the BEFORE frame, or the button rendered in a pressed/active \
state. A button merely being present and visually highlighted (e.g. the \
default blue submit button on a form) is NOT evidence it was clicked: \
the exact same screen change happens whether the user clicked it or \
just pressed Enter with focus still in the field they were typing into. \
When you cannot tell which of the two happened, assume Enter, per the \
paragraph above, and do not also add a button-press line for that same \
change.

If nothing meaningful changed, respond with exactly: NO_ACTION

If something DID meaningfully change but you cannot map it to any of \
the forms above (common for a keyboard-only action you cannot see, \
like Enter, Tab, or a function key), do NOT guess and do NOT say \
NO_ACTION. Respond with exactly one line: `UNEXPLAINED: ` followed by a \
short factual description of what changed (e.g. `UNEXPLAINED: the \
screen changed from a data-entry form to an order overview screen`).

Respond with ONLY the step line(s), NO_ACTION, or one UNEXPLAINED line \
— no prose, no quotes, no numbering, no code fences.";

/// Marks a step whose exact action video could not determine — carried
/// through as a real (freeform) step, not a comment, so the existing
/// live authoring agent resolves it against the real screen when
/// `flowproof record` runs, instead of a human retyping it by hand.
const FLAGGED_MARKER: &str = "__FLAGGED__: ";

/// What [`infer_steps`] needs from a model. Separate from
/// [`crate::llm::ModelClient`] (text-only) — nothing else in the
/// authoring loop sends images. `prior_steps` is everything inferred so
/// far, in order — the only way the model can know a value was typed
/// but never confirmed, since that fact isn't visible in either frame.
pub trait VisionModelClient {
    fn describe_transition(
        &mut self,
        prior_steps: &[String],
        before: &[u8],
        after: &[u8],
    ) -> Result<String, AgentError>;
}

/// Anthropic-only for now; another backend would need its own image
/// content-block shape, which `new` refuses rather than sending blind.
pub struct HttpVisionClient {
    config: BackendConfig,
    agent: ureq::Agent,
}

impl HttpVisionClient {
    pub fn new(config: BackendConfig) -> Result<Self, AgentError> {
        if config.kind != BackendKind::Anthropic {
            return Err(AgentError::Config(
                "author-from-video only supports the anthropic backend today".into(),
            ));
        }
        let agent_config = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .proxy(ureq::Proxy::try_from_env())
            .build();
        Ok(Self {
            config,
            agent: agent_config.into(),
        })
    }

    fn image_block(data: &[u8]) -> serde_json::Value {
        json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": base64::engine::general_purpose::STANDARD.encode(data),
            },
        })
    }
}

impl VisionModelClient for HttpVisionClient {
    fn describe_transition(
        &mut self,
        prior_steps: &[String],
        before: &[u8],
        after: &[u8],
    ) -> Result<String, AgentError> {
        let base = self
            .config
            .base_url
            .clone()
            .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.anthropic.com".to_string());
        let key = self
            .config
            .api_key
            .clone()
            .ok_or_else(|| AgentError::Config("no API key for anthropic backend".into()))?;
        let prior = if prior_steps.is_empty() {
            "(none yet)".to_string()
        } else {
            prior_steps.join("\n")
        };
        let content = json!([
            {"type": "text", "text": format!("Steps inferred so far:\n{prior}\n\nFrame BEFORE:")},
            Self::image_block(before),
            {"type": "text", "text": "Frame AFTER:"},
            Self::image_block(after),
        ]);
        let response: serde_json::Value = self
            .agent
            .post(format!("{base}/v1/messages"))
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .send_json(json!({
                "model": self.config.model.clone().unwrap_or_else(|| "claude-sonnet-5".into()),
                // 256 was too tight: extended thinking can consume the
                // whole budget before the model reaches its actual answer,
                // truncating the reply entirely (stop_reason "max_tokens",
                // no text block at all). 1024 matches llm.rs's MAX_TOKENS.
                "max_tokens": 1024,
                // No `temperature` here: current Sonnet rejects it outright
                // (400 "temperature is deprecated for this model"), unlike
                // the text-only path in llm.rs which still sends 0.
                "system": TRANSITION_SYSTEM_PROMPT,
                "messages": [{"role": "user", "content": content}],
            }))
            .map_err(|e| AgentError::Config(format!("model call failed (anthropic): {e}")))?
            .body_mut()
            .read_json()
            .map_err(|e| {
                AgentError::Config(format!("model call failed (anthropic response): {e}"))
            })?;
        // Not always content[0]: with extended thinking on, a "thinking"
        // block comes first and the reply we want is in a later "text"
        // block - a fixed index broke non-deterministically once enough
        // calls were made to occasionally trigger thinking mode.
        response["content"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|block| block["type"] == "text")
            .and_then(|block| block["text"].as_str())
            .map(str::trim)
            .map(str::to_string)
            .ok_or_else(|| {
                AgentError::Config(format!("unexpected anthropic response shape: {response}"))
            })
    }
}

/// One or more draft steps per transition (`NO_ACTION` lines dropped).
/// The first frame is the starting screen and produces no step of its
/// own.
///
/// Any logged keypress whose timestamp falls in a transition's time
/// window becomes a `Press <Key>` step directly — a logged fact, never
/// left to the model to guess at (the cheap approach of just asking the
/// model to infer an invisible keypress from context was tried and did
/// not reliably work). `key_events` is empty when authoring from a
/// video with no matching key log; every transition falls back to pure
/// vision inference exactly as before.
pub fn infer_steps(
    frames: &[PathBuf],
    interval_ms: u64,
    key_events: &[flowproof_driver::input_log::KeyEvent],
    client: &mut dyn VisionModelClient,
) -> Result<Vec<String>, VideoAuthorError> {
    let mut steps = Vec::new();
    for (i, pair) in frames.windows(2).enumerate() {
        let window_start = i as u64 * interval_ms;
        let window_end = window_start + interval_ms;
        for event in key_events
            .iter()
            .filter(|e| e.offset_ms >= window_start && e.offset_ms < window_end)
        {
            steps.push(format!("Press {}", event.key));
        }

        let before = std::fs::read(&pair[0]).map_err(|e| io_err(&pair[0], e))?;
        let after = std::fs::read(&pair[1]).map_err(|e| io_err(&pair[1], e))?;
        let reply = client.describe_transition(&steps, &before, &after)?;
        for line in reply.lines() {
            let line = line.trim();
            if line.is_empty() || line == "NO_ACTION" {
                continue;
            }
            if let Some(observed) = line.strip_prefix("UNEXPLAINED:") {
                steps.push(format!("{FLAGGED_MARKER}{}", observed.trim()));
            } else {
                steps.push(line.to_string());
            }
        }
    }
    Ok(steps)
}

/// Double-quoted YAML scalar, safe for arbitrary model-generated text.
fn yaml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Assembles a draft spec and validates it parses before returning — a
/// bad model reply fails here, not silently. Emits ZERO `assert:` steps
/// by design; see the module docs on why.
pub fn assemble_draft_spec(
    name: &str,
    app: &str,
    source_video: &Path,
    steps: &[String],
) -> Result<String, VideoAuthorError> {
    if steps.is_empty() {
        return Err(VideoAuthorError::NoStepsInferred);
    }
    let mut yaml = format!(
        "# DRAFT generated by `flowproof author-from-video` from {}.\n\
         # Review every step below, then add at least one `assert:` step —\n\
         # none were inferred: a recording shows behaviour, never a belief\n\
         # about what should be true. Only after that is this ready for\n\
         # `flowproof record`.\nname: {}\napp: {}\nsteps:\n",
        source_video.display(),
        yaml_quote(name),
        yaml_quote(app),
    );
    for step in steps {
        if let Some(observed) = step.strip_prefix(FLAGGED_MARKER) {
            yaml.push_str(
                "  # TODO: unexplained screen change here — flagged for the live\n  \
                 # authoring agent to resolve when `flowproof record` runs, not\n  \
                 # dropped silently. Rewrite by hand first if you already know\n  \
                 # what this should be.\n",
            );
            yaml.push_str(&format!(
                "  - {}\n",
                yaml_quote(&format!(
                    "resolve the action needed here against the live screen — a \
                     keyboard-only action like Enter may be the cause. Observed: {observed}"
                ))
            ));
        } else {
            yaml.push_str(&format!("  - {}\n", yaml_quote(step)));
        }
    }
    FlowSpec::parse(&yaml)?;
    Ok(yaml)
}

/// video-in, draft-spec-out options for [`author_from_video`].
pub struct VideoAuthorOptions {
    pub video: PathBuf,
    pub app: String,
    pub name: String,
    pub interval_ms: u64,
    pub out: PathBuf,
    /// When set, `video` is the OUTPUT path for a fresh recording of
    /// this many seconds (screen + keypresses, captured together) —
    /// rather than an already-recorded file to read.
    pub record_for_secs: Option<u64>,
}

pub fn author_from_video(opts: &VideoAuthorOptions) -> Result<PathBuf, VideoAuthorError> {
    let config = BackendConfig::from_env().map_err(VideoAuthorError::from)?;
    if !config.is_usable() {
        return Err(VideoAuthorError::Agent(AgentError::Config(
            "no usable model backend configured (set FLOWPROOF_AI_API_KEY or ANTHROPIC_API_KEY)"
                .into(),
        )));
    }

    let key_events = match opts.record_for_secs {
        Some(secs) => record_session(&opts.video, secs)?,
        None => Vec::new(),
    };

    let frame_dir = opts.out.with_extension("frames");
    let frames = extract_keyframes(&opts.video, opts.interval_ms, &frame_dir)?;
    let mut client = HttpVisionClient::new(config)?;
    let steps = infer_steps(&frames, opts.interval_ms, &key_events, &mut client)?;
    let yaml = assemble_draft_spec(&opts.name, &opts.app, &opts.video, &steps)?;
    std::fs::write(&opts.out, &yaml).map_err(|e| io_err(&opts.out, e))?;
    Ok(opts.out.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeVision {
        replies: std::collections::VecDeque<&'static str>,
    }

    impl VisionModelClient for FakeVision {
        fn describe_transition(
            &mut self,
            _prior_steps: &[String],
            _before: &[u8],
            _after: &[u8],
        ) -> Result<String, AgentError> {
            Ok(self
                .replies
                .pop_front()
                .expect("test provided enough replies")
                .to_string())
        }
    }

    fn frame_paths(dir: &Path, names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|n| {
                let p = dir.join(n);
                std::fs::write(&p, b"fake-png-bytes").expect("write fixture frame");
                p
            })
            .collect()
    }

    #[test]
    fn infer_steps_drops_no_action_replies() {
        let dir = std::env::temp_dir().join("flowproof-video-author-test-drop");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let frames = frame_paths(&dir, &["a.png", "b.png", "c.png"]);
        let mut client = FakeVision {
            replies: ["Go to /nVA01", "NO_ACTION"].into_iter().collect(),
        };
        let steps = infer_steps(&frames, 1000, &[], &mut client).expect("infer succeeds");
        assert_eq!(steps, vec!["Go to /nVA01".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The actual fix for the invisible-keypress problem: a logged
    /// keypress in a transition's time window becomes a real `Press`
    /// step, regardless of what the vision model says about that same
    /// transition (or even if it says NO_ACTION).
    #[test]
    fn infer_steps_inserts_logged_keypresses_deterministically() {
        use flowproof_driver::input_log::KeyEvent;

        let dir = std::env::temp_dir().join("flowproof-video-author-test-keylog");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let frames = frame_paths(&dir, &["a.png", "b.png", "c.png"]);
        // interval_ms = 1000: transition 0 spans [0, 1000), transition 1
        // spans [1000, 2000). The Enter at 1500ms belongs to transition 1
        // even though the model reports NO_ACTION for it.
        let events = vec![KeyEvent {
            offset_ms: 1500,
            key: "Enter".to_string(),
        }];
        let mut client = FakeVision {
            replies: ["Type OR into the Order Type field", "NO_ACTION"]
                .into_iter()
                .collect(),
        };
        let steps = infer_steps(&frames, 1000, &events, &mut client).expect("infer succeeds");
        assert_eq!(
            steps,
            vec![
                "Type OR into the Order Type field".to_string(),
                "Press Enter".to_string(),
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A reply can name more than one action (e.g. the invisible Enter
    /// plus whatever else the transition shows) — each line becomes its
    /// own step, not one run-on step.
    #[test]
    fn infer_steps_splits_multi_line_replies() {
        let dir = std::env::temp_dir().join("flowproof-video-author-test-multiline");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let frames = frame_paths(&dir, &["a.png", "b.png", "c.png"]);
        let mut client = FakeVision {
            replies: [
                "Type OR into the Order Type field",
                "Press Enter\nGo to /nSESSION_MANAGER",
            ]
            .into_iter()
            .collect(),
        };
        let steps = infer_steps(&frames, 1000, &[], &mut client).expect("infer succeeds");
        assert_eq!(
            steps,
            vec![
                "Type OR into the Order Type field".to_string(),
                "Press Enter".to_string(),
                "Go to /nSESSION_MANAGER".to_string(),
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The whole point of threading prior steps into each call: the
    /// model can only infer an invisible keypress if it knows a value
    /// was already typed and not yet confirmed.
    #[test]
    fn infer_steps_gives_each_transition_the_steps_inferred_so_far() {
        struct RecordingVision {
            seen: Vec<Vec<String>>,
        }
        impl VisionModelClient for RecordingVision {
            fn describe_transition(
                &mut self,
                prior_steps: &[String],
                _before: &[u8],
                _after: &[u8],
            ) -> Result<String, AgentError> {
                self.seen.push(prior_steps.to_vec());
                Ok(if self.seen.len() == 1 {
                    "Go to /nVA01".to_string()
                } else {
                    "Press Enter".to_string()
                })
            }
        }
        let dir = std::env::temp_dir().join("flowproof-video-author-test-context");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let frames = frame_paths(&dir, &["a.png", "b.png", "c.png"]);
        let mut client = RecordingVision { seen: Vec::new() };
        let steps = infer_steps(&frames, 1000, &[], &mut client).expect("infer succeeds");
        assert_eq!(
            steps,
            vec!["Go to /nVA01".to_string(), "Press Enter".to_string()]
        );
        assert!(
            client.seen[0].is_empty(),
            "the first transition has no prior context"
        );
        assert_eq!(
            client.seen[1],
            vec!["Go to /nVA01".to_string()],
            "the second transition must see the first inferred step"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The point of the whole flagging design: an unexplained transition
    /// becomes a real, flagged step for the live authoring agent to
    /// resolve later — never silently dropped like NO_ACTION, and never
    /// guessed at as if it were a confident vision read.
    #[test]
    fn infer_steps_flags_unexplained_transitions_instead_of_dropping_them() {
        let dir = std::env::temp_dir().join("flowproof-video-author-test-unexplained");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let frames = frame_paths(&dir, &["a.png", "b.png"]);
        let mut client = FakeVision {
            replies: ["UNEXPLAINED: the screen changed from a form to an overview"]
                .into_iter()
                .collect(),
        };
        let steps = infer_steps(&frames, 1000, &[], &mut client).expect("infer succeeds");
        assert_eq!(steps.len(), 1);
        assert!(
            steps[0].starts_with(FLAGGED_MARKER),
            "unexplained transitions must be marked, not treated as a normal step: {steps:?}"
        );
        assert!(steps[0].contains("the screen changed from a form to an overview"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A flagged step renders as a visible TODO comment plus a real
    /// freeform step (parseable, so `flowproof record`'s existing live
    /// authoring agent can resolve it) — not a dead comment a human has
    /// to notice and rewrite by hand.
    #[test]
    fn assemble_draft_spec_renders_flagged_steps_as_a_todo_plus_a_real_step() {
        let steps = vec![
            "Go to /nVA01".to_string(),
            "Type OR into the Order Type field".to_string(),
            format!("{FLAGGED_MARKER}the screen changed to an order overview"),
        ];
        let yaml = assemble_draft_spec("n", "sap", Path::new("x.mp4"), &steps).expect("assembles");
        assert!(yaml.contains("TODO"), "must be visibly flagged: {yaml}");
        assert!(
            !yaml.contains(FLAGGED_MARKER),
            "internal marker must not leak into the draft"
        );
        let spec = FlowSpec::parse(&yaml).expect("draft with a flagged step still parses");
        assert_eq!(
            spec.steps.len(),
            3,
            "the flagged entry is a real step, not just a comment"
        );
    }

    /// Round-trips through `FlowSpec::parse` AND checks the no-assert
    /// invariant in one pass — both are properties of the same output.
    #[test]
    fn assemble_draft_spec_round_trips_and_never_asserts() {
        let steps = vec![
            "Go to /nVA01".to_string(),
            "Type ZOR: \"quoted\" into the Order Type field".to_string(),
        ];
        let yaml = assemble_draft_spec(
            "Create order draft",
            "sap",
            Path::new("manual-test.mp4"),
            &steps,
        )
        .expect("assembles");
        let spec = FlowSpec::parse(&yaml).expect("draft parses back");
        assert_eq!(spec.name, "Create order draft");
        assert_eq!(spec.steps.len(), 2);
        assert!(
            !spec
                .steps
                .iter()
                .any(|step| matches!(step, crate::spec::SpecStep::Assert { .. })),
            "a draft must never contain an inferred assertion: {yaml}"
        );
    }

    #[test]
    fn assemble_draft_spec_rejects_empty_steps() {
        let err = assemble_draft_spec("n", "sap", Path::new("x.mp4"), &[])
            .expect_err("no steps is an error, not an empty spec");
        assert!(matches!(err, VideoAuthorError::NoStepsInferred));
    }

    #[test]
    fn extract_keyframes_reports_a_clear_error_when_ffmpeg_is_missing() {
        // Only meaningful on a machine without ffmpeg on PATH; skip
        // otherwise rather than asserting an environment property.
        if check_ffmpeg_available().is_ok() {
            eprintln!("skipping: this machine has ffmpeg on PATH");
            return;
        }
        let dir = std::env::temp_dir().join("flowproof-video-author-test-missing-ffmpeg");
        let err = extract_keyframes(Path::new("does-not-matter.mp4"), 1000, &dir)
            .expect_err("ffmpeg is not on PATH in this environment");
        assert!(matches!(err, VideoAuthorError::FfmpegMissing));
    }
}
