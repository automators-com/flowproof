//! The pixels-only adapter (`vision`): Citrix, RDP, or any window driven
//! without an accessibility API. Perception is OCR over captured frames;
//! action is OS-level input injection at coordinates. Nothing else — this
//! is the provenance for apps where only pixels exist.
//!
//! Addressing model: OCR lines are the elements. A text anchor matches a
//! line (exact first, then exact word, then line prefix — the same
//! exact-over-loose semantics accessible-name anchors have elsewhere),
//! `nth` disambiguates in reading order, and the
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

/// One recognized word inside a line, in window-relative pixel coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrWord {
    pub text: String,
    pub rect: PixelRect,
}

/// One recognized line of text, in window-relative pixel coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrLine {
    pub text: String,
    pub rect: PixelRect,
    /// Per-word boxes when the engine provides them (#69). Empty means
    /// "unknown", and word-level matching derives boxes by splitting the
    /// line's box proportionally at whitespace gaps.
    pub words: Vec<OcrWord>,
}

impl OcrLine {
    pub fn new(text: impl Into<String>, rect: PixelRect) -> Self {
        Self {
            text: text.into(),
            rect,
            words: Vec::new(),
        }
    }

    fn center(&self) -> (i32, i32) {
        let (x, y, w, h) = self.rect;
        (x + w as i32 / 2, y + h as i32 / 2)
    }

    /// Word boxes for matching: the engine's own when present, otherwise
    /// derived by proportional gap-splitting. Crude for proportional
    /// fonts, but a key grid's tokens are short and well separated —
    /// which is exactly the layout that needs this (#69: the digits of a
    /// keypad share one OCR line, so `Click "5"` cannot land by line).
    fn word_boxes(&self) -> Vec<OcrWord> {
        if !self.words.is_empty() {
            return self.words.clone();
        }
        let chars: Vec<char> = self.text.chars().collect();
        let total = chars.len();
        if total == 0 {
            return Vec::new();
        }
        let (x, y, w, h) = self.rect;
        let char_x = |i: usize| x + (w as f64 * i as f64 / total as f64).round() as i32;
        let mut out = Vec::new();
        let mut start = None;
        for i in 0..=total {
            let boundary = i == total || chars[i].is_whitespace();
            match (start, boundary) {
                (None, false) => start = Some(i),
                (Some(s), true) => {
                    out.push(OcrWord {
                        text: chars[s..i].iter().collect(),
                        rect: (char_x(s), y, (char_x(i) - char_x(s)).max(1) as u32, h),
                    });
                    start = None;
                }
                _ => {}
            }
        }
        out
    }
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

    /// Match a text anchor against the current OCR lines: exact line
    /// first, then exact WORD, then line prefix — never substring, so
    /// `Click "Save"` cannot hit "Save As" when a plain "Save" exists.
    ///
    /// The word tier is why a key grid works (#69): OCR merges a keypad
    /// row into one line ("4 5 6"), and `Click "5"` must land on the
    /// digit's own box, not the row's center. It sits above the prefix
    /// tier deliberately — a lone "5" on screen must beat a line that
    /// merely STARTS with 5 ("56 items").
    fn resolve(&mut self, selector: &UiaSelector) -> Result<Option<OcrLine>, DriverError> {
        let Some(needle) = selector.name.as_deref().map(str::trim) else {
            return Ok(None);
        };
        if needle.is_empty() {
            return Ok(None);
        }
        let lines = self.lines()?;
        let nth = selector.nth.unwrap_or(1).max(1) as usize;
        let exact = lines
            .iter()
            .filter(|l| l.text.trim() == needle)
            .nth(nth - 1)
            .cloned();
        if exact.is_some() {
            return Ok(exact);
        }
        // A multi-word needle can't be a single word; skip the allocation.
        if !needle.contains(char::is_whitespace) {
            let word = lines
                .iter()
                .flat_map(|l| l.word_boxes())
                .filter(|w| w.text.trim() == needle)
                .nth(nth - 1)
                .map(|w| OcrLine::new(w.text, w.rect));
            if word.is_some() {
                return Ok(word);
            }
        }
        Ok(lines
            .iter()
            .filter(|l| l.text.trim().starts_with(needle))
            .nth(nth - 1)
            .cloned())
    }

    fn require(&mut self, selector: &UiaSelector) -> Result<OcrLine, DriverError> {
        self.resolve(selector)?.ok_or_else(|| {
            DriverError::Uia(format!("vision: no OCR line matches selector [{selector}]"))
        })
    }

    /// The action point for a matched anchor, honoring the selector's
    /// spatial relation (`default_relation` when the trace predates it).
    fn action_point(line: &OcrLine, selector: &UiaSelector, default_relation: &str) -> (i32, i32) {
        let relation = selector.relation.as_deref().unwrap_or(default_relation);
        let (x, y, w, h) = line.rect;
        match relation {
            "right_of" => (
                x + w as i32 + (h * RIGHT_OF_GAP_FACTOR) as i32,
                y + h as i32 / 2,
            ),
            "below" => (x + w as i32 / 2, y + h as i32 + h as i32 / 2),
            _ => line.center(), // "inside" and anything unrecognized
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
        Ok(lines
            .into_iter()
            .flatten()
            .filter_map(|line| {
                let text = line.to_string();
                if text.trim().is_empty() {
                    return None;
                }
                fn to_pixel_rect(item: &impl TextItem) -> flowproof_driver::PixelRect {
                    let rect = item.bounding_rect();
                    (
                        rect.left(),
                        rect.top(),
                        (rect.right() - rect.left()).max(0) as u32,
                        (rect.bottom() - rect.top()).max(0) as u32,
                    )
                }
                // The engine already segments words to find lines — carry
                // their boxes through so single-token anchors can land on
                // a key grid (#69).
                let words = line
                    .words()
                    .map(|word| OcrWord {
                        text: word.to_string(),
                        rect: to_pixel_rect(&word),
                    })
                    .filter(|w| !w.text.trim().is_empty())
                    .collect();
                Some(OcrLine {
                    text,
                    rect: to_pixel_rect(&line),
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

    /// #69: OCR merges a keypad row into one line, so `Click "5"` must
    /// resolve at word level — via the engine's own word boxes when it
    /// has them, via proportional gap-splitting when it doesn't.
    #[test]
    fn single_token_anchor_lands_on_its_word_in_a_key_grid() {
        // Engine-provided word boxes: click the exact box.
        let row = OcrLine {
            text: "4 5 6".into(),
            rect: (0, 100, 300, 40),
            words: vec![
                OcrWord {
                    text: "4".into(),
                    rect: (10, 100, 60, 40),
                },
                OcrWord {
                    text: "5".into(),
                    rect: (120, 100, 60, 40),
                },
                OcrWord {
                    text: "6".into(),
                    rect: (230, 100, 60, 40),
                },
            ],
        };
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![blank_frame()]),
            FakeOcr::with_lines(vec![row]),
        );
        d.invoke(&anchor("5")).expect("clicks the 5 key");
        assert_eq!(d.screen.clicks, vec![(150, 120)]);

        // No word boxes: the line's box splits proportionally at the gaps,
        // so the click still lands in the middle third, not the row center.
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![blank_frame()]),
            FakeOcr::with_lines(vec![OcrLine::new("4 5 6", (0, 100, 300, 40))]),
        );
        d.invoke(&anchor("5")).expect("clicks the 5 key");
        let (x, y) = d.screen.clicks[0];
        assert!(
            (100..200).contains(&x),
            "x should be in the middle third: {x}"
        );
        assert_eq!(y, 120);
    }

    /// The word tier outranks the prefix tier: a lone "5" on screen beats
    /// a line that merely STARTS with 5 — but an exact LINE still beats a
    /// word, and multi-word needles never match at word level.
    #[test]
    fn word_matches_beat_prefix_matches_but_not_exact_lines() {
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![blank_frame()]),
            FakeOcr::with_lines(vec![
                OcrLine::new("56 items", (0, 0, 160, 20)),
                OcrLine::new("4 5 6", (0, 100, 300, 40)),
            ]),
        );
        let rect = d
            .element_rect(&anchor("5"))
            .expect("resolves")
            .expect("found");
        assert_eq!(
            rect.1, 100,
            "must match the keypad row's word, got {rect:?}"
        );
        // Prefix matching still serves needles no word satisfies.
        assert!(d.element_exists(&anchor("56 it")).expect("walks"));
    }

    #[test]
    fn unmatched_anchor_is_a_clean_error() {
        let mut d = driver();
        let err = d
            .invoke(&anchor("Logout"))
            .expect_err("no such text on screen");
        assert!(err.to_string().contains("no OCR line matches"));
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
