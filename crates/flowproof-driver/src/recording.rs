//! The RunRecorder: owns the clock, captures keyframes around every step,
//! applies redaction in-memory, and persists a self-contained recording
//! bundle. Both the recorder (authoring) and the replayer drive it, so
//! every execution gets the same review surface.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::app::AppDriver;
use crate::redact::{self, RedactionRule};
use crate::DriverError;

/// Identifier of the v1 bundle format (step-synchronized keyframes).
pub const FORMAT_FILMSTRIP_V1: &str = "filmstrip/1";

/// One persisted, already-redacted frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameRef {
    pub offset_ms: u64,
    /// File name inside the bundle's `recording/` directory.
    pub file: String,
}

/// Per-step time range, offsets from execution start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepTiming {
    pub id: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Frames dropped instead of persisted (fail-closed redaction).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames_dropped: Option<String>,
}

/// The completed recording: everything a viewer needs, embedded in the
/// execution's structured artifact (trace or run report) — no sidecar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    pub format: String,
    /// Bundle directory, relative to the owning artifact.
    pub dir: String,
    pub frames: Vec<FrameRef>,
    pub steps: Vec<StepTiming>,
    /// Ready-to-play rendering of the whole run (file inside `dir`):
    /// the keyframes as an animated GIF, paced proportionally to the real
    /// execution. Absent when GIF assembly failed — never fails the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gif: Option<String>,
}

/// Captures, redacts, and persists frames for one execution.
pub struct RunRecorder {
    dir: PathBuf,
    rel_dir: String,
    rules: Vec<RedactionRule>,
    started: Instant,
    frames: Vec<FrameRef>,
    steps: Vec<StepTiming>,
    current: Option<(String, u64)>,
    /// A frame for the in-flight step was dropped by fail-closed redaction.
    pending_drop: bool,
    /// Set once the driver reports it cannot capture; recording is skipped
    /// gracefully (never silently faked).
    unsupported: bool,
}

impl RunRecorder {
    /// `base` is the bundle's parent (e.g. the run dir); frames land in
    /// `<base>/recording/`, referenced relatively as `recording`.
    pub fn new(base: &Path, rules: Vec<RedactionRule>) -> std::io::Result<Self> {
        let dir = base.join("recording");
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            rel_dir: "recording".to_string(),
            rules,
            started: Instant::now(),
            frames: Vec::new(),
            steps: Vec::new(),
            current: None,
            pending_drop: false,
            unsupported: false,
        })
    }

    pub fn rules(&self) -> &[RedactionRule] {
        &self.rules
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Capture one redacted keyframe. Redaction is fail-closed: a driver
    /// error while resolving mask targets drops the frame (recorded on the
    /// current step) instead of persisting unmasked pixels.
    fn snap<D: AppDriver>(&mut self, driver: &mut D) {
        if self.unsupported {
            return;
        }
        let offset_ms = self.now_ms();
        let frame = match driver.capture() {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                self.unsupported = true;
                return;
            }
            Err(_) => return, // transient capture failure: skip this frame
        };
        let mut frame = frame;
        match redact::resolve_rects(driver, &self.rules) {
            Ok(rects) => redact::apply(&mut frame, &rects),
            Err(_) => {
                // Fail closed: never persist a frame whose masks could not
                // be resolved. Recorded on the step when it closes.
                self.pending_drop = true;
                return;
            }
        }

        let mut png = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png);
        if image::ImageEncoder::write_image(
            encoder,
            frame.as_raw(),
            frame.width(),
            frame.height(),
            image::ExtendedColorType::Rgba8,
        )
        .is_err()
        {
            return;
        }
        let hash = short_hash(&png);
        let file = format!("frame-{offset_ms:08}-{hash}.png");
        if std::fs::write(self.dir.join(&file), &png).is_ok() {
            self.frames.push(FrameRef { offset_ms, file });
        }
    }

    pub fn step_started<D: AppDriver>(&mut self, driver: &mut D, id: &str) {
        // Stamp the start BEFORE the pre-step snap so that frame falls
        // inside the step's range — it is this step's "before" evidence.
        let start_ms = self.now_ms();
        self.current = Some((id.to_string(), start_ms));
        self.snap(driver);
    }

    pub fn step_finished<D: AppDriver>(&mut self, driver: &mut D) {
        self.snap(driver);
        if let Some((id, start_ms)) = self.current.take() {
            self.steps.push(StepTiming {
                id,
                start_ms,
                end_ms: self.now_ms(),
                frames_dropped: self.pending_drop.then(|| "redaction".to_string()),
            });
            self.pending_drop = false;
        }
    }

    /// Finish the recording. Returns `None` when no frame was ever
    /// persisted (capture unsupported) — the bundle dir is removed so no
    /// empty artifacts are left behind.
    pub fn finish(mut self) -> Option<Recording> {
        if let Some((id, start_ms)) = self.current.take() {
            self.steps.push(StepTiming {
                id,
                start_ms,
                end_ms: self.now_ms(),
                frames_dropped: self.pending_drop.then(|| "redaction".to_string()),
            });
        }
        if self.frames.is_empty() {
            std::fs::remove_dir_all(&self.dir).ok();
            return None;
        }
        let gif = assemble_gif(&self.dir, &self.frames);
        Some(Recording {
            format: FORMAT_FILMSTRIP_V1.to_string(),
            dir: self.rel_dir,
            frames: self.frames,
            steps: self.steps,
            gif,
        })
    }
}

/// Width of the whole-run GIF; frames are scaled down to keep it small.
const GIF_WIDTH: u32 = 880;
/// Per-frame display time is the real gap to the next frame, clamped so
/// the playback stays watchable (waits don't drag, actions don't blink).
const GIF_MIN_MS: u64 = 350;
const GIF_MAX_MS: u64 = 1400;
/// The final frame lingers so the end state can actually be read.
const GIF_LAST_MS: u64 = 2000;

/// Assemble the persisted (already-redacted) keyframes into one animated
/// GIF — the "watch the whole run" review surface. Returns the file name
/// inside the bundle dir, or None on any failure: the GIF is a rendering,
/// never a reason to fail an execution.
fn assemble_gif(dir: &Path, frames: &[FrameRef]) -> Option<String> {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{imageops, Delay, Frame};

    let name = "recording.gif";
    let file = std::fs::File::create(dir.join(name)).ok()?;
    let mut encoder = GifEncoder::new(std::io::BufWriter::new(file));
    encoder.set_repeat(Repeat::Infinite).ok()?;
    for (i, frame_ref) in frames.iter().enumerate() {
        let png = std::fs::read(dir.join(&frame_ref.file)).ok()?;
        let img = image::load_from_memory(&png).ok()?.to_rgba8();
        let img = if img.width() > GIF_WIDTH {
            let height = (img.height() as u64 * GIF_WIDTH as u64 / img.width() as u64) as u32;
            imageops::resize(
                &img,
                GIF_WIDTH,
                height.max(1),
                imageops::FilterType::Triangle,
            )
        } else {
            img
        };
        let shown_ms = match frames.get(i + 1) {
            Some(next) => (next.offset_ms - frame_ref.offset_ms).clamp(GIF_MIN_MS, GIF_MAX_MS),
            None => GIF_LAST_MS,
        };
        let delay = Delay::from_saturating_duration(std::time::Duration::from_millis(shown_ms));
        encoder
            .encode_frame(Frame::from_parts(img, 0, 0, delay))
            .ok()?;
    }
    Some(name.to_string())
}

fn short_hash(bytes: &[u8]) -> String {
    // FNV-1a: stable, dependency-free content fingerprint for file names.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingError {
    #[error("recording io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("driver error: {0}")]
    Driver(#[from] DriverError),
}

#[cfg(test)]
mod tests {
    use crate::mock::MockAppDriver;
    use crate::redact::RedactionRule;

    use super::*;

    fn red_frame() -> image::RgbaImage {
        image::RgbaImage::from_pixel(20, 20, image::Rgba([200, 10, 10, 255]))
    }

    fn mock_with_frame() -> MockAppDriver {
        let mut driver = MockAppDriver::new(&["#secret"]);
        driver.frame = Some(red_frame());
        driver
    }

    #[test]
    fn timeline_brackets_every_step_monotonically() {
        let base = std::env::temp_dir().join("flowproof-recording-sync");
        std::fs::create_dir_all(&base).expect("temp dir");
        let mut driver = mock_with_frame();
        let mut recorder = RunRecorder::new(&base, vec![]).expect("recorder");
        for id in ["s0001", "s0002", "s0003"] {
            recorder.step_started(&mut driver, id);
            recorder.step_finished(&mut driver);
        }
        let recording = recorder.finish().expect("recording produced");

        assert_eq!(recording.format, FORMAT_FILMSTRIP_V1);
        assert_eq!(recording.steps.len(), 3);
        let mut last_end = 0;
        for step in &recording.steps {
            assert!(step.start_ms <= step.end_ms, "range valid: {step:?}");
            assert!(step.start_ms >= last_end, "monotonic: {step:?}");
            last_end = step.end_ms;
            // Every step's range brackets at least one persisted frame.
            assert!(
                recording
                    .frames
                    .iter()
                    .any(|f| f.offset_ms >= step.start_ms && f.offset_ms <= step.end_ms),
                "step {step:?} has a frame in range"
            );
        }
        for frame in &recording.frames {
            assert!(base.join("recording").join(&frame.file).exists());
        }
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn persisted_frames_are_redacted_before_write() {
        let base = std::env::temp_dir().join("flowproof-recording-redact");
        std::fs::create_dir_all(&base).expect("temp dir");
        let mut driver = mock_with_frame();
        driver.rects.insert("#secret".into(), (2, 2, 6, 6));
        driver.password_fields.push((10, 10, 5, 5));

        let mut recorder =
            RunRecorder::new(&base, vec![RedactionRule::css("#secret")]).expect("recorder");
        recorder.step_started(&mut driver, "s0001");
        recorder.step_finished(&mut driver);
        let recording = recorder.finish().expect("recording produced");

        for frame_ref in &recording.frames {
            let png = std::fs::read(base.join("recording").join(&frame_ref.file))
                .expect("frame readable");
            let decoded = image::load_from_memory(&png).expect("valid png").to_rgba8();
            // The css-masked region and the password field are black in the
            // PERSISTED bytes; everything else is untouched.
            assert_eq!(*decoded.get_pixel(3, 3), image::Rgba([0, 0, 0, 255]));
            assert_eq!(*decoded.get_pixel(12, 12), image::Rgba([0, 0, 0, 255]));
            assert_eq!(*decoded.get_pixel(0, 0), image::Rgba([200, 10, 10, 255]));
            assert_eq!(*decoded.get_pixel(19, 19), image::Rgba([200, 10, 10, 255]));
        }
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn unresolvable_redaction_drops_frames_fail_closed() {
        let base = std::env::temp_dir().join("flowproof-recording-dropped");
        std::fs::create_dir_all(&base).expect("temp dir");
        let mut driver = mock_with_frame();
        driver.fail_element_rect = true;

        let mut recorder =
            RunRecorder::new(&base, vec![RedactionRule::css("#secret")]).expect("recorder");
        recorder.step_started(&mut driver, "s0001");
        recorder.step_finished(&mut driver);
        // No frame was persisted, so no recording is produced at all — and
        // crucially, nothing unmasked reached disk.
        assert!(recorder.finish().is_none());
        assert!(!base.join("recording").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn finish_writes_a_whole_run_gif() {
        let base = std::env::temp_dir().join("flowproof-recording-gif");
        std::fs::remove_dir_all(&base).ok();
        std::fs::create_dir_all(&base).expect("temp dir");
        let mut driver = mock_with_frame();
        let mut recorder = RunRecorder::new(&base, vec![]).expect("recorder");
        recorder.step_started(&mut driver, "s0001");
        // Change the screen mid-run so the GIF has distinct frames.
        driver.frame = Some(image::RgbaImage::from_pixel(
            20,
            20,
            image::Rgba([10, 10, 200, 255]),
        ));
        recorder.step_finished(&mut driver);
        let recording = recorder.finish().expect("recording produced");

        let gif = recording.gif.as_deref().expect("gif rendered");
        let path = base.join("recording").join(gif);
        let bytes = std::fs::read(&path).expect("gif readable");
        assert!(bytes.starts_with(b"GIF89a"), "valid GIF header");
        // Decodes as an animation with one frame per persisted keyframe.
        let decoder =
            image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes)).expect("gif decodes");
        let frames = image::AnimationDecoder::into_frames(decoder)
            .collect_frames()
            .expect("frames decode");
        assert_eq!(frames.len(), recording.frames.len());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn capture_unsupported_means_no_recording() {
        let base = std::env::temp_dir().join("flowproof-recording-unsupported");
        std::fs::create_dir_all(&base).expect("temp dir");
        let mut driver = MockAppDriver::new(&[]); // frame: None
        let mut recorder = RunRecorder::new(&base, vec![]).expect("recorder");
        recorder.step_started(&mut driver, "s0001");
        recorder.step_finished(&mut driver);
        assert!(recorder.finish().is_none());
        std::fs::remove_dir_all(&base).ok();
    }
}
