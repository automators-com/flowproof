//! The pixels-only adapter (`vision`): Citrix, RDP, or any window driven
//! without an accessibility API. Perception is OCR over captured frames;
//! action is OS-level input injection at coordinates. Nothing else — this
//! is the provenance for apps where only pixels exist.
//!
//! Addressing model: OCR lines are the elements. A text anchor matches a
//! line (exact first, then prefix — the same semantics accessible-name
//! anchors have elsewhere), `nth` disambiguates in reading order, and the
//! selector's `relation` says where the ACTION lands relative to the
//! match: `inside` (buttons — click the text itself) or `right_of` (form
//! fields — the input box sits beside its label). Tree-backed drivers
//! ignore `relation`; here it is the difference between clicking a label
//! and clicking the field next to it.
//!
//! Layering mirrors the SAP adapter: [`VisionAppDriver`] is generic over
//! a [`VisionScreen`] (capture + input; Windows impl = GDI + SendInput)
//! and an [`OcrEngine`] ([`OcrsEngine`] — the pure-Rust `ocrs` models —
//! in production, scripted fakes in tests). The full pipeline is
//! exercised with REAL OCR against synthetically rendered screens in CI.

use std::time::Duration;

use flowproof_driver::{AppDriver, DriverError, KeyMod, PixelRect, UiaSelector};
use image::RgbaImage;

/// One recognized word, in window-relative pixel coordinates. Words are
/// what make a dense screen addressable: a spreadsheet row OCRs as one
/// line, and only its words can be clicked individually.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrWord {
    pub text: String,
    pub rect: PixelRect,
}

/// One recognized line of text, in window-relative pixel coordinates,
/// with the words it is made of.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrLine {
    pub text: String,
    pub rect: PixelRect,
    /// The line's words, left to right. Engines that report per-word boxes
    /// fill this with real geometry; [`OcrLine::new`] estimates it.
    pub words: Vec<OcrWord>,
}

impl OcrLine {
    /// A line whose word boxes are ESTIMATED by splitting its rect in
    /// proportion to each word's character count. Real per-word geometry
    /// is always better, so this is only for engines that cannot report
    /// it; a proportional split is right for a monospaced screen and
    /// approximate for anything else, which is enough to land a click
    /// inside the intended word.
    pub fn new(text: impl Into<String>, rect: PixelRect) -> Self {
        let text = text.into();
        let (x, y, w, h) = rect;
        let total: usize = text.chars().filter(|c| !c.is_whitespace()).count().max(1);
        let per_char = w as f32 / total as f32;
        let mut words = Vec::new();
        let mut consumed = 0usize;
        for word in text.split_whitespace() {
            let len = word.chars().count();
            let start = x + (consumed as f32 * per_char).round() as i32;
            let width = (len as f32 * per_char).round().max(1.0) as u32;
            words.push(OcrWord {
                text: word.to_string(),
                rect: (start, y, width, h),
            });
            consumed += len;
        }
        Self { text, rect, words }
    }
}

/// A resolved anchor: the text that matched and the box it occupies. A
/// hit is a whole line, a single word, or a run of adjacent words - the
/// caller does not care which, it just clicks or reads the box.
#[derive(Debug, Clone, PartialEq)]
struct OcrHit {
    text: String,
    rect: PixelRect,
    /// Sort key for `nth`: the parent line's top edge, then the hit's own
    /// left edge. Fable's ordinal rule is top-to-bottom then
    /// left-to-right, and a word must sort by the ROW it sits in rather
    /// than its own top edge, which wobbles by a pixel or two across a
    /// line of mixed-height glyphs.
    order: (i32, i32),
}

impl OcrHit {
    fn center(&self) -> (i32, i32) {
        let (x, y, w, h) = self.rect;
        (x + w as i32 / 2, y + h as i32 / 2)
    }
}

/// The smallest box containing both, used to join a run of adjacent words
/// into one clickable anchor.
fn union(a: PixelRect, b: PixelRect) -> PixelRect {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    let left = ax.min(bx);
    let top = ay.min(by);
    let right = (ax + aw as i32).max(bx + bw as i32);
    let bottom = (ay + ah as i32).max(by + bh as i32);
    (
        left,
        top,
        (right - left).max(0) as u32,
        (bottom - top).max(0) as u32,
    )
}

/// Text recognition over a captured frame.
pub trait OcrEngine {
    fn recognize(&mut self, frame: &RgbaImage) -> Result<Vec<OcrLine>, DriverError>;
}

/// The pixels-only view of an app: frames in, coordinates out.
pub trait VisionScreen {
    /// Attach to the window whose title contains `window` (bring it to the
    /// foreground). Called by `launch`.
    fn attach(&mut self, window: &str, timeout: Duration) -> Result<(), DriverError>;
    /// Current frame of the attached window (window-relative pixels).
    fn frame(&mut self) -> Result<RgbaImage, DriverError>;
    /// Click at window-relative coordinates.
    fn click(&mut self, x: i32, y: i32) -> Result<(), DriverError>;
    /// Type text into whatever has keyboard focus.
    fn type_text(&mut self, text: &str) -> Result<(), DriverError>;
    /// Press a key chord (canonical key names, e.g. `Enter`, `F5`, `a`).
    fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError>;
    /// Screen origin of the attached window, for absolute-coordinate
    /// artifacts (element rects in recordings).
    fn window_origin(&mut self) -> Result<(i32, i32), DriverError>;
    /// Primary screen size (trace header resolution).
    fn screen_size(&mut self) -> Result<(u32, u32), DriverError>;
}

/// How far right of a label's right edge the field click lands, as a
/// multiple of the label's own height — scale-invariant and inside the
/// input box on typical form layouts.
const RIGHT_OF_GAP_FACTOR: u32 = 1;

/// `AppDriver` over frames + OCR + injected input.
pub struct VisionAppDriver<S: VisionScreen, E: OcrEngine> {
    screen: S,
    ocr: E,
}

impl<S: VisionScreen, E: OcrEngine> VisionAppDriver<S, E> {
    pub fn with_parts(screen: S, ocr: E) -> Self {
        Self { screen, ocr }
    }

    fn lines(&mut self) -> Result<Vec<OcrLine>, DriverError> {
        let frame = self.screen.frame()?;
        self.ocr.recognize(&frame)
    }

    /// Match a text anchor over BOTH granularities, strongest first:
    ///
    /// 1. a whole line, exactly;
    /// 2. a whole word, exactly - or a run of adjacent words for a
    ///    multi-word anchor, joined into one box;
    /// 3. a line by prefix.
    ///
    /// An exact whole token always beats an in-line substring, so
    /// `Click "1"` lands on the digit 1 in a keypad row rather than
    /// somewhere inside "12". Prefix stays last and stays LINE-only:
    /// widening it to words would make every anchor ambiguous on a dense
    /// screen, which is the opposite of what word matching is for.
    ///
    /// Within a tier, `nth` counts top-to-bottom then left-to-right. A
    /// tier that has matches but not an `nth`th one falls through to the
    /// next, which is how line-then-prefix already behaved.
    fn resolve(&mut self, selector: &UiaSelector) -> Result<Option<OcrHit>, DriverError> {
        let Some(needle) = selector.name.as_deref().map(str::trim) else {
            return Ok(None);
        };
        if needle.is_empty() {
            return Ok(None);
        }
        let lines = self.lines()?;
        let nth = selector.nth.unwrap_or(1).max(1) as usize;
        for tier in [Self::exact_lines, Self::exact_words, Self::prefix_lines] {
            let mut hits = tier(&lines, needle);
            hits.sort_by_key(|h| h.order);
            if let Some(hit) = hits.into_iter().nth(nth - 1) {
                return Ok(Some(hit));
            }
        }
        Ok(None)
    }

    fn exact_lines(lines: &[OcrLine], needle: &str) -> Vec<OcrHit> {
        lines
            .iter()
            .filter(|l| l.text.trim() == needle)
            .map(|l| OcrHit {
                text: l.text.clone(),
                rect: l.rect,
                order: (l.rect.1, l.rect.0),
            })
            .collect()
    }

    /// Whole-token matches inside a line. A single-token anchor matches
    /// one word; a multi-token anchor matches a run of ADJACENT words and
    /// takes the union of their boxes, so `Click "Total Due"` clicks the
    /// middle of the phrase rather than either word alone.
    fn exact_words(lines: &[OcrLine], needle: &str) -> Vec<OcrHit> {
        let wanted: Vec<&str> = needle.split_whitespace().collect();
        if wanted.is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for line in lines {
            if line.words.len() < wanted.len() {
                continue;
            }
            for start in 0..=(line.words.len() - wanted.len()) {
                let run = &line.words[start..start + wanted.len()];
                if !run.iter().zip(&wanted).all(|(w, t)| w.text.trim() == *t) {
                    continue;
                }
                let rect = run
                    .iter()
                    .skip(1)
                    .fold(run[0].rect, |acc, w| union(acc, w.rect));
                hits.push(OcrHit {
                    text: needle.to_string(),
                    rect,
                    // The parent line's top edge is the row key, so words
                    // on one visual row sort left to right among
                    // themselves regardless of glyph height.
                    order: (line.rect.1, rect.0),
                });
            }
        }
        hits
    }

    fn prefix_lines(lines: &[OcrLine], needle: &str) -> Vec<OcrHit> {
        lines
            .iter()
            .filter(|l| l.text.trim().starts_with(needle))
            .map(|l| OcrHit {
                text: l.text.clone(),
                rect: l.rect,
                order: (l.rect.1, l.rect.0),
            })
            .collect()
    }

    fn require(&mut self, selector: &UiaSelector) -> Result<OcrHit, DriverError> {
        self.resolve(selector)?.ok_or_else(|| {
            DriverError::Uia(format!(
                "vision: no OCR line or word matches selector [{selector}]"
            ))
        })
    }

    /// The action point for a matched anchor, honoring the selector's
    /// spatial relation (`default_relation` when the trace predates it).
    fn action_point(hit: &OcrHit, selector: &UiaSelector, default_relation: &str) -> (i32, i32) {
        let relation = selector.relation.as_deref().unwrap_or(default_relation);
        let (x, y, w, h) = hit.rect;
        match relation {
            "right_of" => (
                x + w as i32 + (h * RIGHT_OF_GAP_FACTOR) as i32,
                y + h as i32 / 2,
            ),
            "below" => (x + w as i32 / 2, y + h as i32 + h as i32 / 2),
            _ => hit.center(), // "inside" and anything unrecognized
        }
    }
}

impl<S: VisionScreen, E: OcrEngine> AppDriver for VisionAppDriver<S, E> {
    fn launch(
        &mut self,
        _command: &str,
        window_name: &str,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        // Pixels mode never spawns processes: the Citrix/RDP client (or
        // target window) is already on screen; we attach by title.
        self.screen.attach(window_name, timeout)
    }

    fn element_exists(&mut self, selector: &UiaSelector) -> Result<bool, DriverError> {
        Ok(self.resolve(selector)?.is_some())
    }

    fn invoke(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let line = self.require(selector)?;
        let (x, y) = Self::action_point(&line, selector, "inside");
        self.screen.click(x, y)
    }

    fn read_text(&mut self, selector: &UiaSelector) -> Result<String, DriverError> {
        Ok(self.require(selector)?.text)
    }

    fn type_text(&mut self, selector: &UiaSelector, text: &str) -> Result<(), DriverError> {
        // A field is addressed by its label; the input box sits beside it.
        let line = self.require(selector)?;
        let (x, y) = Self::action_point(&line, selector, "right_of");
        self.screen.click(x, y)?;
        self.screen.type_text(text)
    }

    fn clear_text(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let line = self.require(selector)?;
        let (x, y) = Self::action_point(&line, selector, "right_of");
        self.screen.click(x, y)?;
        self.screen.press_key("a", &[KeyMod::Ctrl])?;
        self.screen.press_key("Delete", &[])
    }

    fn type_focused(&mut self, text: &str) -> Result<(), DriverError> {
        self.screen.type_text(text)
    }

    fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError> {
        self.screen.press_key(key, modifiers)
    }

    fn surface_text(&mut self) -> Result<String, DriverError> {
        // The OCR'd frame IS the surface — reading order, one line each.
        Ok(self
            .lines()?
            .into_iter()
            .map(|l| l.text)
            .filter(|t| !t.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
        self.screen.screen_size()
    }

    fn capture(&mut self) -> Result<Option<RgbaImage>, DriverError> {
        self.screen.frame().map(Some)
    }

    fn element_rect(&mut self, selector: &UiaSelector) -> Result<Option<PixelRect>, DriverError> {
        let Some(line) = self.resolve(selector)? else {
            return Ok(None);
        };
        let (ox, oy) = self.screen.window_origin()?;
        let (x, y, w, h) = line.rect;
        Ok(Some((x + ox, y + oy, w, h)))
    }

    fn scene(&mut self) -> Result<Option<String>, DriverError> {
        // The grounding set for LLM authoring: every OCR line is offered
        // as a `text:` TARGET TOKEN — the same neutral contract as every
        // other provenance; the agent needs no vision-specific handling.
        let mut entries: Vec<serde_json::Value> = Vec::new();
        for line in self.lines()? {
            if entries.len() >= 100 {
                break;
            }
            let text = line.text.trim();
            if text.is_empty() {
                continue;
            }
            entries.push(serde_json::json!({
                "target": format!("text:{text}"),
                "text": text,
                "rect": [line.rect.0, line.rect.1, line.rect.2, line.rect.3],
            }));
        }
        serde_json::to_string(&entries)
            .map(Some)
            .map_err(|e| DriverError::Uia(format!("vision: serializing scene: {e}")))
    }
}

/// The production OCR engine: `ocrs` (pure-Rust ONNX inference via rten).
/// Models are cached under `FLOWPROOF_OCR_MODEL_DIR` (default
/// `~/.cache/flowproof/ocrs`) and downloaded on first use — fail-closed
/// with a clear message when offline.
pub struct OcrsEngine {
    engine: ocrs::OcrEngine,
}

const MODEL_BASE_URL: &str = "https://ocrs-models.s3-accelerate.amazonaws.com";
const DETECTION_MODEL: &str = "text-detection.rten";
const RECOGNITION_MODEL: &str = "text-recognition.rten";

impl OcrsEngine {
    pub fn new() -> Result<Self, DriverError> {
        let dir = Self::model_dir()?;
        let detection = Self::ensure_model(&dir, DETECTION_MODEL)?;
        let recognition = Self::ensure_model(&dir, RECOGNITION_MODEL)?;
        let load = |path: &std::path::Path| {
            rten::Model::load_file(path)
                .map_err(|e| DriverError::Uia(format!("vision: loading {}: {e}", path.display())))
        };
        let engine = ocrs::OcrEngine::new(ocrs::OcrEngineParams {
            detection_model: Some(load(&detection)?),
            recognition_model: Some(load(&recognition)?),
            ..Default::default()
        })
        .map_err(|e| DriverError::Uia(format!("vision: initializing OCR engine: {e}")))?;
        Ok(Self { engine })
    }

    fn model_dir() -> Result<std::path::PathBuf, DriverError> {
        if let Ok(dir) = std::env::var("FLOWPROOF_OCR_MODEL_DIR") {
            return Ok(std::path::PathBuf::from(dir));
        }
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map_err(|_| {
                DriverError::Uia(
                    "vision: cannot locate a model cache dir (set FLOWPROOF_OCR_MODEL_DIR)".into(),
                )
            })?;
        Ok(std::path::PathBuf::from(home)
            .join(".cache")
            .join("flowproof")
            .join("ocrs"))
    }

    fn ensure_model(dir: &std::path::Path, name: &str) -> Result<std::path::PathBuf, DriverError> {
        let path = dir.join(name);
        if path.exists() {
            return Ok(path);
        }
        std::fs::create_dir_all(dir)
            .map_err(|e| DriverError::Uia(format!("vision: creating {}: {e}", dir.display())))?;
        let url = format!("{MODEL_BASE_URL}/{name}");
        // One retry: model downloads are ~10 MB and a transient hiccup
        // should not fail a whole recording session.
        let mut bytes = Vec::new();
        let mut last_err = String::new();
        for attempt in 0..2 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(2));
                bytes.clear();
            }
            let result = ureq::get(&url)
                .call()
                .map_err(|e| e.to_string())
                .and_then(|response| {
                    std::io::Read::read_to_end(&mut response.into_body().into_reader(), &mut bytes)
                        .map_err(|e| e.to_string())
                });
            match result {
                Ok(_) => break,
                Err(e) if attempt == 0 => last_err = e,
                Err(e) => {
                    return Err(DriverError::Uia(format!(
                        "vision: downloading OCR model {url}: {e} (first attempt: {last_err}) — \
                         place {name} in {} manually or set FLOWPROOF_OCR_MODEL_DIR",
                        dir.display()
                    )));
                }
            }
        }
        // Write-then-rename so a torn download never poses as a model.
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, &bytes)
            .map_err(|e| DriverError::Uia(format!("vision: writing {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| DriverError::Uia(format!("vision: installing {name}: {e}")))?;
        Ok(path)
    }
}

/// Assign each recognized character to the DETECTED word box it sits in,
/// giving every word both its text and its true geometry.
///
/// The two facts that force this shape:
///
/// - ocrs infers words purely from space characters the recognizer emits,
///   and on a sparse layout it emits none. A 3x3 digit grid with 120px
///   between columns came back as the single words `4 56` and `789`,
///   leaving every cell but the first unaddressable.
/// - Its per-character rects cannot rescue that, because they are
///   APPROXIMATE: the recognizer spreads them evenly across the line, so
///   two digits a hundred pixels apart report touching boxes and there is
///   no gap left to split on.
///
/// The detection stage, though, found the real boxes before recognition
/// ever ran, and `find_text_lines` keeps them. So the geometry comes from
/// detection and only the text comes from recognition. A character is
/// placed in the box it overlaps most; one that overlaps nothing (it fell
/// in a gap) joins the word being built, so no text is ever dropped.
fn segment_words(chars: &[ocrs::TextChar], boxes: &[PixelRect]) -> Vec<OcrWord> {
    if boxes.is_empty() {
        return Vec::new();
    }
    let mut text: Vec<String> = vec![String::new(); boxes.len()];
    let mut last = 0usize;
    for c in chars {
        if c.char == ' ' {
            continue;
        }
        let (left, right) = (c.rect.left(), c.rect.right());
        let best = boxes
            .iter()
            .enumerate()
            .map(|(i, (bx, _, bw, _))| {
                let overlap = (right.min(bx + *bw as i32) - left.max(*bx)).max(0);
                (i, overlap)
            })
            .max_by_key(|(_, overlap)| *overlap)
            .filter(|(_, overlap)| *overlap > 0)
            .map(|(i, _)| i)
            .unwrap_or(last);
        text[best].push(c.char);
        last = best;
    }
    boxes
        .iter()
        .zip(text)
        .filter_map(|(rect, text)| {
            let text = text.trim().to_string();
            if text.is_empty() {
                return None;
            }
            Some(OcrWord { text, rect: *rect })
        })
        .collect()
}

/// ocrs reports bounding boxes as edges; flowproof rects are
/// origin + size. Takes the edges rather than ocrs' rect type, which
/// comes from a transitive crate this one does not depend on directly.
fn to_rect(left: i32, top: i32, right: i32, bottom: i32) -> PixelRect {
    (
        left,
        top,
        (right - left).max(0) as u32,
        (bottom - top).max(0) as u32,
    )
}

impl OcrEngine for OcrsEngine {
    fn recognize(&mut self, frame: &RgbaImage) -> Result<Vec<OcrLine>, DriverError> {
        use ocrs::TextItem;

        let rgb = image::DynamicImage::ImageRgba8(frame.clone()).into_rgb8();
        let source = ocrs::ImageSource::from_bytes(rgb.as_raw(), rgb.dimensions())
            .map_err(|e| DriverError::Uia(format!("vision: preparing OCR input: {e}")))?;
        let input = self
            .engine
            .prepare_input(source)
            .map_err(|e| DriverError::Uia(format!("vision: preparing OCR input: {e}")))?;
        let words = self
            .engine
            .detect_words(&input)
            .map_err(|e| DriverError::Uia(format!("vision: detecting words: {e}")))?;
        let line_rects = self.engine.find_text_lines(&input, &words);
        let lines = self
            .engine
            .recognize_text(&input, &line_rects)
            .map_err(|e| DriverError::Uia(format!("vision: recognizing text: {e}")))?;
        // `line_rects[i]` holds the DETECTED word boxes for line `i`, and
        // recognition returns one entry per line, so the two zip.
        Ok(line_rects
            .iter()
            .zip(lines)
            .filter_map(|(detected, line)| {
                let line = line?;
                let text = line.to_string();
                if text.trim().is_empty() {
                    return None;
                }
                let boxes: Vec<PixelRect> = detected
                    .iter()
                    // `corners` is inherent; the bounding-rect trait lives
                    // in a crate this one does not depend on directly.
                    .map(|word| {
                        let corners = word.corners();
                        let xs = corners.iter().map(|p| p.x);
                        let ys = corners.iter().map(|p| p.y);
                        let fold = |it: &mut dyn Iterator<Item = f32>| {
                            it.fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)))
                        };
                        let (left, right) = fold(&mut xs.into_iter());
                        let (top, bottom) = fold(&mut ys.into_iter());
                        to_rect(
                            left.round() as i32,
                            top.round() as i32,
                            right.round() as i32,
                            bottom.round() as i32,
                        )
                    })
                    .collect();
                let words = segment_words(line.chars(), &boxes);
                Some(OcrLine {
                    text,
                    rect: {
                        let r = line.bounding_rect();
                        to_rect(r.left(), r.top(), r.right(), r.bottom())
                    },
                    words,
                })
            })
            .collect())
    }
}

/// The production screen on Windows: GDI capture of the attached window +
/// SendInput at absolute coordinates, via the driver crate's backend.
#[cfg(windows)]
pub mod native {
    use std::time::{Duration, Instant};

    use flowproof_driver::window::{find_window, WindowInfo};
    use flowproof_driver::{DriverError, Input, InputEvent, KeyMod, MouseButton, PlatformBackend};
    use image::RgbaImage;

    use super::VisionScreen;

    /// Canonical key name → Windows virtual-key code.
    fn virtual_key(key: &str) -> Option<u16> {
        let named = match key {
            "Enter" => 0x0D,
            "Escape" => 0x1B,
            "Tab" => 0x09,
            "Backspace" => 0x08,
            "Delete" => 0x2E,
            "Space" => 0x20,
            "ArrowLeft" => 0x25,
            "ArrowUp" => 0x26,
            "ArrowRight" => 0x27,
            "ArrowDown" => 0x28,
            "Home" => 0x24,
            "End" => 0x23,
            "PageUp" => 0x21,
            "PageDown" => 0x22,
            _ => 0,
        };
        if named != 0 {
            return Some(named);
        }
        if let Some(n) = key
            .strip_prefix(['F', 'f'])
            .and_then(|n| n.parse::<u16>().ok())
        {
            if (1..=12).contains(&n) {
                return Some(0x6F + n); // F1 = 0x70
            }
        }
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_alphanumeric() => {
                Some(c.to_ascii_uppercase() as u16) // VK for A-Z/0-9 equals ASCII
            }
            _ => None,
        }
    }

    fn modifier_key(m: &KeyMod) -> u16 {
        match m {
            KeyMod::Ctrl => 0x11,
            KeyMod::Alt => 0x12,
            KeyMod::Shift => 0x10,
            KeyMod::Meta => 0x5B,
        }
    }

    pub struct NativeScreen {
        backend: PlatformBackend,
        window: Option<WindowInfo>,
    }

    impl NativeScreen {
        pub fn new() -> Self {
            Self {
                backend: PlatformBackend::new(),
                window: None,
            }
        }

        fn window(&mut self) -> Result<&mut WindowInfo, DriverError> {
            self.window
                .as_mut()
                .ok_or_else(|| DriverError::Uia("vision: not attached: call launch first".into()))
        }

        fn inject(&mut self, event: InputEvent) -> Result<(), DriverError> {
            self.backend.inject(&event)
        }
    }

    impl Default for NativeScreen {
        fn default() -> Self {
            Self::new()
        }
    }

    impl VisionScreen for NativeScreen {
        fn attach(&mut self, window: &str, timeout: Duration) -> Result<(), DriverError> {
            if window.trim().is_empty() {
                return Err(DriverError::Uia(
                    "vision: spec needs `window:` — the title (substring) of the window to drive"
                        .into(),
                ));
            }
            let deadline = Instant::now() + timeout;
            loop {
                if let Some(info) = find_window(window)? {
                    info.focus()?;
                    std::thread::sleep(Duration::from_millis(200));
                    self.window = Some(info);
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(DriverError::Uia(format!(
                        "vision: no visible window with '{window}' in its title"
                    )));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }

        fn frame(&mut self) -> Result<RgbaImage, DriverError> {
            let (x, y, w, h) = self.window()?.refresh_rect()?;
            let screen = flowproof_driver::gdi::capture_screen()?;
            let x = x.max(0) as u32;
            let y = y.max(0) as u32;
            let w = w.min(screen.width().saturating_sub(x)).max(1);
            let h = h.min(screen.height().saturating_sub(y)).max(1);
            Ok(image::imageops::crop_imm(&screen, x, y, w, h).to_image())
        }

        fn click(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
            let (wx, wy, _, _) = self.window()?.refresh_rect()?;
            let (ax, ay) = (wx + x, wy + y);
            self.inject(InputEvent::MouseMove { x: ax, y: ay })?;
            std::thread::sleep(Duration::from_millis(60));
            self.inject(InputEvent::MouseDown {
                button: MouseButton::Left,
            })?;
            std::thread::sleep(Duration::from_millis(40));
            self.inject(InputEvent::MouseUp {
                button: MouseButton::Left,
            })?;
            std::thread::sleep(Duration::from_millis(120));
            Ok(())
        }

        fn type_text(&mut self, text: &str) -> Result<(), DriverError> {
            self.inject(InputEvent::Text {
                text: text.to_string(),
            })?;
            std::thread::sleep(Duration::from_millis(120));
            Ok(())
        }

        fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError> {
            let vk = virtual_key(key)
                .ok_or_else(|| DriverError::Uia(format!("vision: no virtual key for '{key}'")))?;
            for m in modifiers {
                self.inject(InputEvent::KeyDown {
                    virtual_key: modifier_key(m),
                })?;
            }
            self.inject(InputEvent::KeyDown { virtual_key: vk })?;
            self.inject(InputEvent::KeyUp { virtual_key: vk })?;
            for m in modifiers.iter().rev() {
                self.inject(InputEvent::KeyUp {
                    virtual_key: modifier_key(m),
                })?;
            }
            std::thread::sleep(Duration::from_millis(120));
            Ok(())
        }

        fn window_origin(&mut self) -> Result<(i32, i32), DriverError> {
            let (x, y, _, _) = self.window()?.refresh_rect()?;
            Ok((x, y))
        }

        fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
            let screen = flowproof_driver::gdi::capture_screen()?;
            Ok((screen.width(), screen.height()))
        }
    }
}

#[cfg(windows)]
impl VisionAppDriver<native::NativeScreen, OcrsEngine> {
    /// The production driver: GDI + SendInput + ocrs.
    pub fn new() -> Result<Self, DriverError> {
        Ok(Self::with_parts(
            native::NativeScreen::new(),
            OcrsEngine::new()?,
        ))
    }
}

/// Scripted screen and OCR for tests — the whole record→trace→replay
/// pipeline runs against these on any platform (pair the fake screen with
/// the REAL `OcrsEngine` for end-to-end OCR coverage).
pub mod fake {
    use super::*;

    /// A screen made of pre-rendered frames. A click inside
    /// `advance_on_click` moves to the next frame — the scripted "the app
    /// reacted" effect.
    #[derive(Default)]
    pub struct FakeScreen {
        pub frames: Vec<RgbaImage>,
        pub current: usize,
        pub advance_on_click: Option<PixelRect>,
        pub attached: Option<String>,
        pub clicks: Vec<(i32, i32)>,
        pub typed: Vec<String>,
        pub keys: Vec<(String, Vec<KeyMod>)>,
    }

    impl FakeScreen {
        pub fn with_frames(frames: Vec<RgbaImage>) -> Self {
            Self {
                frames,
                ..Self::default()
            }
        }
    }

    impl VisionScreen for FakeScreen {
        fn attach(&mut self, window: &str, _timeout: Duration) -> Result<(), DriverError> {
            self.attached = Some(window.to_string());
            Ok(())
        }

        fn frame(&mut self) -> Result<RgbaImage, DriverError> {
            self.frames
                .get(self.current)
                .cloned()
                .ok_or_else(|| DriverError::Uia("fake screen has no frame".into()))
        }

        fn click(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
            self.clicks.push((x, y));
            if let Some((rx, ry, rw, rh)) = self.advance_on_click {
                let inside = x >= rx && y >= ry && x < rx + rw as i32 && y < ry + rh as i32;
                if inside && self.current + 1 < self.frames.len() {
                    self.current += 1;
                }
            }
            Ok(())
        }

        fn type_text(&mut self, text: &str) -> Result<(), DriverError> {
            self.typed.push(text.to_string());
            Ok(())
        }

        fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError> {
            self.keys.push((key.to_string(), modifiers.to_vec()));
            Ok(())
        }

        fn window_origin(&mut self) -> Result<(i32, i32), DriverError> {
            Ok((0, 0))
        }

        fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
            Ok((1920, 1080))
        }
    }

    /// Scripted OCR: returns a fixed line set per frame content hash — or
    /// simply the configured lines, ignoring the frame.
    #[derive(Default)]
    pub struct FakeOcr {
        pub lines: Vec<OcrLine>,
    }

    impl FakeOcr {
        pub fn with_lines(lines: Vec<OcrLine>) -> Self {
            Self { lines }
        }
    }

    impl OcrEngine for FakeOcr {
        fn recognize(&mut self, _frame: &RgbaImage) -> Result<Vec<OcrLine>, DriverError> {
            Ok(self.lines.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeOcr, FakeScreen};
    use super::*;

    fn blank_frame() -> RgbaImage {
        RgbaImage::from_pixel(400, 200, image::Rgba([255, 255, 255, 255]))
    }

    fn login_lines() -> Vec<OcrLine> {
        vec![
            OcrLine::new("User name", (20, 20, 90, 16)),
            OcrLine::new("Password", (20, 60, 80, 16)),
            OcrLine::new("Sign in", (20, 100, 70, 20)),
            OcrLine::new("Sign in with SSO", (20, 140, 150, 16)),
        ]
    }

    fn driver() -> VisionAppDriver<FakeScreen, FakeOcr> {
        VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![blank_frame()]),
            FakeOcr::with_lines(login_lines()),
        )
    }

    fn anchor(text: &str) -> UiaSelector {
        UiaSelector {
            name: Some(text.into()),
            ..UiaSelector::default()
        }
    }

    #[test]
    fn typing_clicks_right_of_the_label_then_types() {
        let mut d = driver();
        d.type_text(&anchor("User name"), "ada").expect("types");
        // Label ends at x=110, height 16 → click lands one text-height right.
        assert_eq!(d.screen.clicks, vec![(126, 28)]);
        assert_eq!(d.screen.typed, vec!["ada".to_string()]);
    }

    #[test]
    fn invoke_clicks_the_text_itself() {
        let mut d = driver();
        d.invoke(&anchor("Sign in")).expect("clicks");
        assert_eq!(d.screen.clicks, vec![(55, 110)]);
    }

    #[test]
    fn exact_match_beats_prefix_match() {
        let mut d = driver();
        // "Sign in" matches both "Sign in" and "Sign in with SSO" — the
        // exact line must win.
        let rect = d
            .element_rect(&anchor("Sign in"))
            .expect("resolves")
            .expect("found");
        assert_eq!(rect, (20, 100, 70, 20));
        // Prefix matching still works when no exact line exists.
        assert!(d.element_exists(&anchor("Sign in with")).expect("walks"));
    }

    #[test]
    fn nth_disambiguates_in_reading_order() {
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![blank_frame()]),
            FakeOcr::with_lines(vec![
                OcrLine::new("Amount", (20, 20, 60, 16)),
                OcrLine::new("Amount", (20, 60, 60, 16)),
            ]),
        );
        let second = anchor("Amount").with_nth(Some(2));
        let rect = d.element_rect(&second).expect("resolves").expect("found");
        assert_eq!(rect.1, 60);
    }

    #[test]
    fn explicit_relation_overrides_the_default() {
        let mut d = driver();
        let below = anchor("User name").with_relation(Some("below".into()));
        d.type_text(&below, "x").expect("types");
        assert_eq!(d.screen.clicks, vec![(65, 44)]);
    }

    #[test]
    fn surface_and_scene_expose_the_ocr_lines() {
        let mut d = driver();
        let surface = d.surface_text().expect("surface");
        assert!(surface.contains("Password") && surface.contains("Sign in"));
        let scene = d.scene().expect("scene").expect("json");
        assert!(scene.contains(r#""target":"text:Sign in""#));
        assert!(scene.contains("\"rect\""));
    }

    #[test]
    fn unmatched_anchor_is_a_clean_error() {
        let mut d = driver();
        let err = d
            .invoke(&anchor("Logout"))
            .expect_err("no such text on screen");
        assert!(err.to_string().contains("no OCR line or word matches"));
    }

    #[test]
    fn clear_selects_all_then_deletes() {
        let mut d = driver();
        d.clear_text(&anchor("Password")).expect("clears");
        assert_eq!(
            d.screen.keys,
            vec![
                ("a".to_string(), vec![KeyMod::Ctrl]),
                ("Delete".to_string(), vec![])
            ]
        );
    }
}

#[cfg(test)]
mod word_matching_tests {
    use super::*;

    /// A keypad row: one OCR line, three addressable cells. Word boxes are
    /// spelled out rather than estimated so the assertions are about the
    /// MATCHER, not about the estimator.
    fn keypad() -> Vec<OcrLine> {
        vec![
            OcrLine {
                text: "1 2 3".into(),
                rect: (10, 10, 230, 20),
                words: vec![
                    OcrWord {
                        text: "1".into(),
                        rect: (10, 10, 10, 20),
                    },
                    OcrWord {
                        text: "2".into(),
                        rect: (110, 10, 10, 20),
                    },
                    OcrWord {
                        text: "3".into(),
                        rect: (230, 10, 10, 20),
                    },
                ],
            },
            OcrLine {
                text: "Total Due 42".into(),
                rect: (10, 60, 300, 20),
                words: vec![
                    OcrWord {
                        text: "Total".into(),
                        rect: (10, 60, 60, 20),
                    },
                    OcrWord {
                        text: "Due".into(),
                        rect: (80, 60, 40, 20),
                    },
                    OcrWord {
                        text: "42".into(),
                        rect: (130, 60, 20, 20),
                    },
                ],
            },
        ]
    }

    fn hit(needle: &str, nth: Option<u32>) -> Option<OcrHit> {
        let mut driver = VisionAppDriver::with_parts(
            fake::FakeScreen::with_frames(vec![RgbaImage::from_pixel(
                400,
                200,
                image::Rgba([255, 255, 255, 255]),
            )]),
            fake::FakeOcr::with_lines(keypad()),
        );
        driver
            .resolve(&UiaSelector {
                name: Some(needle.into()),
                nth,
                ..Default::default()
            })
            .expect("resolve runs")
    }

    /// The whole point of #69: a cell inside a line becomes addressable.
    /// Before word matching, "2" matched no line exactly and no line by
    /// prefix either, because the row starts with "1".
    #[test]
    fn a_word_inside_a_line_is_addressable() {
        assert_eq!(hit("2", None).expect("2 resolves").rect, (110, 10, 10, 20));
        assert_eq!(hit("3", None).expect("3 resolves").rect, (230, 10, 10, 20));
    }

    /// An exact whole token beats an in-line substring: "1" is the first
    /// word of the keypad row AND a prefix of the line "1 2 3". The word
    /// must win, or clicking "1" would click the middle of the whole row.
    #[test]
    fn an_exact_word_beats_a_line_prefix() {
        let digit = hit("1", None).expect("1 resolves");
        assert_eq!(digit.rect, (10, 10, 10, 20), "the digit, not the row");
    }

    /// A whole line still wins over any word: the strongest match first.
    #[test]
    fn an_exact_line_still_wins() {
        let line = hit("1 2 3", None).expect("the line resolves");
        assert_eq!(line.rect, (10, 10, 230, 20));
    }

    /// A multi-word anchor matches ADJACENT words and takes the union, so
    /// the click lands in the middle of the phrase.
    #[test]
    fn a_multi_word_anchor_joins_adjacent_words() {
        let phrase = hit("Total Due", None).expect("phrase resolves");
        assert_eq!(phrase.rect, (10, 60, 110, 20), "union of Total and Due");
        // Non-adjacent words must NOT join across the gap.
        assert!(hit("Total 42", None).is_none(), "words are not adjacent");
    }

    /// Ordinals count top-to-bottom then left-to-right.
    #[test]
    fn ordinals_run_in_reading_order() {
        let lines = vec![
            OcrLine {
                text: "x y".into(),
                rect: (10, 10, 100, 20),
                words: vec![
                    OcrWord {
                        text: "go".into(),
                        rect: (10, 10, 30, 20),
                    },
                    OcrWord {
                        text: "go".into(),
                        rect: (60, 10, 30, 20),
                    },
                ],
            },
            OcrLine {
                text: "z".into(),
                rect: (10, 60, 30, 20),
                words: vec![OcrWord {
                    text: "go".into(),
                    rect: (10, 60, 30, 20),
                }],
            },
        ];
        let mut driver = VisionAppDriver::with_parts(
            fake::FakeScreen::with_frames(vec![RgbaImage::from_pixel(
                400,
                200,
                image::Rgba([255, 255, 255, 255]),
            )]),
            fake::FakeOcr::with_lines(lines),
        );
        let mut nth = |n: u32| {
            driver
                .resolve(&UiaSelector {
                    name: Some("go".into()),
                    nth: Some(n),
                    ..Default::default()
                })
                .expect("resolve runs")
                .expect("resolves")
                .rect
        };
        assert_eq!(nth(1), (10, 10, 30, 20), "top row, left");
        assert_eq!(nth(2), (60, 10, 30, 20), "top row, right");
        assert_eq!(nth(3), (10, 60, 30, 20), "second row");
    }

    /// `OcrLine::new` estimates word boxes for engines that cannot report
    /// them: each word inside the line's box, in order, non-overlapping.
    #[test]
    fn estimated_word_boxes_stay_inside_the_line() {
        let line = OcrLine::new("one two three", (100, 50, 260, 20));
        let texts: Vec<&str> = line.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["one", "two", "three"]);
        for word in &line.words {
            let (x, y, w, h) = word.rect;
            assert!(x >= 100 && x + w as i32 <= 100 + 260, "{word:?}");
            assert_eq!((y, h), (50, 20));
        }
        for pair in line.words.windows(2) {
            assert!(pair[0].rect.0 < pair[1].rect.0, "left to right");
        }
    }
}
