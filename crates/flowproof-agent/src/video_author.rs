//! Draft a `.flow.yaml` spec from the frames of a screen recording. The
//! frames themselves come from the caller (the CLI decodes the video in the
//! browser web flows already use); this module turns them into a draft with
//! one model call that sees every frame at once, so nothing that happens
//! across two frames is lost the way it is when a video is described in
//! separate clips.
//!
//! A recording shows behaviour and states no belief about what is correct,
//! so a video draft NEVER contains an assert (the reverse of
//! [`crate::doc_author`], whose document states an Expected for every step).
//! Anything the model cannot explain is flagged through the same
//! [`draft_assembly`] mechanism, for `flowproof record` to resolve against
//! the live app.

use std::path::{Path, PathBuf};

use crate::draft_assembly::{self, DraftLine, STEP_GRAMMAR_RULES};
use crate::llm::{ImagePart, ModelClient};
use crate::{AgentError, BackendConfig};

#[derive(Debug, thiserror::Error)]
pub enum VideoAuthorError {
    #[error("the video gave no frames to draft from")]
    NoFrames,
    #[error("no steps were read from the video's frames — nothing to draft")]
    NoStepsInferred,
    /// The frames show no application in use (driving footage, a talk, a
    /// black screen): there is nothing a flow could repeat. Carries what
    /// the model says the video shows instead.
    #[error("the video shows no app being used: {0}")]
    NotAnAppRecording(String),
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

/// One frame of the recording, as the caller decoded it.
pub struct Frame {
    /// Where in the video it was taken.
    pub at_ms: u64,
    pub jpeg: Vec<u8>,
}

/// frames-in, draft-spec-out options for [`author_from_frames`].
pub struct VideoAuthorOptions {
    /// The recording, named in the draft's header.
    pub video: PathBuf,
    pub app: String,
    pub name: String,
    pub out: PathBuf,
    /// Where the flow starts, when the caller knows better than the frames
    /// (an address bar is often cropped out or unreadable).
    pub url: Option<String>,
    pub duration_ms: u64,
}

pub struct VideoAuthorResult {
    pub flow: PathBuf,
    /// The drafted lines in file order, so a caller can show which steps
    /// were flagged without reading the `# TODO` comments back.
    pub lines: Vec<DraftLine>,
}

fn system_prompt() -> String {
    format!(
        "You watch a screen recording of someone using an application and write \
         down, as test steps, what they did. You are given the frames where the \
         screen changed, in order, each labelled with its time in the video.

Write the user's actions in the order they happen, each on its own line \
prefixed `STEP: `. Use these forms where they fit. {STEP_GRAMMAR_RULES}
- Go to <address> (only when the address bar shows where the session starts)
- Click \"<label>\"

When no form fits, write a short imperative sentence that names the visible \
text of what was used, e.g. `STEP: Open Produkte and select Haushaltsstrom`; \
flowproof resolves such a sentence against the live app when it records.

Rules:
- One action per line. Never fold two fields into one line: a street and a \
house number are two Type lines.
- When the user typed into a field and then picked an entry from a list that \
appeared under it, write ONE Type line whose value is the entry they picked, \
in full (typed `stadtwer`, picked `Nord Stadtwerke GmbH` → Type Nord \
Stadtwerke GmbH into the \"Mein aktueller Versorger\" field). flowproof picks \
it from the list when it records.
- A value is what the frames show, exactly. A masked field (dots, a \
password) is ${{PASSWORD}}; never guess a secret.
- Do not write scrolling: flowproof scrolls to whatever a step names.
- Read changes between frames carefully: a count going from 2 to 3 and then \
to 4 is two clicks, even when the frames between are few.
- Never write an assertion or an expected result. A recording shows what \
happened, not what should happen; a person adds those.

If the screen changes between two frames in a way no visible action \
explains, do NOT guess. Write `UNEXPLAINED: ` followed by what changed, the \
two frame times, then ` — ` and what is missing to write the step (e.g. \
`UNEXPLAINED: the form moved to Persönliche Daten between 0:41.0 and 0:44.5 — \
no click is visible`).

If an action clearly happens in a different application than the one under \
test (an email client, a spreadsheet, a file browser), write `OUT_OF_SCOPE: ` \
followed by a short factual description of it.

If no frame shows someone using an application at all (camera footage, a \
film, slides advancing on their own, an empty screen), reply with ONE line, \
`NOT_APP: ` followed by a short phrase saying what the video shows instead \
(e.g. `NOT_APP: in-car footage of someone driving through a town`), and \
nothing else.

Respond with ONLY STEP:/UNEXPLAINED:/OUT_OF_SCOPE: lines, or the single \
NOT_APP: line — no prose, no numbering, no code fences."
    )
}

/// `m:ss.s`, the label a frame is shown to the model with.
fn clock(ms: u64) -> String {
    format!("{}:{:04.1}", ms / 60_000, (ms % 60_000) as f64 / 1000.0)
}

fn user_prompt(opts: &VideoAuthorOptions, frames: usize) -> String {
    let file = opts
        .video
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        "Recording: {file}, {} long, {frames} frames where the screen changed (above).\n\
         App under test: {}.\n\
         Write the steps.",
        clock(opts.duration_ms),
        opts.app,
    )
}

/// The reply as draft lines. An assert is never kept, whatever the model
/// wrote: see the module docs.
///
/// A draft needs at least one action: flagged and out-of-scope lines alone
/// are observations, not a flow (the model files an unrelated video under
/// either when it does not say `NOT_APP:`). A `NOT_APP:` line is honoured
/// only when no action came with it.
fn parse_reply(reply: &str) -> Result<Vec<DraftLine>, VideoAuthorError> {
    let mut lines = Vec::new();
    let mut not_app = None;
    for line in reply.lines() {
        let line = line.trim();
        if let Some(shows) = line.strip_prefix("NOT_APP:") {
            not_app.get_or_insert_with(|| shows.trim().to_string());
        } else if let Some(step) = line.strip_prefix("STEP:") {
            let step = step.trim();
            if !step.is_empty() && step != "NO_ACTION" {
                lines.push(DraftLine::Action(step.to_string()));
            }
        } else if let Some(observed) = line.strip_prefix("UNEXPLAINED:") {
            lines.push(DraftLine::Flagged(observed.trim().to_string()));
        } else if let Some(observed) = line.strip_prefix("OUT_OF_SCOPE:") {
            lines.push(DraftLine::OutOfScope(observed.trim().to_string()));
        }
        // Any other shape (a stray ASSERT:, prose) is dropped: one ignorable
        // line is not worth failing the draft over.
    }
    if lines.iter().any(|l| matches!(l, DraftLine::Action(_))) {
        return Ok(lines);
    }
    Err(match not_app {
        Some(shows) if !shows.is_empty() => VideoAuthorError::NotAnAppRecording(shows),
        _ => VideoAuthorError::NoStepsInferred,
    })
}

fn model_client() -> Result<crate::llm::HttpModelClient, VideoAuthorError> {
    let config = BackendConfig::from_env()?;
    if !config.is_usable() {
        return Err(VideoAuthorError::Agent(AgentError::Config(
            "no usable model backend configured (run `flowproof config ai` or set FLOWPROOF_AI_API_KEY / ANTHROPIC_API_KEY / OPENAI_API_KEY)"
                .into(),
        )));
    }
    Ok(crate::llm::HttpModelClient::new(config))
}

pub fn author_from_frames(
    opts: &VideoAuthorOptions,
    frames: &[Frame],
) -> Result<VideoAuthorResult, VideoAuthorError> {
    let mut client = model_client()?;
    draft_from_frames(opts, frames, &mut client)
}

/// [`author_from_frames`] against a given model, for tests and callers that
/// hold their own client.
pub fn draft_from_frames(
    opts: &VideoAuthorOptions,
    frames: &[Frame],
    client: &mut dyn ModelClient,
) -> Result<VideoAuthorResult, VideoAuthorError> {
    if frames.is_empty() {
        return Err(VideoAuthorError::NoFrames);
    }
    let images: Vec<ImagePart> = frames
        .iter()
        .enumerate()
        .map(|(i, frame)| ImagePart {
            caption: format!("Frame {} at {}", i + 1, clock(frame.at_ms)),
            media_type: "image/jpeg",
            data: frame.jpeg.clone(),
        })
        .collect();
    let reply =
        client.complete_with_images(&system_prompt(), &user_prompt(opts, frames.len()), &images)?;
    let mut lines = parse_reply(&reply)?;
    if let Some(url) = &opts.url {
        let starts_there = matches!(lines.first(), Some(DraftLine::Action(step))
            if step.to_ascii_lowercase().starts_with("go to "));
        if !starts_there {
            lines.insert(0, DraftLine::Action(format!("Go to {url}")));
        }
    }
    let header = format!(
        "# DRAFT generated by `flowproof author-from-video` from {}.\n\
         # Review every step below before `flowproof record` - a flagged\n\
         # step (marked TODO) needs the live app to resolve it. A recording\n\
         # shows what happened, not what should: add at least one\n\
         # `assert:` before you rely on this flow.",
        opts.video.display()
    );
    let yaml = draft_assembly::assemble(&header, &opts.name, &opts.app, &lines)?;
    write(&opts.out, &yaml)?;
    Ok(VideoAuthorResult {
        flow: opts.out.clone(),
        lines,
    })
}

fn write(path: &Path, yaml: &str) -> Result<(), VideoAuthorError> {
    std::fs::write(path, yaml).map_err(|source| VideoAuthorError::Io {
        path: path.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers once, and remembers how many pictures it was shown.
    struct ScriptedModel {
        reply: &'static str,
        seen: Vec<String>,
    }

    impl ModelClient for ScriptedModel {
        fn complete(&mut self, _system: &str, _user: &str) -> Result<String, AgentError> {
            panic!("a video draft must be authored from the pictures")
        }

        fn complete_with_images(
            &mut self,
            _system: &str,
            _user: &str,
            images: &[ImagePart],
        ) -> Result<String, AgentError> {
            self.seen = images.iter().map(|i| i.caption.clone()).collect();
            Ok(self.reply.to_string())
        }

        fn identity(&self) -> (String, String) {
            ("scripted".into(), "scripted".into())
        }
    }

    fn opts(out: &Path, url: Option<&str>) -> VideoAuthorOptions {
        VideoAuthorOptions {
            video: PathBuf::from("order.mp4"),
            app: "web".into(),
            name: "Electricity order".into(),
            out: out.to_path_buf(),
            url: url.map(str::to_string),
            duration_ms: 84_366,
        }
    }

    fn frames(n: u64) -> Vec<Frame> {
        (0..n)
            .map(|i| Frame {
                at_ms: i * 3_500,
                jpeg: vec![0xff, 0xd8],
            })
            .collect()
    }

    #[test]
    fn every_frame_is_shown_once_in_order_with_its_time() {
        let out = std::env::temp_dir().join("flowproof-video-order.flow.yaml");
        let mut model = ScriptedModel {
            reply: "STEP: Click \"Preise berechnen\"",
            seen: Vec::new(),
        };
        draft_from_frames(&opts(&out, None), &frames(3), &mut model).expect("drafts");
        assert_eq!(
            model.seen,
            [
                "Frame 1 at 0:00.0",
                "Frame 2 at 0:03.5",
                "Frame 3 at 0:07.0"
            ]
        );
        std::fs::remove_file(out).ok();
    }

    /// The invariant that sets video apart from documents: a recording
    /// states no belief, so an assert the model writes anyway is dropped.
    #[test]
    fn a_video_draft_never_asserts_and_flags_what_it_cannot_explain() {
        let out = std::env::temp_dir().join("flowproof-video-flags.flow.yaml");
        let mut model = ScriptedModel {
            reply: "STEP: Type 24944 Flensburg into the \"PLZ und Ort\" field\n\
                    ASSERT: page shows 4200 kWh/Jahr\n\
                    UNEXPLAINED: the household went from 2 to 3 between 0:09.5 and 0:12.5 — no click is visible\n\
                    OUT_OF_SCOPE: opens the confirmation e-mail in Outlook\n\
                    Here are the steps you asked for.",
            seen: Vec::new(),
        };
        let result = draft_from_frames(&opts(&out, None), &frames(2), &mut model).expect("drafts");
        assert!(matches!(result.lines[0], DraftLine::Action(_)));
        assert!(matches!(result.lines[1], DraftLine::Flagged(_)));
        assert!(matches!(result.lines[2], DraftLine::OutOfScope(_)));
        assert_eq!(result.lines.len(), 3);
        let yaml = std::fs::read_to_string(&out).expect("written");
        // No assert STEP; the header's own advice to add one is fine.
        assert!(!yaml.contains("- assert:"), "{yaml}");
        assert!(yaml.contains("author-from-video"), "{yaml}");
        std::fs::remove_file(out).ok();
    }

    #[test]
    fn a_given_start_address_leads_unless_the_video_already_goes_there() {
        let out = std::env::temp_dir().join("flowproof-video-url.flow.yaml");
        let mut model = ScriptedModel {
            reply: "STEP: Click \"Produkte\"",
            seen: Vec::new(),
        };
        let result = draft_from_frames(
            &opts(&out, Some("https://www.stadtwerke-flensburg.de/")),
            &frames(1),
            &mut model,
        )
        .expect("drafts");
        assert_eq!(
            result.lines[0],
            DraftLine::Action("Go to https://www.stadtwerke-flensburg.de/".into())
        );

        let mut model = ScriptedModel {
            reply: "STEP: Go to https://www.stadtwerke-flensburg.de/produkte/strom\n\
                    STEP: Click \"Preise berechnen\"",
            seen: Vec::new(),
        };
        let result = draft_from_frames(
            &opts(&out, Some("https://www.stadtwerke-flensburg.de/")),
            &frames(1),
            &mut model,
        )
        .expect("drafts");
        assert_eq!(result.lines.len(), 2);
        std::fs::remove_file(out).ok();
    }

    #[test]
    fn no_frames_and_no_steps_are_errors_not_empty_drafts() {
        let out = std::env::temp_dir().join("flowproof-video-empty.flow.yaml");
        let mut model = ScriptedModel {
            reply: "Nothing happens in this video.",
            seen: Vec::new(),
        };
        assert!(matches!(
            draft_from_frames(&opts(&out, None), &[], &mut model),
            Err(VideoAuthorError::NoFrames)
        ));
        assert!(matches!(
            draft_from_frames(&opts(&out, None), &frames(2), &mut model),
            Err(VideoAuthorError::NoStepsInferred)
        ));
        assert!(!out.exists());
    }

    /// Driving footage, a talk, a black screen: refused with what the video
    /// shows, never written as a draft of observations.
    #[test]
    fn a_video_that_shows_no_app_is_refused_not_drafted() {
        let out = std::env::temp_dir().join("flowproof-video-not-app.flow.yaml");
        let mut model = ScriptedModel {
            reply: "NOT_APP: in-car footage of someone driving through a town",
            seen: Vec::new(),
        };
        match draft_from_frames(&opts(&out, None), &frames(2), &mut model) {
            Err(e @ VideoAuthorError::NotAnAppRecording(_)) => assert_eq!(
                e.to_string(),
                "the video shows no app being used: in-car footage of someone driving through a town"
            ),
            other => panic!("expected NotAnAppRecording, got {:?}", other.err()),
        }

        // Without NOT_APP the model files the same video under a flag or
        // out-of-scope line; with no action beside it, that is no draft.
        let mut model = ScriptedModel {
            reply: "UNEXPLAINED: the slide changed from \"Intro\" to \"Agenda\" between 0:00.0 and 0:08.0 — no click is visible\n\
                    OUT_OF_SCOPE: night-time highway traffic, no application on screen",
            seen: Vec::new(),
        };
        assert!(matches!(
            draft_from_frames(
                &opts(&out, Some("https://example.com/")),
                &frames(2),
                &mut model
            ),
            Err(VideoAuthorError::NoStepsInferred)
        ));
        assert!(!out.exists());
    }

    #[test]
    fn a_stray_not_app_beside_real_steps_does_not_refuse_the_draft() {
        let out = std::env::temp_dir().join("flowproof-video-stray-not-app.flow.yaml");
        let mut model = ScriptedModel {
            reply: "STEP: Click \"Preise berechnen\"\nNOT_APP: a website",
            seen: Vec::new(),
        };
        let result = draft_from_frames(&opts(&out, None), &frames(1), &mut model).expect("drafts");
        assert_eq!(
            result.lines,
            [DraftLine::Action("Click \"Preise berechnen\"".into())]
        );
        std::fs::remove_file(out).ok();
    }

    #[test]
    fn frame_times_read_as_minutes_and_seconds() {
        assert_eq!(clock(0), "0:00.0");
        assert_eq!(clock(84_366), "1:24.4");
        assert_eq!(clock(9_500), "0:09.5");
    }
}
