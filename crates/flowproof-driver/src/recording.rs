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

/// How densely an execution captures visual evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordingDetail {
    /// Capture before and after every step (the historical behavior).
    #[default]
    Full,
    /// Capture the initial state, every fifth completed step, and the final
    /// state. Several steps intentionally share one visual checkpoint.
    Low,
    /// Capture no visual evidence at all.
    Off,
}

/// Per-execution recording controls. These affect artifacts only; they never
/// change which actions execute or how a verdict is computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordingOptions {
    pub detail: RecordingDetail,
    /// Assemble the captured PNG keyframes into `recording.gif`.
    pub video: bool,
    /// Draw a synthetic cursor and a prominent click halo into pointer-event
    /// checkpoints. Screen-capture APIs commonly omit the real OS cursor.
    pub highlight_cursor: bool,
}

impl Default for RecordingOptions {
    fn default() -> Self {
        Self {
            detail: RecordingDetail::Full,
            video: false,
            highlight_cursor: false,
        }
    }
}

impl RecordingOptions {
    pub fn enabled(self) -> bool {
        self.detail != RecordingDetail::Off
    }
}

const LOW_DETAIL_STEP_INTERVAL: usize = 5;

#[derive(Debug, Clone, Copy)]
struct PointerMarker {
    x: i32,
    y: i32,
    highlighted: bool,
}

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
    options: RecordingOptions,
    completed_steps: usize,
    last_snapshot_after_step: usize,
    pointer: Option<PointerMarker>,
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
        Self::with_options(base, rules, RecordingOptions::default())
    }

    pub fn with_options(
        base: &Path,
        rules: Vec<RedactionRule>,
        options: RecordingOptions,
    ) -> std::io::Result<Self> {
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
            options,
            completed_steps: 0,
            last_snapshot_after_step: 0,
            pointer: None,
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
    fn snap<D: AppDriver>(&mut self, driver: &mut D) -> bool {
        if self.unsupported {
            return false;
        }
        let offset_ms = self.now_ms();
        let frame = match driver.capture() {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                self.unsupported = true;
                return false;
            }
            Err(_) => return false, // transient capture failure: skip this frame
        };
        let mut frame = frame;
        match redact::resolve_rects(driver, &self.rules) {
            Ok(rects) => redact::apply(&mut frame, &rects),
            Err(_) => {
                // Fail closed: never persist a frame whose masks could not
                // be resolved. Recorded on the step when it closes.
                self.pending_drop = true;
                return false;
            }
        }
        if let Some(pointer) = self.pointer {
            draw_pointer(&mut frame, pointer);
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
            return false;
        }
        let hash = short_hash(&png);
        let file = format!("frame-{offset_ms:08}-{hash}.png");
        if std::fs::write(self.dir.join(&file), &png).is_ok() {
            self.frames.push(FrameRef { offset_ms, file });
            true
        } else {
            false
        }
    }

    pub fn step_started<D: AppDriver>(&mut self, driver: &mut D, id: &str) {
        // Stamp the start BEFORE the pre-step snap so that frame falls
        // inside the step's range — it is this step's "before" evidence.
        let start_ms = self.now_ms();
        self.current = Some((id.to_string(), start_ms));
        match self.options.detail {
            RecordingDetail::Full => {
                self.snap(driver);
            }
            RecordingDetail::Low if self.completed_steps == 0 && self.frames.is_empty() => {
                self.snap(driver);
            }
            RecordingDetail::Low | RecordingDetail::Off => {}
        }
    }

    /// Add a pointer-event checkpoint at a point inside `selector`.
    ///
    /// This is best-effort evidence: an unavailable element rectangle or a
    /// capture failure never changes execution. The event frame gets the
    /// bright halo; later frames retain the visible cursor at the last known
    /// position without implying another click.
    pub fn pointer_event<D: AppDriver>(
        &mut self,
        driver: &mut D,
        selector: &crate::app::UiaSelector,
        x_pct: f64,
        y_pct: f64,
    ) -> bool {
        if !self.options.highlight_cursor || !self.options.enabled() {
            return false;
        }
        let Ok(Some((left, top, width, height))) = driver.element_rect(selector) else {
            return false;
        };
        let x = left
            + ((width.saturating_sub(1) as f64) * (x_pct.clamp(0.0, 100.0) / 100.0)).round() as i32;
        let y = top
            + ((height.saturating_sub(1) as f64) * (y_pct.clamp(0.0, 100.0) / 100.0)).round()
                as i32;
        self.pointer = Some(PointerMarker {
            x,
            y,
            highlighted: true,
        });
        let captured = self.snap(driver);
        if let Some(pointer) = self.pointer.as_mut() {
            pointer.highlighted = false;
        }
        captured
    }

    pub fn step_finished<D: AppDriver>(&mut self, driver: &mut D) {
        self.completed_steps += 1;
        match self.options.detail {
            RecordingDetail::Full => {
                if self.snap(driver) {
                    self.last_snapshot_after_step = self.completed_steps;
                }
            }
            RecordingDetail::Low
                if self
                    .completed_steps
                    .is_multiple_of(LOW_DETAIL_STEP_INTERVAL) =>
            {
                if self.snap(driver) {
                    self.last_snapshot_after_step = self.completed_steps;
                }
            }
            RecordingDetail::Low | RecordingDetail::Off => {}
        }
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

    /// Finish while the driver is still available. Low-detail recordings use
    /// this to guarantee a final-state checkpoint when the step count is not
    /// an exact multiple of the sparse cadence.
    pub fn finish_with_driver<D: AppDriver>(mut self, driver: &mut D) -> Option<Recording> {
        if self.options.detail == RecordingDetail::Low
            && self.completed_steps > 0
            && self.last_snapshot_after_step != self.completed_steps
            && self.snap(driver)
        {
            self.last_snapshot_after_step = self.completed_steps;
            // Keep the final-state frame inside the last step's range so
            // step-scoped viewers include it as evidence for that step.
            let end_ms = self.now_ms();
            if let Some(last) = self.steps.last_mut() {
                last.end_ms = end_ms;
            }
        }
        self.finish()
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
        let gif = self
            .options
            .video
            .then(|| assemble_gif(&self.dir, &self.frames))
            .flatten();
        Some(Recording {
            format: FORMAT_FILMSTRIP_V1.to_string(),
            dir: self.rel_dir,
            frames: self.frames,
            steps: self.steps,
            gif,
        })
    }
}

fn draw_pointer(frame: &mut image::RgbaImage, marker: PointerMarker) {
    if marker.highlighted {
        // Two high-contrast rings survive GIF downscaling and remain visible
        // against both light and dark application chrome.
        draw_ring(frame, marker.x, marker.y, 22, 16, [255, 210, 0, 220]);
        draw_ring(frame, marker.x, marker.y, 11, 7, [255, 45, 45, 245]);
    }

    // A compact, two-colour cursor bitmap. The arrow tip is exactly the
    // action coordinate; drawing at 2x keeps it legible in the 880px GIF.
    const CURSOR: [&[u8]; 16] = [
        b"##...........",
        b"#W#..........",
        b"#WW#.........",
        b"#WWW#........",
        b"#WWWW#.......",
        b"#WWWWW#......",
        b"#WWWWWW#.....",
        b"#WWWWWWW#....",
        b"#WWWW#####...",
        b"#WW#W#.......",
        b"#W#.#W#......",
        b"##..#W#......",
        b"#....#W#.....",
        b".....#W#.....",
        b".....#W#.....",
        b"......##.....",
    ];
    for (row, pixels) in CURSOR.iter().enumerate() {
        for (col, pixel) in pixels.iter().enumerate() {
            let color = match pixel {
                b'#' => [15, 15, 15, 255],
                b'W' => [255, 255, 255, 255],
                _ => continue,
            };
            for sy in 0..2 {
                for sx in 0..2 {
                    put_pixel(
                        frame,
                        marker.x + (col as i32 * 2) + sx,
                        marker.y + (row as i32 * 2) + sy,
                        color,
                    );
                }
            }
        }
    }
}

fn draw_ring(
    frame: &mut image::RgbaImage,
    center_x: i32,
    center_y: i32,
    outer: i32,
    inner: i32,
    color: [u8; 4],
) {
    let outer_sq = outer * outer;
    let inner_sq = inner * inner;
    for dy in -outer..=outer {
        for dx in -outer..=outer {
            let distance_sq = dx * dx + dy * dy;
            if (inner_sq..=outer_sq).contains(&distance_sq) {
                blend_pixel(frame, center_x + dx, center_y + dy, color);
            }
        }
    }
}

fn put_pixel(frame: &mut image::RgbaImage, x: i32, y: i32, color: [u8; 4]) {
    if x >= 0 && y >= 0 && (x as u32) < frame.width() && (y as u32) < frame.height() {
        frame.put_pixel(x as u32, y as u32, image::Rgba(color));
    }
}

fn blend_pixel(frame: &mut image::RgbaImage, x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 || (x as u32) >= frame.width() || (y as u32) >= frame.height() {
        return;
    }
    let pixel = frame.get_pixel_mut(x as u32, y as u32);
    let alpha = u16::from(color[3]);
    let inverse = 255 - alpha;
    for channel in 0..3 {
        pixel[channel] =
            ((u16::from(color[channel]) * alpha + u16::from(pixel[channel]) * inverse) / 255) as u8;
    }
    pixel[3] = 255;
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
        let options = RecordingOptions {
            video: true,
            ..RecordingOptions::default()
        };
        let mut recorder = RunRecorder::with_options(&base, vec![], options).expect("recorder");
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
    fn video_is_off_by_default_without_disabling_keyframes() {
        let base = std::env::temp_dir().join("flowproof-recording-default-no-video");
        std::fs::remove_dir_all(&base).ok();
        std::fs::create_dir_all(&base).expect("temp dir");
        let mut driver = mock_with_frame();
        let mut recorder = RunRecorder::new(&base, vec![]).expect("recorder");
        recorder.step_started(&mut driver, "s0001");
        recorder.step_finished(&mut driver);
        let recording = recorder.finish().expect("recording produced");

        assert_eq!(recording.frames.len(), 2);
        assert!(recording.gif.is_none());
        assert!(!base.join("recording/recording.gif").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn pointer_events_add_a_highlighted_cursor_checkpoint() {
        let base = std::env::temp_dir().join("flowproof-recording-highlight-cursor");
        std::fs::remove_dir_all(&base).ok();
        std::fs::create_dir_all(&base).expect("temp dir");
        let mut driver = MockAppDriver::new(&["#target"]);
        driver.frame = Some(image::RgbaImage::from_pixel(
            100,
            100,
            image::Rgba([0, 0, 0, 255]),
        ));
        driver.rects.insert("#target".into(), (30, 30, 20, 20));
        let options = RecordingOptions {
            detail: RecordingDetail::Full,
            video: false,
            highlight_cursor: true,
        };
        let mut recorder = RunRecorder::with_options(&base, vec![], options).expect("recorder");
        recorder.step_started(&mut driver, "s0001");
        assert!(recorder.pointer_event(
            &mut driver,
            &crate::app::UiaSelector::css("#target"),
            50.0,
            50.0,
        ));
        recorder.step_finished(&mut driver);
        let recording = recorder.finish().expect("recording produced");

        assert_eq!(recording.frames.len(), 3);
        let event_png = std::fs::read(base.join("recording").join(&recording.frames[1].file))
            .expect("event frame readable");
        let event = image::load_from_memory(&event_png)
            .expect("valid event png")
            .to_rgba8();
        assert!(
            event
                .pixels()
                .any(|pixel| pixel[0] > 180 && pixel[1] > 130 && pixel[2] < 40),
            "yellow halo is visible"
        );
        assert!(
            event
                .pixels()
                .any(|pixel| pixel[0] > 240 && pixel[1] > 240 && pixel[2] > 240),
            "white cursor is visible"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn low_detail_captures_checkpoints_instead_of_every_step() {
        let base = std::env::temp_dir().join("flowproof-recording-low-detail");
        std::fs::remove_dir_all(&base).ok();
        std::fs::create_dir_all(&base).expect("temp dir");
        let mut driver = mock_with_frame();
        let options = RecordingOptions {
            detail: RecordingDetail::Low,
            video: false,
            highlight_cursor: false,
        };
        let mut recorder = RunRecorder::with_options(&base, vec![], options).expect("recorder");
        for n in 1..=7 {
            recorder.step_started(&mut driver, &format!("s{n:04}"));
            recorder.step_finished(&mut driver);
        }
        let recording = recorder
            .finish_with_driver(&mut driver)
            .expect("recording produced");

        assert_eq!(recording.steps.len(), 7);
        assert_eq!(
            recording.frames.len(),
            3,
            "initial, fifth-step, and final checkpoints"
        );
        assert!(
            recording.frames.last().expect("final frame").offset_ms
                <= recording.steps.last().expect("final step").end_ms
        );
        assert!(recording.gif.is_none());
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
