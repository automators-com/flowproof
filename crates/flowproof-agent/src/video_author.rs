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

use std::path::{Path, PathBuf};
use std::process::Command;

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

fn check_ffmpeg_available() -> Result<(), VideoAuthorError> {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|_| ())
        .map_err(|_| VideoAuthorError::FfmpegMissing)
}

const TRANSITION_SYSTEM_PROMPT: &str = "\
Two frames from a screen recording: FIRST is before an action, SECOND is \
after. Describe the SINGLE action a flowproof step would perform to go \
from first to second, using ONLY these forms:
- \"Go to /nVA01\" (a transaction code in the command field)
- \"Type <value> into the <label> field\"
- \"Press the <label> button\"
- \"Select <option> from the <label> field\"
If nothing meaningful changed, or you cannot tell what did, respond with \
exactly: NO_ACTION
Respond with ONLY the step text or NO_ACTION — no prose, no quotes, no \
code fences.";

/// What [`infer_steps`] needs from a model. Separate from
/// [`crate::llm::ModelClient`] (text-only) — nothing else in the
/// authoring loop sends images.
pub trait VisionModelClient {
    fn describe_transition(&mut self, before: &[u8], after: &[u8]) -> Result<String, AgentError>;
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
    fn describe_transition(&mut self, before: &[u8], after: &[u8]) -> Result<String, AgentError> {
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
        let content = json!([
            {"type": "text", "text": "Frame BEFORE:"},
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
                "max_tokens": 256,
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
        response["content"][0]["text"]
            .as_str()
            .map(str::trim)
            .map(str::to_string)
            .ok_or_else(|| {
                AgentError::Config(format!("unexpected anthropic response shape: {response}"))
            })
    }
}

/// One draft step per transition (`NO_ACTION` replies dropped). The
/// first frame is the starting screen and produces no step of its own.
pub fn infer_steps(
    frames: &[PathBuf],
    client: &mut dyn VisionModelClient,
) -> Result<Vec<String>, VideoAuthorError> {
    let mut steps = Vec::new();
    for pair in frames.windows(2) {
        let before = std::fs::read(&pair[0]).map_err(|e| io_err(&pair[0], e))?;
        let after = std::fs::read(&pair[1]).map_err(|e| io_err(&pair[1], e))?;
        let step = client.describe_transition(&before, &after)?;
        if step != "NO_ACTION" && !step.is_empty() {
            steps.push(step);
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
        yaml.push_str(&format!("  - {}\n", yaml_quote(step)));
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
}

pub fn author_from_video(opts: &VideoAuthorOptions) -> Result<PathBuf, VideoAuthorError> {
    let config = BackendConfig::from_env().map_err(VideoAuthorError::from)?;
    if !config.is_usable() {
        return Err(VideoAuthorError::Agent(AgentError::Config(
            "no usable model backend configured (set FLOWPROOF_AI_API_KEY or ANTHROPIC_API_KEY)"
                .into(),
        )));
    }

    let frame_dir = opts.out.with_extension("frames");
    let frames = extract_keyframes(&opts.video, opts.interval_ms, &frame_dir)?;
    let mut client = HttpVisionClient::new(config)?;
    let steps = infer_steps(&frames, &mut client)?;
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
        let steps = infer_steps(&frames, &mut client).expect("infer succeeds");
        assert_eq!(steps, vec!["Go to /nVA01".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
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
