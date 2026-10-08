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

use flowproof_driver::{AppDriver, DriverError, KeyMod, PixelRect, ScrollTo, UiaSelector};
use image::RgbaImage;

mod start;

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
    /// The box is the CONTROL itself - found in the accessibility tree -
    /// not text standing for it. A label's relation (`right_of`, `below`)
    /// says where its field is from the label; a control needs no such
    /// guess, and an action lands inside it whatever the relation.
    control: bool,
}

impl OcrHit {
    fn center(&self) -> (i32, i32) {
        let (x, y, w, h) = self.rect;
        (x + w as i32 / 2, y + h as i32 / 2)
    }
}

/// An element vision can address on the current frame, and where a click
/// on it lands - both window-relative.
#[derive(Debug, Clone, PartialEq)]
pub struct VisionTarget {
    pub text: String,
    pub rect: PixelRect,
    pub point: (i32, i32),
}

/// The smallest box containing both, used to join a run of adjacent words
/// into one clickable anchor.
pub(crate) fn union(a: PixelRect, b: PixelRect) -> PixelRect {
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

    /// A closer reading of the same frame, `scale` times its size, taken
    /// when a target was not found in the first. Coordinates are the
    /// frame's own.
    fn recognize_closer(
        &mut self,
        frame: &RgbaImage,
        _scale: u32,
    ) -> Result<Vec<OcrLine>, DriverError> {
        self.recognize(frame)
    }
}

/// A control the window's accessibility tree names, window-relative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibleControl {
    pub name: String,
    pub control_type: String,
    pub rect: PixelRect,
}

/// The pixels-only view of an app: frames in, coordinates out.
pub trait VisionScreen {
    /// The named, interactive controls the attached window's accessibility
    /// tree reports, window-relative: a second source of elements beside
    /// OCR, as Microsoft's UFO2 agent fuses UIA with vision on Windows. An
    /// icon-only button has no text for OCR, but has a name. Empty for a
    /// screen with no tree - over Citrix or RDP the client reports one
    /// opaque pane, and vision stays pixels-only there.
    fn accessible_controls(&mut self) -> Result<Vec<AccessibleControl>, DriverError> {
        Ok(Vec::new())
    }
    /// Whether a click at window-relative `(x, y)` reaches this window, not
    /// another drawn over it. Asked before acting on a control found in the
    /// tree, which reports a control under a popup menu as if it showed.
    fn reaches(&mut self, _x: i32, _y: i32) -> Result<bool, DriverError> {
        Ok(true)
    }
    /// Attach to the window whose title contains `window` (bring it to the
    /// foreground). Called by `launch`.
    fn attach(&mut self, window: &str, timeout: Duration) -> Result<(), DriverError>;
    /// Current frame of the attached window (window-relative pixels).
    fn frame(&mut self) -> Result<RgbaImage, DriverError>;
    /// Click at window-relative coordinates.
    fn click(&mut self, x: i32, y: i32) -> Result<(), DriverError>;
    /// Double-click (left) at window-relative coordinates, inside the
    /// system double-click interval.
    fn double_click(&mut self, x: i32, y: i32) -> Result<(), DriverError>;
    /// Right-click at window-relative coordinates.
    fn right_click(&mut self, x: i32, y: i32) -> Result<(), DriverError>;
    /// Type text into whatever has keyboard focus.
    fn type_text(&mut self, text: &str) -> Result<(), DriverError>;
    /// Press a key chord (canonical key names, e.g. `Enter`, `F5`, `a`).
    fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError>;
    /// Wait, bounded, for the window to stop changing: an action's effect
    /// (a menu fading in, a page loading, a mode switch redrawing the
    /// keypad) takes frames to finish, and a target read mid-way is read
    /// wrong or not at all. Anthropic's reference computer-use loop waits
    /// after every action for the same reason. A screen that never changes
    /// on its own need not wait.
    fn settle(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
    /// Move the pointer onto window-relative coordinates, pressing nothing:
    /// what opens a tooltip or a hover menu.
    fn move_to(&mut self, _x: i32, _y: i32) -> Result<(), DriverError> {
        Err(DriverError::Uia(
            "vision: this screen cannot move the pointer".into(),
        ))
    }
    /// Press the left button at `from`, move to `to` in steps an app sees as
    /// a drag, release there.
    fn drag(&mut self, _from: (i32, i32), _to: (i32, i32)) -> Result<(), DriverError> {
        Err(DriverError::Uia("vision: this screen cannot drag".into()))
    }
    /// Turn the mouse wheel over the window by `notches` (negative scrolls
    /// the view down) and let the frame settle.
    fn scroll(&mut self, notches: i32) -> Result<(), DriverError>;
    /// How long a click's effect may take to show before the click is taken
    /// to have gone nowhere and is made once more - see
    /// [`VisionAppDriver::press`]. Zero checks nothing.
    fn click_wait(&self) -> Duration {
        Duration::ZERO
    }
    /// Rest the pointer on window-relative `(x, y)` long enough for a hover
    /// tint, not a tooltip, so the frame before a click already shows it.
    fn point_at(&mut self, _x: i32, _y: i32) -> Result<(), DriverError> {
        Ok(())
    }
    /// How long to wait between two captures so a blinking text caret is
    /// on in one and off in the other: half a blink. Zero for a screen
    /// that does not blink.
    fn blink_gap(&self) -> Duration {
        Duration::ZERO
    }
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

/// Wheel notches per search step. A browser scrolls about 100px a notch,
/// so five move well under one screen of a maximized window and
/// consecutive frames overlap: a line straddling the fold is read whole in
/// one of them.
const SCROLL_NOTCHES_PER_PAGE: i32 = 5;

/// Search steps per direction before giving up. Bounds the cost of a
/// target that is not on the page at all, and of OCR noise that keeps an
/// edge from ever reading as "no change".
const MAX_SCROLL_PAGES: usize = 20;

/// `FLOWPROOF_VISION_LOG=1` prints, to stderr, what each vision action
/// matched (the OCR text and its box), the point it acted on, each scroll,
/// and the typed-value read-back counts. A pixels-only run has no DOM to
/// inspect afterwards; this is the record of where the clicks went, which
/// is the first question when a Citrix flow misbehaves (plan 15, finding
/// 26). Never the typed text or its read-back keys - the driver sees
/// resolved values, and one may be a password.
fn vision_log(message: impl FnOnce() -> String) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| {
        std::env::var("FLOWPROOF_VISION_LOG").is_ok_and(|v| !v.is_empty() && v != "0")
    }) {
        eprintln!("[vision] {}", message());
    }
}

/// Wheel notches to bring an action point that fell past the window's edge
/// back inside it - a couple of lines, not a page.
const EDGE_NUDGE_NOTCHES: i32 = 2;

/// `AppDriver` over frames + OCR + injected input.
pub struct VisionAppDriver<S: VisionScreen, E: OcrEngine> {
    screen: S,
    ocr: E,
    /// The last `type_text`: its value's read-back keys and how many times
    /// each was on screen BEFORE typing - see `typed_value_arrived`.
    typed_baseline: Option<(Vec<String>, Vec<usize>)>,
    /// Height of the last frame read, to keep action points inside it.
    frame_height: u32,
    /// The last frame read and its reading, at screen size and each closer
    /// scale.
    read_cache: [Option<(RgbaImage, Vec<OcrLine>)>; 1 + CLOSER_SCALES.len()],
    /// The accessible controls read with the last frame, for as long as
    /// that frame is what the screen shows: a tree walk costs more than a
    /// look, and one step looks several times.
    controls_cache: Option<(RgbaImage, Vec<AccessibleControl>)>,
    window_title: Option<String>,
}

impl<S: VisionScreen, E: OcrEngine> VisionAppDriver<S, E> {
    pub fn with_parts(screen: S, ocr: E) -> Self {
        Self {
            screen,
            ocr,
            typed_baseline: None,
            frame_height: 0,
            read_cache: [None, None, None],
            controls_cache: None,
            window_title: None,
        }
    }

    /// How a typed value is recognised on screen: letters and digits only,
    /// lowercased, because OCR drops punctuation (plan 15, finding 6). A
    /// value of eight or more gives TWO keys, either of which will do: its
    /// first six, and the six before its last two. The head, because a
    /// value's last characters are where OCR stumbles - `Quokka77` was read
    /// `Quokka/`; the near-tail, because a value longer than its field
    /// scrolls and leaves only the end in view. A shorter value is its own
    /// key; under three characters, none - too short to tell from the page.
    ///
    /// Arithmetic operators split a value into pieces keyed apart: keystrokes
    /// like `12+30=` are an instruction to the app, not text it shows back -
    /// Calculator showed `42`, and the key `1230` failed a typing that had
    /// worked.
    fn readback_keys(text: &str) -> Vec<String> {
        let mut keys: Vec<String> = text
            .split(['+', '*', '/', '=', '×', '÷'])
            .flat_map(|piece| {
                let chars: Vec<char> = piece
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect();
                let n = chars.len();
                match n {
                    0..=2 => Vec::new(),
                    3..=7 => vec![chars.iter().collect()],
                    _ => vec![
                        chars[..6].iter().collect(),
                        chars[n - 8..n - 2].iter().collect(),
                    ],
                }
            })
            .collect();
        keys.dedup();
        keys
    }

    /// Note how often a value's read-back keys are on screen just before it
    /// is typed - the baseline `typed_value_arrived` compares against.
    fn count_before_typing(&mut self, text: &str) -> Result<(), DriverError> {
        vision_log(|| format!("typing {} character(s)", text.chars().count()));
        let keys = Self::readback_keys(text);
        self.typed_baseline = if keys.is_empty() {
            None
        } else {
            let before = self.readback_counts(&keys)?;
            Some((keys, before))
        };
        Ok(())
    }

    /// How many times each key is on screen now. Across a caret blink both
    /// readings count, and the higher one is the answer.
    fn readback_counts(&mut self, keys: &[String]) -> Result<Vec<usize>, DriverError> {
        let readings = self.steady_lines()?;
        let counts: Vec<usize> = keys
            .iter()
            .map(|key| {
                readings
                    .iter()
                    .map(|lines| Self::key_count(lines, key))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        vision_log(|| {
            format!(
                "read-back: {} key(s) on screen {counts:?} times",
                keys.len()
            )
        });
        Ok(counts)
    }

    fn key_count(lines: &[OcrLine], key: &str) -> usize {
        lines
            .iter()
            .map(|l| {
                let text: String = l
                    .text
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect();
                text.matches(key).count()
            })
            .sum()
    }

    /// OCR a frame, reusing the reading of the last identical one. OCR is
    /// deterministic, and one step reads the same screen several times -
    /// reveal, wait, act - so a replayed Calculator flow spent a minute and
    /// a half re-reading pixels it had already read, its closer looks the
    /// costliest of all.
    fn read(&mut self, frame: &RgbaImage, scale: u32) -> Result<Vec<OcrLine>, DriverError> {
        let slot = CLOSER_SCALES
            .iter()
            .position(|s| *s == scale)
            .map_or(0, |i| i + 1);
        if let Some((cached, lines)) = &self.read_cache[slot] {
            if cached == frame {
                return Ok(lines.clone());
            }
        }
        let lines = if scale > 1 {
            self.ocr.recognize_closer(frame, scale)?
        } else {
            self.ocr.recognize(frame)?
        };
        self.read_cache[slot] = Some((frame.clone(), lines.clone()));
        Ok(lines)
    }

    fn lines(&mut self) -> Result<Vec<OcrLine>, DriverError> {
        let frame = self.screen.frame()?;
        self.frame_height = frame.height();
        self.read(&frame, 1)
    }

    /// One reading per distinct frame across a caret blink. A text caret
    /// blinking beside freshly typed text merges with its last characters:
    /// `Quokka77` beside the caret was read as `Quokka/` (plan 15). Two
    /// captures half a blink apart differ only when something blinks, and
    /// then both are read, so a check sees the caret-free reading too. An
    /// unchanging screen costs one OCR pass, as before.
    fn steady_lines(&mut self) -> Result<Vec<Vec<OcrLine>>, DriverError> {
        let first = self.screen.frame()?;
        let gap = self.screen.blink_gap();
        if !gap.is_zero() {
            std::thread::sleep(gap);
        }
        let second = self.screen.frame()?;
        self.frame_height = second.height();
        let mut readings = vec![self.read(&second, 1)?];
        if first != second {
            readings.push(self.read(&first, 1)?);
        }
        Ok(readings)
    }

    /// [`Self::require`] plus the action point, kept inside the frame. A
    /// search stops as soon as the TEXT is on screen, and a field below its
    /// label can then sit past the bottom edge: kitchensink's Coupon Code
    /// label came to rest on the last line, its box out of view, and the
    /// click went nowhere (plan 15). An off-frame point scrolls a little
    /// and looks again.
    fn locate(
        &mut self,
        selector: &UiaSelector,
        default_relation: &str,
    ) -> Result<(OcrHit, (i32, i32)), DriverError> {
        self.screen.settle()?;
        let mut hit = self.require(selector)?;
        let mut point = Self::action_point(&hit, selector, default_relation);
        for _ in 0..2 {
            // A control's box is the tree's, already inside the window.
            if hit.control {
                break;
            }
            let line = hit.rect.3 as i32;
            // Two lines clear of the bottom edge, not one: live, a Coupon Code
            // box just inside the edge took its value but showed it half cut
            // off, so OCR could not read it back.
            let notches = if point.1 > self.frame_height as i32 - 2 * line {
                -EDGE_NUDGE_NOTCHES
            } else if point.1 < line / 2 {
                EDGE_NUDGE_NOTCHES
            } else {
                break;
            };
            vision_log(|| {
                format!(
                    "nudge {notches} notches: point {point:?} is near the edge of a {}px frame",
                    self.frame_height
                )
            });
            self.screen.scroll(notches)?;
            hit = self.require(selector)?;
            point = Self::action_point(&hit, selector, default_relation);
        }
        if let Some(field) = self.drawn_field(&hit, selector, default_relation)? {
            vision_log(|| format!("[{selector}] field drawn at {field:?} - acting inside it"));
            point = (field.0 + field.2 as i32 / 2, field.1 + field.3 as i32 / 2);
        }
        vision_log(|| {
            format!(
                "[{selector}] matched '{}' at {:?}; acting {} at {point:?}",
                hit.text,
                hit.rect,
                selector.relation.as_deref().unwrap_or(default_relation)
            )
        });
        Ok((hit, point))
    }

    /// The input box a label's field is drawn in, when the action lands
    /// beside or below a label: the offset from the label is a guess about
    /// the layout, and a box drawn there is not. A form with a wide gap
    /// between label and field, or a tall multi-line field, put the guessed
    /// point in the gap. `None` - and the guess stands - when the hit is a
    /// control already, the relation is `inside`, or nothing is drawn there.
    fn drawn_field(
        &mut self,
        hit: &OcrHit,
        selector: &UiaSelector,
        default_relation: &str,
    ) -> Result<Option<PixelRect>, DriverError> {
        let relation = selector.relation.as_deref().unwrap_or(default_relation);
        if hit.control || !matches!(relation, "right_of" | "below") {
            return Ok(None);
        }
        let frame = self.screen.frame()?;
        let text: Vec<PixelRect> = self.read(&frame, 1)?.iter().map(|l| l.rect).collect();
        let elements = crate::vision_elements::detect(&frame, &text);
        Ok(crate::vision_elements::field_box(
            &elements, hit.rect, relation,
        ))
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
        Ok(self.look(selector)?.1)
    }

    /// One frame's reading and the target in it - read again, closer and
    /// then closer still, when a reading misses it (see
    /// [`OcrEngine::recognize_closer`]). The lines returned are the first
    /// reading's: they describe the screen, and scrolling compares them to
    /// tell whether the view moved.
    ///
    /// An anchor of a character or two is a key, and a key stands alone: it
    /// is looked for as a whole line at every scale before it may match a
    /// character inside a phrase. OCR missed Calculator's `5` key at screen
    /// size and read the Keep-on-top icon beside "Standard" as `5`; the
    /// click put Calculator in compact mode. A dense keypad whose row reads
    /// as one line still resolves by word, after.
    ///
    /// Between the screen-size reading and the closer ones, the window's
    /// accessibility tree is asked (see [`VisionScreen::accessible_controls`]):
    /// a closer look is where OCR misreads - an icon as `5` - and a name the
    /// tree reports is not a guess.
    fn look(
        &mut self,
        selector: &UiaSelector,
    ) -> Result<(Vec<OcrLine>, Option<OcrHit>), DriverError> {
        let frame = self.screen.frame()?;
        self.frame_height = frame.height();
        let lines = self.read(&frame, 1)?;
        // The Windows Start logo has no text to read: it is found by its
        // shape on every look, and never by OCR or the tree.
        if selector.name.as_deref().map(str::trim) == Some(start::LABEL) {
            if selector.nth.unwrap_or(1) != 1 {
                return Ok((lines, None));
            }
            let hit = start::locate(&frame).map(|rect| OcrHit {
                text: start::LABEL.into(),
                rect,
                order: (rect.1, rect.0),
                control: true,
            });
            return Ok((lines, hit));
        }
        let short = selector
            .name
            .as_deref()
            .is_some_and(|name| name.trim().chars().count() <= 2);
        // An icon named by its pixels (see `vision_elements::icon_hash`): the
        // nearest-looking icon on screen, within a tolerance.
        if let Some(hash) = selector
            .name
            .as_deref()
            .and_then(|n| n.trim().strip_prefix(ICON_PREFIX))
        {
            let text: Vec<PixelRect> = lines.iter().map(|l| l.rect).collect();
            let hit = Self::icons(&frame, &text)
                .into_iter()
                .filter_map(|(name, rect)| {
                    crate::vision_elements::hash_distance(&name[ICON_PREFIX.len()..], hash)
                        .filter(|d| *d <= ICON_TOLERANCE)
                        .map(|_| rect)
                })
                .nth(selector.nth.unwrap_or(1).max(1) as usize - 1)
                .map(|rect| OcrHit {
                    text: selector.name.clone().unwrap_or_default(),
                    rect,
                    order: (rect.1, rect.0),
                    control: true,
                });
            return Ok((lines, hit));
        }
        // A control the tree names EXACTLY is the strongest evidence there
        // is: text can repeat - a page heading reads "Windows Update" above
        // the nav item of that name, and reading order took the heading.
        let controls = self.controls_for(&frame)?;
        if let Some(hit) = Self::resolve_named(&controls, selector) {
            let (x, y) = hit.center();
            if self.screen.reaches(x, y)? {
                vision_log(|| {
                    format!(
                        "[{selector}] the accessibility tree names it: {:?}",
                        hit.rect
                    )
                });
                return Ok((lines, Some(hit)));
            }
            vision_log(|| format!("[{selector}] named control at {:?} is covered", hit.rect));
        }
        // An operator key drawn only as strokes, which no OCR reads - and
        // which OCR's look-alike folding would otherwise match to a speck:
        // `−` matched a 5px fragment above Calculator's display, and the
        // subtraction never happened.
        if let Some(symbol) = selector
            .name
            .as_deref()
            .and_then(crate::vision_elements::Symbol::named)
        {
            let text: Vec<PixelRect> = lines.iter().map(|l| l.rect).collect();
            let nth = selector.nth.unwrap_or(1).max(1) as usize;
            let hit = crate::vision_elements::find_symbols(&frame, &text)
                .into_iter()
                // Not the window's minimize button, a bar in the caption.
                .filter(|(s, (_, y, _, _))| *s == symbol && *y >= CAPTION_BAND)
                .nth(nth - 1)
                .map(|(_, rect)| OcrHit {
                    text: selector.name.clone().unwrap_or_default(),
                    rect,
                    order: (rect.1, rect.0),
                    control: false,
                });
            if let Some(hit) = hit {
                vision_log(|| format!("[{selector}] drawn as strokes at {:?}", hit.rect));
                return Ok((lines, Some(hit)));
            }
        }
        // (scale, whole lines only) per OCR look; `None` asks the tree.
        let [near, nearer] = CLOSER_SCALES;
        let attempts: &[Option<(u32, bool)>] = if short {
            &[
                Some((1, true)),
                None,
                Some((near, true)),
                Some((nearer, true)),
                Some((1, false)),
                Some((near, false)),
                Some((nearer, false)),
            ]
        } else {
            &[
                Some((1, false)),
                None,
                Some((near, false)),
                Some((nearer, false)),
            ]
        };
        for attempt in attempts {
            let Some((scale, whole_lines)) = *attempt else {
                if let Some(hit) = Self::resolve_accessible(&controls, selector) {
                    let (x, y) = hit.center();
                    if !self.screen.reaches(x, y)? {
                        continue;
                    }
                    vision_log(|| {
                        format!(
                            "[{selector}] not read by OCR; the accessibility tree names '{}'",
                            hit.text
                        )
                    });
                    return Ok((lines, Some(hit)));
                }
                continue;
            };
            let mut reading = self.read(&frame, scale)?;
            if whole_lines {
                reading.iter_mut().for_each(|line| line.words.clear());
            }
            if let Some(hit) = Self::resolve_in(&reading, selector) {
                if scale > 1 {
                    vision_log(|| format!("[{selector}] found only at {scale}x: '{}'", hit.text));
                }
                return Ok((lines, Some(hit)));
            }
        }
        Ok((lines, None))
    }

    /// Every icon on the frame - a small shape that is not text - named by
    /// its pixels, in reading order. Below the caption band: the window's
    /// own buttons are not a step's target.
    fn icons(frame: &RgbaImage, text: &[PixelRect]) -> Vec<(String, PixelRect)> {
        crate::vision_elements::detect(frame, text)
            .into_iter()
            .filter(|e| {
                e.kind == crate::vision_elements::ElementKind::Icon && e.rect.1 >= CAPTION_BAND
            })
            .map(|e| {
                (
                    format!(
                        "{ICON_PREFIX}{}",
                        crate::vision_elements::icon_hash(frame, e.rect)
                    ),
                    e.rect,
                )
            })
            .collect()
    }

    /// The accessible controls of the window as `frame` shows it, inside it
    /// and below its caption buttons.
    fn controls_for(&mut self, frame: &RgbaImage) -> Result<Vec<AccessibleControl>, DriverError> {
        if let Some((cached, controls)) = &self.controls_cache {
            if cached == frame {
                return Ok(controls.clone());
            }
        }
        let (w, h) = (frame.width() as i32, frame.height() as i32);
        let controls: Vec<AccessibleControl> = self
            .screen
            .accessible_controls()?
            .into_iter()
            .filter(|c| {
                let (x, y, cw, ch) = c.rect;
                let (cx, cy) = (x + cw as i32 / 2, y + ch as i32 / 2);
                !c.name.trim().is_empty() && cx >= 0 && cy >= 0 && cx < w && cy < h
            })
            .collect();
        self.controls_cache = Some((frame.clone(), controls.clone()));
        Ok(controls)
    }

    /// A control the anchor names: its accessible name exactly, ignoring
    /// case and punctuation at word edges; then by the name a key is SPOKEN
    /// by - Calculator names its `=` key `Equals` and its `7` `Seven`, and an
    /// author writes what the key shows; then a name the anchor begins on a
    /// word boundary (`Save` for `Save as`), for three characters or more.
    /// `nth` counts top-to-bottom, left-to-right, as for text.
    fn resolve_accessible(
        controls: &[AccessibleControl],
        selector: &UiaSelector,
    ) -> Option<OcrHit> {
        Self::resolve_controls(controls, selector, false)
    }

    /// The exact-name tier of [`Self::resolve_accessible`] alone.
    fn resolve_named(controls: &[AccessibleControl], selector: &UiaSelector) -> Option<OcrHit> {
        Self::resolve_controls(controls, selector, true)
    }

    fn resolve_controls(
        controls: &[AccessibleControl],
        selector: &UiaSelector,
        exact_only: bool,
    ) -> Option<OcrHit> {
        let needle = selector.name.as_deref().map(str::trim)?;
        let nth = selector.nth.unwrap_or(1).max(1) as usize;
        let norm = |text: &str| {
            text.split_whitespace()
                .map(|w| {
                    w.trim_matches(|c: char| c.is_ascii_punctuation() && !"+=-*/%.".contains(c))
                })
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        };
        let wanted = norm(needle);
        let spoken = spoken_key_name(needle);
        let exact = |name: &str| !wanted.is_empty() && name == wanted;
        let said = |name: &str| spoken.is_some_and(|s| name == s);
        let begun = |name: &str| {
            wanted.chars().count() >= 3
                && name.starts_with(&wanted)
                && name[wanted.len()..].starts_with(' ')
        };
        let tiers: [&dyn Fn(&str) -> bool; 3] = [&exact, &said, &begun];
        let tiers = if exact_only { &tiers[..1] } else { &tiers[..] };
        for matches in tiers {
            let mut hits: Vec<OcrHit> = controls
                .iter()
                .filter(|c| matches(&norm(&c.name)))
                .map(|c| OcrHit {
                    text: c.name.clone(),
                    rect: c.rect,
                    order: (c.rect.1, c.rect.0),
                    control: true,
                })
                .collect();
            hits.sort_by_key(|h| h.order);
            if let Some(hit) = hits.into_iter().nth(nth - 1) {
                return Some(hit);
            }
        }
        None
    }

    fn resolve_in(lines: &[OcrLine], selector: &UiaSelector) -> Option<OcrHit> {
        let needle = selector.name.as_deref().map(str::trim)?;
        if needle.is_empty() {
            return None;
        }
        let nth = selector.nth.unwrap_or(1).max(1) as usize;
        // A window's minimize, maximize and close buttons are glyphs OCR
        // reads as a character or two, and they are read first, top to
        // bottom: `0` matched Calculator's maximize box and maximised it,
        // `×` its close button and closed it. No step means the window's
        // chrome; words up there - the title - stay.
        let lines: Vec<OcrLine> = lines
            .iter()
            .filter(|l| l.rect.1 >= CAPTION_BAND || l.text.trim().chars().count() > 2)
            .cloned()
            .collect();
        let lines = lines.as_slice();
        // Strongest first, each a fallback for the one before: as written;
        // then case-insensitive - OCR's capitals wobble between runs, a field
        // recorded as `Fake@email.com` read back as `fake@email.com`; then
        // also ignoring punctuation at the edges of words - OCR drops a
        // label's trailing colon, so `Describe:` read as `Describe` and the
        // field could not be found at all (plan 15, finding 26), and labels
        // ending in a colon are the norm on SAP and business forms. A hit
        // keeps the screen's own spelling.
        let fold = |text: &str| text.to_lowercase();
        let loosen = |text: &str| {
            text.split_whitespace()
                .map(|word| word.trim_matches(|c: char| c.is_ascii_punctuation()))
                .filter(|word| !word.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        };
        // Last, glyphs OCR reads as one another, for whole lines and words
        // only: on Calculator's keypad `×` read as `X`, and its thin `1` as
        // `I`, and an author writes what the key shows. Not `O` as `0`: OCR
        // misread Calculator's `9` as `O`, and that fold would have clicked
        // the wrong key. Not `|` as `1`: a toolbar's separators stand alone.
        // Case kept: the italic `x` of `x²` beside the keypad is a letter,
        // and matched `×` when the fold lowercased.
        let look_alike = |text: &str| {
            text.chars()
                .map(|c| match c {
                    '×' | 'X' | '*' => 'X',
                    '−' | '–' | '—' => '-',
                    '÷' => '/',
                    'I' | 'l' => '1',
                    other => other,
                })
                .collect::<String>()
        };
        // Also ignoring spaces, for an anchor of three characters or more:
        // OCR breaks a name after its underscores, and File Explorer's
        // `ssis_pc_data` read as `ssis_ pc_ data`.
        let squeeze = |text: &str| {
            let squeezed: String = text.split_whitespace().collect();
            if squeezed.chars().count() < 3 {
                String::new()
            } else {
                squeezed.to_lowercase()
            }
        };
        // Every whole match before any prefix: with the prefix tried first,
        // that `ssis_ pc_ data` lost to the file `ssis_pc_data.dtproj`, and
        // the click opened the project instead of the folder.
        Self::match_tiers(lines, needle, nth, false)
            .or_else(|| Self::match_reshaped(lines, needle, nth, fold, false))
            .or_else(|| Self::match_reshaped(lines, needle, nth, loosen, false))
            .or_else(|| Self::match_reshaped(lines, needle, nth, squeeze, false))
            .or_else(|| Self::match_reshaped(lines, needle, nth, look_alike, false))
            .or_else(|| Self::match_tiers(lines, needle, nth, true))
            .or_else(|| Self::match_reshaped(lines, needle, nth, fold, true))
            .or_else(|| Self::match_reshaped(lines, needle, nth, loosen, true))
    }

    /// [`Self::match_tiers`] over lines and needle passed through `shape`,
    /// handing back the matched line's original text.
    fn match_reshaped(
        lines: &[OcrLine],
        needle: &str,
        nth: usize,
        shape: impl Fn(&str) -> String,
        prefix: bool,
    ) -> Option<OcrHit> {
        let shaped_needle = shape(needle);
        if shaped_needle.is_empty() {
            return None;
        }
        let shaped: Vec<OcrLine> = lines
            .iter()
            .map(|l| OcrLine {
                text: shape(&l.text),
                rect: l.rect,
                words: l
                    .words
                    .iter()
                    .map(|w| OcrWord {
                        text: shape(&w.text),
                        rect: w.rect,
                    })
                    .filter(|w| !w.text.is_empty())
                    .collect(),
            })
            .collect();
        let hit = Self::match_tiers(&shaped, &shaped_needle, nth, prefix)?;
        let text = lines
            .iter()
            .find(|l| l.rect == hit.rect)
            .map_or_else(|| hit.text.clone(), |l| l.text.clone());
        Some(OcrHit { text, ..hit })
    }

    /// `prefix: false` keeps to whole lines and words.
    fn match_tiers(lines: &[OcrLine], needle: &str, nth: usize, prefix: bool) -> Option<OcrHit> {
        let all = [Self::exact_lines, Self::exact_words, Self::prefix_lines];
        let tiers = if prefix { &all[..] } else { &all[..2] };
        for tier in tiers {
            let mut hits = tier(lines, needle);
            hits.sort_by_key(|h| h.order);
            if let Some(hit) = hits.into_iter().nth(nth - 1) {
                return Some(hit);
            }
        }
        None
    }

    /// [`Self::resolve`], but a target that is not on screen is looked for
    /// by scrolling: down a page at a time to the bottom, then up to the
    /// top. A web driver scrolls an element into view before acting on it;
    /// pixels have no element to scroll to, so the only way to find text
    /// below the fold is to bring it into the frame and read again. Without
    /// this, every field below the first screen of a long form was "not on
    /// the current screen" (plan 15, finding 7).
    ///
    /// Deterministic for replay: a replay starts from the same page state
    /// the recording did and runs the same search, so it stops at the same
    /// scroll position. An edge is detected by the OCR'd text not changing
    /// across a scroll, and [`MAX_SCROLL_PAGES`] bounds the search when OCR
    /// noise keeps two frames of one position from reading identically.
    ///
    /// Only actions search. `element_exists` stays a pure read of the
    /// current frame, because it is what auto-wait polls: a poll that
    /// scrolled would move the page on every tick.
    fn resolve_scrolling(&mut self, selector: &UiaSelector) -> Result<Option<OcrHit>, DriverError> {
        let (mut lines, hit) = self.look(selector)?;
        if hit.is_some() {
            return Ok(hit);
        }
        vision_log(|| {
            let read: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
            format!("[{selector}] not on screen - searching by scrolling. OCR read: {read:?}")
        });
        for notches in [-SCROLL_NOTCHES_PER_PAGE, SCROLL_NOTCHES_PER_PAGE] {
            for _ in 0..MAX_SCROLL_PAGES {
                self.screen.scroll(notches)?;
                let next = self.lines()?;
                if let Some(hit) = Self::resolve_in(&next, selector) {
                    return Ok(Some(hit));
                }
                let edge = Self::same_text(&next, &lines);
                lines = next;
                if edge {
                    break;
                }
            }
        }
        Ok(None)
    }

    /// Scroll one way until the view stops moving (or the page bound).
    fn scroll_to_edge(&mut self, notches: i32) -> Result<(), DriverError> {
        let mut lines = self.lines()?;
        for _ in 0..MAX_SCROLL_PAGES {
            self.screen.scroll(notches)?;
            let next = self.lines()?;
            if Self::same_text(&next, &lines) {
                return Ok(());
            }
            lines = next;
        }
        Ok(())
    }

    /// Where a checkbox or radio button is drawn: the square-ish region just
    /// LEFT of its label, three label-heights wide and two tall - where both
    /// a browser and SAP GUI put the box.
    fn box_region(label: &OcrHit) -> PixelRect {
        let (x, y, _, h) = label.rect;
        let h = h.max(1) as i32;
        let left = (x - 3 * h).max(0);
        (
            (left),
            (y - h / 2).max(0),
            (x - left).max(1) as u32,
            (2 * h) as u32,
        )
    }

    /// "Ink" in a region of the current frame: pixels dark enough or
    /// coloured enough to be a tick, a dot or a filled box rather than the
    /// white or grey of an empty one.
    fn ink(&mut self, (rx, ry, rw, rh): PixelRect) -> Result<u32, DriverError> {
        let frame = self.screen.frame()?;
        let mut ink = 0;
        for py in ry.max(0) as u32..((ry as u32 + rh).min(frame.height())) {
            for px in rx.max(0) as u32..((rx as u32 + rw).min(frame.width())) {
                let [r, g, b, _] = frame.get_pixel(px, py).0;
                let luma = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000;
                let chroma = r.max(g).max(b) - r.min(g).min(b);
                if luma < 140 || chroma > 80 {
                    ink += 1;
                }
            }
        }
        Ok(ink)
    }

    fn same_text(a: &[OcrLine], b: &[OcrLine]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.text == y.text)
    }

    fn exact_lines(lines: &[OcrLine], needle: &str) -> Vec<OcrHit> {
        lines
            .iter()
            .filter(|l| l.text.trim() == needle)
            .map(|l| OcrHit {
                text: l.text.clone(),
                rect: l.rect,
                order: (l.rect.1, l.rect.0),
                control: false,
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
                    control: false,
                });
            }
        }
        hits
    }

    /// Needs a needle of three characters or more: a shorter one is a key
    /// on a keypad, not the start of a label, and `1` matched Calculator's
    /// `1/x` button.
    fn prefix_lines(lines: &[OcrLine], needle: &str) -> Vec<OcrHit> {
        if needle.chars().count() < 3 {
            return Vec::new();
        }
        lines
            .iter()
            .filter(|l| l.text.trim().starts_with(needle))
            .map(|l| OcrHit {
                text: l.text.clone(),
                rect: l.rect,
                order: (l.rect.1, l.rect.0),
                control: false,
            })
            .collect()
    }

    /// Click, and check the screen answered. A computer-use model looks at
    /// the screen after every action and clicks again when nothing
    /// happened; replayed over pixels, a click a slow remote session
    /// swallowed passed silently, and the step after it failed for a reason
    /// that was not its own. So the frame before the click - pointer
    /// already resting on the target, its hover tint drawn - is compared
    /// with the frames after it for up to [`VisionScreen::click_wait`], and
    /// a click that changed nothing but a caret is made once more.
    ///
    /// Once more, not until something changes, and never failing: a click
    /// can rightly change nothing (`0` on a Calculator showing 0, a tab
    /// already open), and a second press on a toggle the app took but did
    /// not yet draw would undo it - so the wait is long before the one
    /// retry, and a click still answered by nothing is logged and the step
    /// goes on to be judged by what follows it.
    fn press(&mut self, selector: &UiaSelector, (x, y): (i32, i32)) -> Result<(), DriverError> {
        let wait = self.screen.click_wait();
        if wait.is_zero() {
            return self.screen.click(x, y);
        }
        self.screen.point_at(x, y)?;
        self.screen.settle()?;
        let before = self.screen.frame()?;
        for attempt in 0..2 {
            self.screen.click(x, y)?;
            if self.answered(&before, wait)? {
                return Ok(());
            }
            if attempt == 0 {
                vision_log(|| {
                    format!("[{selector}] nothing changed {wait:?} after the click - clicking once more")
                });
            }
        }
        vision_log(|| {
            format!("[{selector}] still nothing changed after a second click - going on: a click may rightly do nothing")
        });
        Ok(())
    }

    /// Whether a frame within `wait` differs from `before` by more than a
    /// caret.
    fn answered(&mut self, before: &RgbaImage, wait: Duration) -> Result<bool, DriverError> {
        let deadline = std::time::Instant::now() + wait;
        loop {
            if !only_caret_differs(before, &self.screen.frame()?) {
                return Ok(true);
            }
            if std::time::Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(CLICK_POLL.min(wait));
        }
    }

    fn require(&mut self, selector: &UiaSelector) -> Result<OcrHit, DriverError> {
        self.resolve_scrolling(selector)?.ok_or_else(|| {
            if selector.name.as_deref().map(str::trim) == Some(start::LABEL) {
                return DriverError::Uia(
                    "vision: Windows Start button unavailable: expected one visible bottom-taskbar logo"
                        .into(),
                );
            }
            // Calculator's `=` and `−` are never read at all: a glyph made
            // only of strokes is not text to an OCR detector. Saying so
            // beats leaving an author to hunt for a key that is on screen.
            let symbol_only = selector.name.as_deref().is_some_and(|name| {
                !name.trim().is_empty() && !name.chars().any(char::is_alphanumeric)
            });
            DriverError::Uia(format!(
                "vision: no OCR line or word matches selector [{selector}], \
                 after scrolling the window to the bottom and back to the top{}",
                if symbol_only {
                    ". OCR often cannot read a control that shows only a symbol; if the \
                     app takes keyboard input, a bare `Type <symbol>` presses that key instead"
                } else {
                    ""
                }
            ))
        })
    }

    /// Everything on the current frame an anchor could address - each OCR
    /// line and each word of a line with more than one - and the point a
    /// click on it would land, window-relative. What the grounding
    /// benchmark scores; it reads the screen and touches nothing.
    pub fn targets(&mut self) -> Result<Vec<VisionTarget>, DriverError> {
        let target = |text: &str, rect: PixelRect| {
            let (x, y, w, h) = rect;
            VisionTarget {
                text: text.trim().to_string(),
                rect,
                point: (x + w as i32 / 2, y + h as i32 / 2),
            }
        };
        let mut targets = Vec::new();
        let frame = self.screen.frame()?;
        self.frame_height = frame.height();
        for line in self.read(&frame, 1)? {
            if line.text.trim().is_empty() {
                continue;
            }
            targets.push(target(&line.text, line.rect));
            if line.words.len() > 1 {
                targets.extend(line.words.iter().map(|w| target(&w.text, w.rect)));
            }
        }
        for control in self.controls_for(&frame)? {
            targets.push(target(&control.name, control.rect));
        }
        let text: Vec<PixelRect> = targets.iter().map(|t| t.rect).collect();
        for (name, rect) in Self::icons(&frame, &text) {
            targets.push(target(&name, rect));
        }
        Ok(targets)
    }

    /// Where `Click the nth "<name>"` would land on the current frame, window-
    /// relative, without scrolling or clicking - `None` when nothing
    /// matches. The same resolution an action takes, minus the search.
    pub fn locate_point(
        &mut self,
        name: &str,
        nth: u32,
        relation: &str,
    ) -> Result<Option<(i32, i32)>, DriverError> {
        let selector = UiaSelector {
            name: Some(name.to_string()),
            nth: (nth > 1).then_some(nth),
            relation: Some(relation.to_string()),
            ..UiaSelector::default()
        };
        let Some(hit) = self.resolve(&selector)? else {
            return Ok(None);
        };
        let point = Self::action_point(&hit, &selector, relation);
        Ok(Some(match self.drawn_field(&hit, &selector, relation)? {
            Some((x, y, w, h)) => (x + w as i32 / 2, y + h as i32 / 2),
            None => point,
        }))
    }

    /// Screen origin of the attached window.
    pub fn window_origin(&mut self) -> Result<(i32, i32), DriverError> {
        self.screen.window_origin()
    }

    /// The action point for a matched anchor, honoring the selector's
    /// spatial relation (`default_relation` when the trace predates it).
    fn action_point(hit: &OcrHit, selector: &UiaSelector, default_relation: &str) -> (i32, i32) {
        if hit.control {
            return hit.center();
        }
        let relation = selector.relation.as_deref().unwrap_or(default_relation);
        let (x, y, w, h) = hit.rect;
        match relation {
            "right_of" => (
                x + w as i32 + (h * RIGHT_OF_GAP_FACTOR) as i32,
                y + h as i32 / 2,
            ),
            // The middle of where an input box sits under its label: one and
            // a half line-heights below the label's bottom edge, the height
            // floored at 14px. Live, OCR measured `Describe:` only 10px tall,
            // and "one height below" landed in the gap ABOVE its box (plan 15,
            // finding 26); earlier, half a height landed above kitchensink's
            // email box. A label's OCR height wobbles; an input box does not.
            "below" => {
                let line = (h as i32).max(14);
                (x + w as i32 / 2, y + h as i32 + line * 3 / 2)
            }
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
        self.screen.attach(window_name, timeout)?;
        self.window_title = Some(window_name.to_lowercase());
        Ok(())
    }

    fn element_exists(&mut self, selector: &UiaSelector) -> Result<bool, DriverError> {
        Ok(self.resolve(selector)?.is_some())
    }

    fn invoke(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let (_, (x, y)) = self.locate(selector, "inside")?;
        self.press(selector, (x, y))
    }

    fn read_text(&mut self, selector: &UiaSelector) -> Result<String, DriverError> {
        Ok(self.require(selector)?.text)
    }

    fn type_text(&mut self, selector: &UiaSelector, text: &str) -> Result<(), DriverError> {
        // A field is addressed by its label; the input box sits beside it.
        let (_, (x, y)) = self.locate(selector, "right_of")?;
        self.screen.click(x, y)?;
        // Select what the field holds before the keystrokes land - the
        // trait's fill contract, by the same chord clear_text below already
        // uses. Pixels give no way to read the field first, which is exactly
        // why the recipe must not depend on knowing: select-all over an
        // empty field is a no-op, over a filled one it is the replace.
        self.screen.press_key("a", &[KeyMod::Ctrl])?;
        // Typing NOTHING over a selection replaces nothing: the old value
        // stayed selected and on screen. A clear is recorded as typing ""
        // with replace, so this is the path every vision clear takes
        // (plan 15) - delete the selection instead.
        if text.is_empty() {
            self.typed_baseline = None;
            return self.screen.press_key("Delete", &[]);
        }
        // Empty the field, then count the value's read-back key on screen:
        // counted after the delete, a field that already held the value does
        // not inflate the baseline, so re-typing it still reads as arrived.
        self.screen.press_key("Delete", &[])?;
        self.count_before_typing(text)?;
        self.screen.type_text(text)
    }

    /// Plan 15, finding 21: with the field's label ABOVE the box, the click
    /// landed beside the label, select-all took the whole page, and the
    /// value was typed into nothing - and the flow replayed green. Arrived
    /// means the value's key is on screen MORE often than before typing,
    /// so a page that already prints the value elsewhere (a code sample, a
    /// summary panel) cannot vouch for it.
    fn typed_value_arrived(&mut self, text: &str) -> Result<Option<bool>, DriverError> {
        let Some((keys, before)) = self.typed_baseline.clone() else {
            return Ok(None);
        };
        if Self::readback_keys(text) != keys {
            return Ok(None);
        }
        let now = self.readback_counts(&keys)?;
        Ok(Some(
            now.iter().zip(&before).any(|(now, before)| now > before),
        ))
    }

    fn clear_text(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let (_, (x, y)) = self.locate(selector, "right_of")?;
        self.screen.click(x, y)?;
        self.screen.press_key("a", &[KeyMod::Ctrl])?;
        self.screen.press_key("Delete", &[])
    }

    /// Read back too: a bare `Type X` after a double-click that did NOT open
    /// its input typed into nothing and passed (plan 15). Typing appends at
    /// the caret, so arrival is the same "more often than before" test.
    fn type_focused(&mut self, text: &str) -> Result<(), DriverError> {
        self.count_before_typing(text)?;
        self.screen.type_text(text)
    }

    fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError> {
        // A symbol has no key NAME - `*` is Shift+8 on one layout and its
        // own key on another - so it goes out as the keystroke that types
        // it. A model asked to "Press times" on Calculator answered `*`, and
        // the step failed for want of a virtual key.
        let mut chars = key.chars();
        if let (Some(c), None, true) = (chars.next(), chars.next(), modifiers.is_empty()) {
            if !c.is_alphanumeric() && !c.is_whitespace() {
                return self.screen.type_text(key);
            }
        }
        self.screen.press_key(key, modifiers)
    }

    fn reveal(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        self.resolve_scrolling(selector).map(|_| ())
    }

    fn double_click(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let (_, (x, y)) = self.locate(selector, "inside")?;
        self.screen.double_click(x, y)
    }

    /// A point inside the matched text's box. OCR reads a Calculator keypad
    /// row as one line - `7 8 9 ×` - and a model shown that line reaches the
    /// `8` as a click a third of the way along it. Refused, the simplest
    /// calculator flow failed on its first step.
    fn click_at(
        &mut self,
        selector: &UiaSelector,
        x_pct: f64,
        y_pct: f64,
    ) -> Result<(), DriverError> {
        let (hit, _) = self.locate(selector, "inside")?;
        let (x, y, w, h) = hit.rect;
        let at = |origin: i32, extent: u32, pct: f64| {
            origin + (f64::from(extent) * pct.clamp(0.0, 100.0) / 100.0).round() as i32
        };
        let point = (at(x, w, x_pct), at(y, h, y_pct));
        vision_log(|| {
            format!(
                "[{selector}] clicking {x_pct}%,{y_pct}% of {:?} at {point:?}",
                hit.rect
            )
        });
        self.press(selector, point)
    }

    /// SET-state from pixels (plan 15, finding 15). Pixels give no checkbox
    /// state to read, but they do show which way a click flipped it: a tick,
    /// dot or fill appearing beside the label is more ink, its disappearing
    /// less. Click, look; if that is the wrong way, click again and confirm
    /// it came back. A click that changes nothing visible is refused rather
    /// than assumed - a disabled box, or a radio button already selected,
    /// cannot be told apart from a miss.
    fn set_checked(&mut self, selector: &UiaSelector, checked: bool) -> Result<(), DriverError> {
        let (hit, (x, y)) = self.locate(selector, "inside")?;
        let region = Self::box_region(&hit);
        let threshold = (region.2 * region.3 / 50).max(4);
        let before = self.ink(region)?;
        self.screen.click(x, y)?;
        let after = self.ink(region)?;
        if before.abs_diff(after) < threshold {
            return Err(DriverError::Uia(format!(
                "vision: clicking [{selector}] changed nothing visible beside it, so whether it \
                 is now {} cannot be told - a disabled box, a radio button already selected, or \
                 a click that missed",
                if checked { "checked" } else { "unchecked" }
            )));
        }
        if (after > before) == checked {
            return Ok(());
        }
        // It already held the wanted state, and the click flipped it away.
        self.screen.click(x, y)?;
        let restored = self.ink(region)?;
        if restored.abs_diff(before) < threshold {
            Ok(())
        } else {
            Err(DriverError::Uia(format!(
                "vision: [{selector}] did not return to {} on a second click",
                if checked { "checked" } else { "unchecked" }
            )))
        }
    }

    fn context_click(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let (_, (x, y)) = self.locate(selector, "inside")?;
        self.screen.right_click(x, y)
    }

    /// The pointer onto the matched text, nothing pressed - a tooltip, a
    /// menu that opens on hover. Computer-use agents have `mouse_move` for
    /// exactly this; vision refused it.
    fn hover(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let (_, (x, y)) = self.locate(selector, "inside")?;
        self.screen.move_to(x, y)
    }

    /// Press on one anchor, release on the other: a file onto a folder, a
    /// card onto a column, a slider's thumb along its track. Both are found
    /// before the button goes down, so a target that is not there fails
    /// the step without leaving anything half-dragged.
    fn drag(&mut self, from: &UiaSelector, to: &UiaSelector) -> Result<(), DriverError> {
        let (_, start) = self.locate(from, "inside")?;
        let (_, end) = self.locate(to, "inside")?;
        vision_log(|| format!("dragging from {start:?} to {end:?}"));
        self.screen.drag(start, end)
    }

    fn scroll(&mut self, selector: Option<&UiaSelector>, to: ScrollTo) -> Result<(), DriverError> {
        match (selector, to) {
            (Some(selector), ScrollTo::IntoView) => self.require(selector).map(|_| ()),
            (None, ScrollTo::Top) => self.scroll_to_edge(SCROLL_NOTCHES_PER_PAGE),
            (None, ScrollTo::Bottom) => self.scroll_to_edge(-SCROLL_NOTCHES_PER_PAGE),
            (_, to) => Err(DriverError::Uia(format!(
                "vision: cannot scroll {to:?}{}: pixels give no scroll container or offset, \
                 only the window's wheel - scroll the page to top/bottom or a text into view",
                if selector.is_some() {
                    " inside an element"
                } else {
                    ""
                }
            ))),
        }
    }

    /// OCR'd text, so text assertions take the punctuation-forgiving rung
    /// (`flowproof_driver::ocr_text_contains`).
    fn reads_text_by_ocr(&mut self) -> Result<bool, DriverError> {
        Ok(true)
    }

    fn surface_text(&mut self) -> Result<String, DriverError> {
        // The OCR'd frame IS the surface - reading order, one line each.
        // Across a caret blink both readings are kept, so a check is judged
        // against the one the caret did not garble.
        Ok(self
            .steady_lines()?
            .into_iter()
            .flatten()
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
        let frame = self.screen.frame()?;
        self.frame_height = frame.height();
        let lines = self.read(&frame, 1)?;
        // A text shown more than once is offered once per place, each with
        // its ordinal - `nth:2:text:Home` - counted the way resolution counts
        // (top to bottom, then left to right, the caption's glyphs aside).
        // A computer-use model tells two "Home"s apart by where they are;
        // offered alike as `text:Home`, every one it picked replayed as the
        // first.
        let reachable = |l: &OcrLine| l.rect.1 >= CAPTION_BAND || l.text.trim().chars().count() > 2;
        if let Some(rect) = start::locate(&frame) {
            entries.push(serde_json::json!({
                "target": format!("text:{}", start::LABEL),
                "label": start::LABEL, "tag": "button", "actionable": true,
                "source": "visual_icon", "rect": [rect.0, rect.1, rect.2, rect.3],
            }));
        }
        for line in &lines {
            if entries.len() >= 100 {
                break;
            }
            let text = line.text.trim();
            if text.is_empty() || text == start::LABEL {
                continue;
            }
            let nth = 1 + lines
                .iter()
                .filter(|m| {
                    reachable(m)
                        && m.text.trim() == text
                        && (m.rect.1, m.rect.0) < (line.rect.1, line.rect.0)
                })
                .count();
            let target = if nth > 1 && reachable(line) {
                format!("nth:{nth}:text:{text}")
            } else {
                format!("text:{text}")
            };
            // The window's own title, read in a Citrix client's caption, is
            // not a button: a model anchored a Start click to it.
            let actionable = !(line.rect.1 < 64
                && self
                    .window_title
                    .as_ref()
                    .is_some_and(|title| !title.is_empty() && text.to_lowercase().contains(title)));
            entries.push(serde_json::json!({
                "target": target,
                "text": text,
                "rect": [line.rect.0, line.rect.1, line.rect.2, line.rect.3],
                "actionable": actionable,
            }));
        }
        // What OCR did not read but the accessibility tree names - an icon
        // button, a key drawn as a symbol - offered by that name, which is
        // what resolution falls back to. A control whose name OCR already
        // read inside it is listed once, as the text.
        let read_inside = |c: &AccessibleControl| {
            let name = c.name.trim().to_lowercase();
            lines.iter().any(|l| {
                let (x, y, w, h) = l.rect;
                let (cx, cy, cw, ch) = c.rect;
                let (mx, my) = (x + w as i32 / 2, y + h as i32 / 2);
                l.text.trim().to_lowercase() == name
                    && mx >= cx
                    && my >= cy
                    && mx < cx + cw as i32
                    && my < cy + ch as i32
            })
        };
        for control in self.controls_for(&frame)? {
            if entries.len() >= 150 {
                break;
            }
            if read_inside(&control) {
                continue;
            }
            let (x, y, w, h) = control.rect;
            entries.push(serde_json::json!({
                "target": format!("text:{}", control.name.trim()),
                "text": control.name.trim(),
                "rect": [x, y, w, h],
                "control_type": control.control_type,
                "source": "accessibility",
            }));
        }
        // Icons, named by their pixels - which icon is which is for the
        // model to see in the screenshot. Not one the tree already named: a
        // name a person can read beats a hash.
        let text: Vec<PixelRect> = lines.iter().map(|l| l.rect).collect();
        let named: Vec<PixelRect> = self.controls_for(&frame)?.iter().map(|c| c.rect).collect();
        let icons = Self::icons(&frame, &text);
        for (i, (name, rect)) in icons.iter().enumerate() {
            if entries.len() >= 200 {
                break;
            }
            // Two icons alike - a toolbar's identical buttons - are told apart
            // by ordinal, as resolution counts: the alike icons before it.
            let hash = &name[ICON_PREFIX.len()..];
            let nth = 1 + icons[..i]
                .iter()
                .filter(|(other, _)| {
                    crate::vision_elements::hash_distance(&other[ICON_PREFIX.len()..], hash)
                        .is_some_and(|d| d <= ICON_TOLERANCE)
                })
                .count();
            let target = if nth > 1 {
                format!("nth:{nth}:text:{name}")
            } else {
                format!("text:{name}")
            };
            let (x, y, w, h) = *rect;
            let (cx, cy) = (x + w as i32 / 2, y + h as i32 / 2);
            let inside_named = named.iter().any(|&(nx, ny, nw, nh)| {
                cx >= nx && cy >= ny && cx < nx + nw as i32 && cy < ny + nh as i32
            });
            if inside_named {
                continue;
            }
            entries.push(serde_json::json!({
                "target": target,
                "rect": [x, y, w, h],
                "kind": "icon",
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

/// The axis-aligned box around a detected word's corners.
fn corner_bounds(points: impl Iterator<Item = (f32, f32)>) -> PixelRect {
    let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (x, y) in points {
        left = left.min(x);
        right = right.max(x);
        top = top.min(y);
        bottom = bottom.max(y);
    }
    to_rect(
        left.round() as i32,
        top.round() as i32,
        right.round() as i32,
        bottom.round() as i32,
    )
}

/// The closer looks a missed target gets, in order: the frame at twice its
/// size, then three times. Calculator's `1` was found at 2x in one framing
/// and only at 3x - read as `I` - in another a few pixels narrower.
const CLOSER_SCALES: [u32; 2] = [2, 3];

/// An anchor naming an icon by its pixels: `icon:` and the icon's hash.
const ICON_PREFIX: &str = "icon:";

/// Bits of an icon hash that may differ and still be the same icon: room
/// for anti-aliasing and a hover tint, not for a different glyph.
const ICON_TOLERANCE: u32 = 10;

/// Two captures of one window that differ in no more than a blinking
/// caret: a 2px-wide, line-tall strip, or a few hundred pixels of a large
/// window. A screen that holds still apart from its caret has settled; one
/// whose caret alone kept frames from matching never would.
#[cfg_attr(not(windows), allow(dead_code))]
fn nearly_same(a: &RgbaImage, b: &RgbaImage) -> bool {
    if a.dimensions() != b.dimensions() {
        return false;
    }
    let allowed = (a.width() as usize * a.height() as usize / 4000).max(64);
    a.pixels()
        .zip(b.pixels())
        .filter(|(p, q)| p != q)
        .take(allowed + 1)
        .count()
        <= allowed
}

/// Two frames the same but for a text caret: nothing differs, or all that
/// differs fits in a strip a caret's width. Stricter than [`nearly_same`],
/// which forgives a few hundred pixels anywhere: a checkbox's tick is fewer
/// than that, and a tick taken for "nothing happened" would be clicked
/// off again.
fn only_caret_differs(a: &RgbaImage, b: &RgbaImage) -> bool {
    if a.dimensions() != b.dimensions() {
        return false;
    }
    let (mut left, mut right, mut top, mut bottom) = (u32::MAX, 0, u32::MAX, 0);
    for ((x, y, p), q) in a.enumerate_pixels().zip(b.pixels()) {
        if p != q {
            left = left.min(x);
            right = right.max(x);
            top = top.min(y);
            bottom = bottom.max(y);
        }
    }
    left == u32::MAX || (right - left < CARET_WIDTH && bottom - top < CARET_HEIGHT)
}

/// A text caret at 150% scaling, with a pixel of anti-aliasing each side.
const CARET_WIDTH: u32 = 4;

/// A caret is a line tall: a large font at 150%, with room.
const CARET_HEIGHT: u32 = 64;

/// How often a click's effect is looked for.
const CLICK_POLL: Duration = Duration::from_millis(100);

/// The name a key is spoken by in an accessibility tree, for an anchor
/// that is the glyph on the key - what Windows Calculator, and screen
/// readers generally, call them.
fn spoken_key_name(glyph: &str) -> Option<&'static str> {
    Some(match glyph.trim() {
        "0" => "zero",
        "1" => "one",
        "2" => "two",
        "3" => "three",
        "4" => "four",
        "5" => "five",
        "6" => "six",
        "7" => "seven",
        "8" => "eight",
        "9" => "nine",
        "+" => "plus",
        "-" | "\u{2212}" | "\u{2013}" => "minus",
        "*" | "x" | "X" | "\u{d7}" => "multiply by",
        "/" | "\u{f7}" => "divide by",
        "=" => "equals",
        "." | "," => "decimal separator",
        "%" => "percent",
        "\u{b1}" | "+/-" => "positive negative",
        "C" => "clear",
        "CE" => "clear entry",
        "\u{232b}" => "backspace",
        _ => return None,
    })
}

/// The top of a frame where a window draws its caption buttons, in
/// physical pixels: 32px tall at 100% scaling, 48px at 150%. Windows
/// reports no button rectangles for a UWP frame (Calculator's come back
/// empty), so the band is fixed rather than asked for.
const CAPTION_BAND: i32 = 48;

/// A gap wider than this many line heights ends a line. Words in running
/// text sit a fraction of a height apart; separate controls on one row - a
/// keypad's buttons, a toolbar, a grid's columns - sit several apart.
const LINE_BREAK_GAPS: f32 = 2.0;

/// Cut a detected line, as runs of word indices, wherever two neighbours
/// stand further apart than any words of one phrase. ocrs groups every
/// word on a row into one line, so a Calculator keypad row became the
/// single line `7 8 9 ×`, recognised as `8 9X` - the `7` lost across the
/// gaps and the `8` reachable only as part of a phrase. Recognised apart,
/// each button is a line of its own, the way a person reads it.
pub(crate) fn split_at_gaps(boxes: &[PixelRect]) -> Vec<std::ops::Range<usize>> {
    let mut runs = Vec::new();
    let mut start = 0;
    for i in 1..boxes.len() {
        let (px, _, pw, ph) = boxes[i - 1];
        let (x, _, _, h) = boxes[i];
        let gap = (x - (px + pw as i32)) as f32;
        if gap > LINE_BREAK_GAPS * ph.max(h).max(1) as f32 {
            runs.push(start..i);
            start = i;
        }
    }
    if !boxes.is_empty() {
        runs.push(start..boxes.len());
    }
    runs
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
        self.recognize_raw(frame, false)
    }

    /// The frame read at `scale` times its size, each lone glyph with a
    /// margin. A thin glyph alone on a button - Calculator's `1` - is too
    /// slight for the text detector at screen size; enlarged and given the
    /// margin a line of text would have, it is found. Reading every frame
    /// this way instead loses ordinary text, and costs far more time, so
    /// these are second looks, not the first.
    fn recognize_closer(
        &mut self,
        frame: &RgbaImage,
        scale: u32,
    ) -> Result<Vec<OcrLine>, DriverError> {
        let scale = scale.max(1);
        let enlarged = image::imageops::resize(
            frame,
            frame.width() * scale,
            frame.height() * scale,
            image::imageops::FilterType::CatmullRom,
        );
        let s = scale as i32;
        let shrink = |(x, y, w, h): PixelRect| (x / s, y / s, w / scale, h / scale);
        let mut lines = self.recognize_raw(&enlarged, true)?;
        for line in &mut lines {
            line.rect = shrink(line.rect);
            for word in &mut line.words {
                word.rect = shrink(word.rect);
            }
        }
        Ok(lines)
    }
}

impl OcrsEngine {
    fn recognize_raw(&mut self, frame: &RgbaImage, pad: bool) -> Result<Vec<OcrLine>, DriverError> {
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
        let line_rects: Vec<_> = self
            .engine
            .find_text_lines(&input, &words)
            .into_iter()
            .flat_map(|line| {
                let boxes: Vec<PixelRect> = line
                    .iter()
                    .map(|w| corner_bounds(w.corners().iter().map(|p| (p.x, p.y))))
                    .collect();
                split_at_gaps(&boxes)
                    .into_iter()
                    .map(|run| match &line[run.clone()] {
                        // A lone glyph cut out tight to its box touches the
                        // edges of the crop, and the recogniser, trained on
                        // lines of text with margins, misreads it. Half a
                        // height of margin each side, on the closer look.
                        [word] if pad => vec![word.expanded(word.height(), word.height() * 0.3)],
                        words => words.to_vec(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
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
                    .map(|w| corner_bounds(w.corners().iter().map(|p| (p.x, p.y))))
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

    use super::{vision_log, AccessibleControl, VisionScreen};

    // Key mapping lives in the driver crate (`virtual_key`,
    // `modifier_virtual_key`) — one table for every SendInput-based
    // driver, so `Press Escape` means the same keystroke on each.
    use flowproof_driver::{modifier_virtual_key as modifier_key, virtual_key};

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

        /// The attached window, raised again first if another process has
        /// taken the foreground since the last look. Capture copies the
        /// screen and input goes to whatever is in front, so a window
        /// covered mid-run - by a notification, or by the terminal the
        /// run was started from - would otherwise be read and clicked as
        /// the window on top.
        fn front(&mut self) -> Result<&mut WindowInfo, DriverError> {
            let window = self.window()?;
            if !window.holds_foreground() {
                vision_log(|| {
                    format!(
                        "'{}' lost the foreground to '{}' - raising it",
                        window.title,
                        flowproof_driver::window::foreground_title()
                    )
                });
                window.focus()?;
                Self::settle_window(window)?;
            }
            self.window()
        }

        /// Wait for a just-raised window to finish painting over what
        /// covered it. Being the foreground window is not being on screen:
        /// Windows animates the switch, and the first capture of a Calculator
        /// run, taken 200ms after it was raised, still showed the window
        /// in front of it. Two identical captures 100ms apart mean the
        /// switch is over; bounded, because a window with an animation of
        /// its own never holds still.
        fn settle_window(window: &mut WindowInfo) -> Result<(), DriverError> {
            let grab = |window: &mut WindowInfo| -> Result<RgbaImage, DriverError> {
                let (x, y, w, h) = window.refresh_rect()?;
                flowproof_driver::gdi::capture_region(x, y, w as i32, h as i32)
            };
            let deadline = Instant::now() + Duration::from_millis(1500);
            let mut last = grab(window)?;
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
                let next = grab(window)?;
                if super::nearly_same(&next, &last) {
                    return Ok(());
                }
                last = next;
            }
            Ok(())
        }

        fn inject(&mut self, event: InputEvent) -> Result<(), DriverError> {
            self.backend.inject(&event)
        }

        /// Move onto the point, then `presses` down/up pairs of `button`.
        /// The pairs are 40ms apart - well inside the system double-click
        /// interval (500ms by default) even with a remote display's input
        /// latency in between, since the client forwards them in order.
        fn press_at(
            &mut self,
            x: i32,
            y: i32,
            button: MouseButton,
            presses: u32,
        ) -> Result<(), DriverError> {
            let (wx, wy, _, _) = self.front()?.refresh_rect()?;
            self.inject(InputEvent::MouseMove {
                x: wx + x,
                y: wy + y,
            })?;
            std::thread::sleep(Duration::from_millis(60));
            for _ in 0..presses {
                self.inject(InputEvent::MouseDown { button })?;
                std::thread::sleep(Duration::from_millis(20));
                self.inject(InputEvent::MouseUp { button })?;
                std::thread::sleep(Duration::from_millis(40));
            }
            std::thread::sleep(Duration::from_millis(120));
            Ok(())
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
                if let Some(mut info) = find_window(window)? {
                    info.focus()?;
                    Self::settle_window(&mut info)?;
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
            // The window's own rectangle, wherever it is on the desktop:
            // the same origin `click` adds, so OCR coordinates and clicks
            // agree on every monitor (plan 15, finding 3).
            let (x, y, w, h) = self.front()?.refresh_rect()?;
            flowproof_driver::gdi::capture_region(x, y, w as i32, h as i32)
        }

        fn click(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
            let (wx, wy, _, _) = self.front()?.refresh_rect()?;
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

        fn double_click(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
            self.press_at(x, y, MouseButton::Left, 2)
        }

        fn right_click(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
            self.press_at(x, y, MouseButton::Right, 1)
        }

        fn type_text(&mut self, text: &str) -> Result<(), DriverError> {
            // As key strokes, not Unicode packets: the window is usually a
            // Citrix or RDP client, and those forward scan codes only.
            self.front()?;
            self.inject(InputEvent::KeyText {
                text: text.to_string(),
            })?;
            std::thread::sleep(Duration::from_millis(120));
            Ok(())
        }

        fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError> {
            let vk = virtual_key(key)
                .ok_or_else(|| DriverError::Uia(format!("vision: no virtual key for '{key}'")))?;
            self.front()?;
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

        fn settle(&mut self) -> Result<(), DriverError> {
            Self::settle_window(self.front()?)
        }

        fn move_to(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
            let (wx, wy, _, _) = self.front()?.refresh_rect()?;
            self.inject(InputEvent::MouseMove {
                x: wx + x,
                y: wy + y,
            })?;
            // Long enough for a tooltip or a hover menu to open.
            std::thread::sleep(Duration::from_millis(600));
            Ok(())
        }

        /// A drag an app recognises as one: Windows starts a drag only once
        /// the pointer has moved a few pixels with the button down
        /// (SM_CXDRAG), and drop targets track the pointer as it moves, so
        /// the path is walked in steps rather than jumped.
        fn drag(&mut self, from: (i32, i32), to: (i32, i32)) -> Result<(), DriverError> {
            let (wx, wy, _, _) = self.front()?.refresh_rect()?;
            let at = |(x, y): (i32, i32)| InputEvent::MouseMove {
                x: wx + x,
                y: wy + y,
            };
            self.inject(at(from))?;
            std::thread::sleep(Duration::from_millis(80));
            self.inject(InputEvent::MouseDown {
                button: MouseButton::Left,
            })?;
            std::thread::sleep(Duration::from_millis(120));
            const STEPS: i32 = 16;
            for i in 1..=STEPS {
                let point = (
                    from.0 + (to.0 - from.0) * i / STEPS,
                    from.1 + (to.1 - from.1) * i / STEPS,
                );
                self.inject(at(point))?;
                std::thread::sleep(Duration::from_millis(25));
            }
            // Let the target see the pointer settle over it before the drop.
            std::thread::sleep(Duration::from_millis(150));
            self.inject(InputEvent::MouseUp {
                button: MouseButton::Left,
            })?;
            std::thread::sleep(Duration::from_millis(200));
            Ok(())
        }

        fn scroll(&mut self, notches: i32) -> Result<(), DriverError> {
            // The wheel goes to whatever is under the cursor, so park it
            // mid-window first: over the content, not the title bar.
            let (wx, wy, w, h) = self.front()?.refresh_rect()?;
            self.inject(InputEvent::MouseMove {
                x: wx + w as i32 / 2,
                y: wy + h as i32 / 2,
            })?;
            std::thread::sleep(Duration::from_millis(60));
            self.inject(InputEvent::Wheel { notches })?;
            // Smooth scrolling animates for ~150ms in a browser, and a
            // remote display adds its own repaint latency on top.
            std::thread::sleep(Duration::from_millis(400));
            Ok(())
        }

        fn reaches(&mut self, x: i32, y: i32) -> Result<bool, DriverError> {
            let window = self.front()?;
            let (wx, wy, _, _) = window.refresh_rect()?;
            Ok(flowproof_driver::window::root_window_at(wx + x, wy + y) == window.hwnd())
        }

        /// `FLOWPROOF_VISION_UIA=0` keeps vision to pixels alone, as it is
        /// over a remote session anyway - to measure what the tree adds, or
        /// to prove a flow does not lean on it.
        fn accessible_controls(&mut self) -> Result<Vec<AccessibleControl>, DriverError> {
            if std::env::var("FLOWPROOF_VISION_UIA").is_ok_and(|v| v == "0") {
                return Ok(Vec::new());
            }
            let window = self.front()?;
            let (wx, wy, _, _) = window.refresh_rect()?;
            // A tree that cannot be read is no tree: pixels still work.
            let Ok(controls) = flowproof_driver::uia_scan::window_controls(window.hwnd()) else {
                return Ok(Vec::new());
            };
            Ok(controls
                .into_iter()
                .filter(|c| c.is_interactive() && c.enabled && !c.name.is_empty())
                .map(|c| AccessibleControl {
                    name: c.name,
                    control_type: c.control_type,
                    rect: (c.rect.0 - wx, c.rect.1 - wy, c.rect.2, c.rect.3),
                })
                .collect())
        }

        fn click_wait(&self) -> Duration {
            // Long, before the one retry it decides: over Citrix a click
            // can take a second to draw, and pressed again before it does,
            // a toggle is undone.
            Duration::from_millis(1500)
        }
        fn point_at(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
            let (wx, wy, _, _) = self.front()?.refresh_rect()?;
            self.inject(InputEvent::MouseMove {
                x: wx + x,
                y: wy + y,
            })?;
            // A hover tint fades in over about 150ms; a tooltip waits 400.
            std::thread::sleep(Duration::from_millis(150));
            Ok(())
        }
        fn blink_gap(&self) -> Duration {
            // Windows' default caret blink: 530ms on, 530ms off. Two
            // captures 300ms apart straddle a transition far more often
            // than not, and cost little on a screen that is not blinking.
            Duration::from_millis(300)
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

/// The production OCR. On Windows, BOTH engines, merged: Windows' own
/// reads interface text faster and better - a menu, a grey caption, a
/// tinted button - but drops a glyph standing alone, and a keypad is
/// nothing but; `ocrs` reads those. Measured on Windows Calculator: `ocrs`
/// found text inside 81% of the keys, Windows OCR 6%. A reading is every
/// Windows line plus each `ocrs` line it did not already read, and the
/// closer second looks are `ocrs`'s, which is what finds a thin `1`.
///
/// `FLOWPROOF_VISION_OCR=ocrs` or `=windows` pins one engine - the second
/// an error when Windows cannot read the profile's languages. Elsewhere,
/// and where Windows has no OCR language, `ocrs` alone.
pub enum AutoOcr {
    #[cfg(windows)]
    Windows(crate::vision_winocr::WindowsOcr),
    #[cfg(windows)]
    Both(crate::vision_winocr::WindowsOcr, OcrsEngine),
    Ocrs(OcrsEngine),
}

impl AutoOcr {
    pub fn new() -> Result<Self, DriverError> {
        let wanted = std::env::var("FLOWPROOF_VISION_OCR").unwrap_or_default();
        #[cfg(windows)]
        if wanted != "ocrs" {
            match crate::vision_winocr::WindowsOcr::new() {
                Ok(engine) if wanted == "windows" => return Ok(Self::Windows(engine)),
                Ok(engine) => return Ok(Self::Both(engine, OcrsEngine::new()?)),
                Err(e) if wanted == "windows" => return Err(e),
                Err(e) => vision_log(|| format!("{e} - reading with ocrs alone")),
            }
        }
        #[cfg(not(windows))]
        if wanted == "windows" {
            return Err(DriverError::Uia(
                "vision: FLOWPROOF_VISION_OCR=windows needs Windows".into(),
            ));
        }
        Ok(Self::Ocrs(OcrsEngine::new()?))
    }

    /// Which engine reads: `windows+ocrs`, `windows` or `ocrs`.
    pub fn name(&self) -> &'static str {
        match self {
            #[cfg(windows)]
            Self::Windows(_) => "windows",
            #[cfg(windows)]
            Self::Both(..) => "windows+ocrs",
            Self::Ocrs(_) => "ocrs",
        }
    }
}

/// `first`'s lines, then each of `second`'s that is not the same text in
/// the same place - a line both read once, so `the 2nd "Total"` still
/// counts what is on screen. Readings that DIFFER where they overlap are
/// both kept: `Sta ndard` from one and `Standard` from the other, and an
/// anchor matches whichever is right.
#[cfg_attr(not(windows), allow(dead_code))]
fn merge_readings(first: Vec<OcrLine>, second: Vec<OcrLine>) -> Vec<OcrLine> {
    // Case and punctuation aside - one engine keeps a colon the other
    // drops - but not spacing: `Sta ndard` is a different reading.
    let fold = |t: &str| {
        t.chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let centre_in = |(x, y, w, h): PixelRect, (ox, oy, ow, oh): PixelRect| {
        let (cx, cy) = (x + w as i32 / 2, y + h as i32 / 2);
        cx >= ox && cy >= oy && cx < ox + ow as i32 && cy < oy + oh as i32
    };
    let mut merged = first;
    let kept = merged.len();
    for line in second {
        let seen = merged[..kept].iter().any(|m| {
            fold(&m.text) == fold(&line.text)
                && (centre_in(line.rect, m.rect) || centre_in(m.rect, line.rect))
        });
        if !seen {
            merged.push(line);
        }
    }
    merged
}

impl OcrEngine for AutoOcr {
    fn recognize(&mut self, frame: &RgbaImage) -> Result<Vec<OcrLine>, DriverError> {
        match self {
            #[cfg(windows)]
            Self::Windows(e) => e.recognize(frame),
            #[cfg(windows)]
            Self::Both(windows, ocrs) => {
                let first = windows.recognize(frame)?;
                Ok(merge_readings(first, ocrs.recognize(frame)?))
            }
            Self::Ocrs(e) => e.recognize(frame),
        }
    }

    fn recognize_closer(
        &mut self,
        frame: &RgbaImage,
        scale: u32,
    ) -> Result<Vec<OcrLine>, DriverError> {
        match self {
            #[cfg(windows)]
            Self::Windows(e) => e.recognize_closer(frame, scale),
            #[cfg(windows)]
            Self::Both(_, ocrs) => ocrs.recognize_closer(frame, scale),
            Self::Ocrs(e) => e.recognize_closer(frame, scale),
        }
    }
}

#[cfg(windows)]
impl VisionAppDriver<native::NativeScreen, AutoOcr> {
    /// The production driver: GDI + SendInput + [`AutoOcr`].
    pub fn new() -> Result<Self, DriverError> {
        // Before any window rectangle or capture: at display scaling, an
        // unaware process gets scaled rectangles and physical captures.
        flowproof_driver::gdi::ensure_dpi_aware();
        let ocr = AutoOcr::new()?;
        vision_log(|| format!("reading with {} OCR", ocr.name()));
        Ok(Self::with_parts(native::NativeScreen::new(), ocr))
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
        /// Typing moves to the next frame: "the field now shows it".
        pub advance_on_type: bool,
        pub attached: Option<String>,
        pub clicks: Vec<(i32, i32)>,
        pub typed: Vec<String>,
        pub keys: Vec<(String, Vec<KeyMod>)>,
        /// Wheel turns received, in order. Each one moves the view a whole
        /// frame: negative to the next frame, positive to the previous,
        /// stopping at either end - a page whose frames ARE its scroll
        /// positions.
        pub scrolls: Vec<i32>,
        pub double_clicks: Vec<(i32, i32)>,
        /// A blinking caret: every other `frame()` returns this instead.
        pub blink_frame: Option<RgbaImage>,
        pub blink_on: bool,
        pub right_clicks: Vec<(i32, i32)>,
        /// What the accessibility tree reports, window-relative.
        pub controls: Vec<AccessibleControl>,
        pub hovers: Vec<(i32, i32)>,
        /// Window-relative areas another window is drawn over.
        pub covered: Vec<PixelRect>,
        pub drags: Vec<((i32, i32), (i32, i32))>,
        /// See [`VisionScreen::click_wait`]; zero, as for most tests,
        /// checks nothing.
        pub click_wait: Duration,
        /// Clicks the "app" takes but does nothing with, first ones first -
        /// what a remote session that swallowed them looks like.
        pub swallow_clicks: usize,
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
            if let Some(alternate) = &self.blink_frame {
                self.blink_on = !self.blink_on;
                if self.blink_on {
                    return Ok(alternate.clone());
                }
            }
            self.frames
                .get(self.current)
                .cloned()
                .ok_or_else(|| DriverError::Uia("fake screen has no frame".into()))
        }

        fn click(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
            self.clicks.push((x, y));
            if self.swallow_clicks > 0 {
                self.swallow_clicks -= 1;
                return Ok(());
            }
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
            if self.advance_on_type && self.current + 1 < self.frames.len() {
                self.current += 1;
            }
            Ok(())
        }

        fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError> {
            self.keys.push((key.to_string(), modifiers.to_vec()));
            Ok(())
        }

        fn double_click(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
            self.double_clicks.push((x, y));
            Ok(())
        }

        fn right_click(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
            self.right_clicks.push((x, y));
            Ok(())
        }

        fn scroll(&mut self, notches: i32) -> Result<(), DriverError> {
            self.scrolls.push(notches);
            if notches < 0 && self.current + 1 < self.frames.len() {
                self.current += 1;
            } else if notches > 0 && self.current > 0 {
                self.current -= 1;
            }
            Ok(())
        }

        fn window_origin(&mut self) -> Result<(i32, i32), DriverError> {
            Ok((0, 0))
        }

        fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
            Ok((1920, 1080))
        }

        fn click_wait(&self) -> Duration {
            self.click_wait
        }

        fn accessible_controls(&mut self) -> Result<Vec<AccessibleControl>, DriverError> {
            Ok(self.controls.clone())
        }

        fn reaches(&mut self, x: i32, y: i32) -> Result<bool, DriverError> {
            Ok(!self.covered.iter().any(|&(cx, cy, cw, ch)| {
                x >= cx && y >= cy && x < cx + cw as i32 && y < cy + ch as i32
            }))
        }

        fn move_to(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
            self.hovers.push((x, y));
            Ok(())
        }

        fn drag(&mut self, from: (i32, i32), to: (i32, i32)) -> Result<(), DriverError> {
            self.drags.push((from, to));
            Ok(())
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

    fn control(name: &str, rect: PixelRect) -> AccessibleControl {
        AccessibleControl {
            name: name.into(),
            control_type: "Button".into(),
            rect,
        }
    }

    /// Calculator draws `=` as strokes OCR never reads, and names the key
    /// `Equals`; an icon-only toolbar button has no text at all. Where the
    /// window has an accessibility tree, the name it reports is found when
    /// OCR finds nothing.
    #[test]
    fn a_key_ocr_cannot_read_is_found_by_the_name_the_tree_gives_it() {
        let mut d = driver();
        d.screen.controls = vec![
            control("Equals", (300, 150, 60, 40)),
            control("Settings", (360, 0, 30, 30)),
        ];
        d.invoke(&anchor("=")).expect("found as Equals");
        d.invoke(&anchor("settings"))
            .expect("found by name, any case");
        assert_eq!(d.screen.clicks, vec![(330, 170), (375, 15)]);
        assert_eq!(
            d.screen.scrolls,
            Vec::<i32>::new(),
            "no search: it is on screen"
        );
    }

    /// A toolbar button drawn only as an icon, over Citrix: the scene names
    /// it by its pixels, and a step using that name clicks it - wherever it
    /// is on the next run.
    #[test]
    fn an_icon_offered_in_the_scene_is_clicked_by_its_name() {
        let mut frame = blank_frame();
        for i in 0..16 {
            for t in 0..3 {
                frame.put_pixel(300 + i, 80 + i.min(13) + t, image::Rgba([20, 20, 20, 255]));
                frame.put_pixel(300 + t, 80 + i, image::Rgba([20, 20, 20, 255]));
            }
        }
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![frame]),
            FakeOcr::with_lines(login_lines()),
        );
        let scene = d.scene().expect("scene").expect("some");
        let entries: Vec<serde_json::Value> = serde_json::from_str(&scene).expect("json");
        let icon = entries
            .iter()
            .find(|e| e["kind"] == "icon")
            .and_then(|e| e["target"].as_str())
            .expect("an icon in the scene")
            .trim_start_matches("text:")
            .to_string();
        d.invoke(&anchor(&icon)).expect("clicks the icon");
        let (x, y) = d.screen.clicks[0];
        assert!((300..318).contains(&x) && (80..98).contains(&y), "{x},{y}");
    }

    #[test]
    fn drag_presses_on_one_anchor_and_releases_on_the_other_and_hover_presses_nothing() {
        let mut d = driver();
        d.drag(&anchor("Sign in"), &anchor("Password"))
            .expect("drags");
        d.hover(&anchor("User name")).expect("hovers");
        assert_eq!(d.screen.drags, vec![((55, 110), (60, 68))]);
        assert_eq!(d.screen.hovers, vec![(65, 28)]);
        assert!(d.screen.clicks.is_empty());
        // A drop target that is not there fails before the button goes down.
        assert!(d.drag(&anchor("Sign in"), &anchor("Nowhere")).is_err());
        assert_eq!(d.screen.drags.len(), 1);
    }

    #[test]
    fn two_engines_reading_one_line_count_it_once_and_differing_readings_both_stay() {
        let merged = merge_readings(
            vec![
                OcrLine::new("Total", (10, 10, 50, 16)),
                OcrLine::new("Sta ndard", (10, 40, 80, 16)),
            ],
            vec![
                OcrLine::new("total:", (11, 11, 52, 15)),
                OcrLine::new("Standard", (10, 40, 80, 16)),
                OcrLine::new("7", (200, 200, 10, 16)),
            ],
        );
        let texts: Vec<&str> = merged.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["Total", "Sta ndard", "Standard", "7"]);
    }

    /// Windows Settings shows "Windows Update" as the page heading above
    /// the nav item of that name; reading order took the heading, and the
    /// click did nothing. A control the tree names exactly is the target.
    #[test]
    fn a_control_named_exactly_beats_the_same_text_elsewhere() {
        let mut d = driver();
        d.screen.controls = vec![control("Sign in", (300, 150, 60, 40))];
        d.invoke(&anchor("Sign in")).expect("clicks");
        assert_eq!(d.screen.clicks, vec![(330, 170)]);
    }

    /// The tree reports a control under a popup menu as if it showed; a
    /// click there reaches the menu. Covered, the text that shows wins.
    #[test]
    fn a_named_control_something_covers_gives_way_to_the_text_on_screen() {
        let mut d = driver();
        d.screen.controls = vec![control("Sign in", (300, 150, 60, 40))];
        d.screen.covered = vec![(280, 140, 100, 60)];
        d.invoke(&anchor("Sign in")).expect("clicks");
        assert_eq!(d.screen.clicks, vec![(55, 110)]);
    }

    /// A control is the field itself: typing into an edit box the tree
    /// names lands inside it, not a label-height to its right.
    #[test]
    fn typing_into_a_named_control_lands_inside_it() {
        let mut d = driver();
        d.screen.controls = vec![AccessibleControl {
            name: "Search".into(),
            control_type: "Edit".into(),
            rect: (100, 160, 200, 30),
        }];
        d.type_text(&anchor("Search"), "cats").expect("types");
        assert_eq!(d.screen.clicks, vec![(200, 175)]);
    }

    /// The tree's controls are offered to the authoring model by name - but
    /// a button whose caption OCR already read is listed once, as text.
    #[test]
    fn the_scene_offers_what_only_the_tree_names() {
        let mut d = driver();
        d.screen.controls = vec![
            control("Sign in", (10, 95, 100, 30)),
            control("Back", (360, 60, 30, 30)),
        ];
        let scene = d.scene().expect("scene").expect("some");
        let entries: Vec<serde_json::Value> = serde_json::from_str(&scene).expect("json");
        let targets: Vec<&str> = entries
            .iter()
            .filter_map(|e| e["target"].as_str())
            .collect();
        assert_eq!(targets.iter().filter(|t| **t == "text:Sign in").count(), 1);
        assert!(targets.contains(&"text:Back"), "{targets:?}");
    }

    /// A label with its field drawn well to the right: the click lands in
    /// the drawn box, not a label-height past the label's end, in the gap.
    #[test]
    fn typing_beside_a_label_lands_in_the_box_drawn_there() {
        let mut frame = blank_frame();
        let grey = image::Rgba([140, 140, 140, 255]);
        let (x0, y0, w, h) = (220u32, 14u32, 160u32, 28u32);
        for x in x0..x0 + w {
            frame.put_pixel(x, y0, grey);
            frame.put_pixel(x, y0 + h - 1, grey);
        }
        for y in y0..y0 + h {
            frame.put_pixel(x0, y, grey);
            frame.put_pixel(x0 + w - 1, y, grey);
        }
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![frame]),
            FakeOcr::with_lines(login_lines()),
        );
        d.type_text(&anchor("User name"), "ada").expect("types");
        let (cx, cy) = d.screen.clicks[0];
        assert!(
            (290..=310).contains(&cx) && (24..=32).contains(&cy),
            "{cx},{cy}"
        );
    }

    fn desktop() -> RgbaImage {
        let mut frame = RgbaImage::from_pixel(640, 360, image::Rgba([30, 30, 30, 255]));
        let (x, y, pane, gap) = (12, 334, 7, 2);
        for (dx, dy) in [
            (0, 0),
            (pane + gap, 0),
            (0, pane + gap),
            (pane + gap, pane + gap),
        ] {
            for px in x + dx..x + dx + pane {
                for py in y + dy..y + dy + pane {
                    frame.put_pixel(px, py, image::Rgba([240, 240, 240, 255]));
                }
            }
        }
        frame
    }

    #[test]
    fn start_icon_is_grounded_without_fabricating_ocr_evidence() {
        let frame = desktop();
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![frame]),
            FakeOcr::with_lines(vec![OcrLine::new("ERGO - Desktop Viewer", (8, 4, 200, 20))]),
        );
        d.launch("", "Desktop Viewer", Duration::ZERO)
            .expect("vision");
        let scene: serde_json::Value =
            serde_json::from_str(&d.scene().expect("vision").expect("vision")).expect("vision");
        assert_eq!(scene[0]["label"], start::LABEL);
        assert_eq!(scene[0]["source"], "visual_icon");
        assert_eq!(scene[1]["actionable"], false, "title is not a button");
        assert!(!d.surface_text().expect("vision").contains(start::LABEL));
        let mut second = anchor(start::LABEL);
        second.nth = Some(2);
        assert!(!d.element_exists(&second).expect("vision"));
    }

    #[test]
    fn damaged_start_icon_never_falls_back_to_ocr_or_clicks() {
        let mut frame = desktop();
        for y in 334..341 {
            for x in 12..19 {
                frame.put_pixel(x, y, image::Rgba([30, 30, 30, 255]));
            }
        }
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![frame]),
            FakeOcr::with_lines(vec![OcrLine::new(start::LABEL, (8, 4, 200, 20))]),
        );
        assert!(!d.element_exists(&anchor(start::LABEL)).expect("vision"));
        let error = d.invoke(&anchor(start::LABEL)).expect_err("damaged icon");
        assert!(error.to_string().contains("unavailable"));
        assert_eq!(d.scene().expect("vision").as_deref(), Some("[]"));
        assert!(d.screen.clicks.is_empty());
    }


    #[test]
    fn typing_clicks_right_of_the_label_then_types() {
        let mut d = driver();
        d.type_text(&anchor("User name"), "ada").expect("types");
        // Label ends at x=110, height 16 → click lands one text-height right.
        assert_eq!(d.screen.clicks, vec![(126, 28)]);
        assert_eq!(d.screen.typed, vec!["ada".to_string()]);
    }

    /// Plan 15, finding 15: kitchensink's "Double click to edit" failed
    /// with "double-click is not supported by this driver". Both act ON
    /// the text, like a click.
    #[test]
    fn double_and_right_clicks_land_on_the_text() {
        let mut d = driver();
        d.double_click(&anchor("Sign in")).expect("double-clicks");
        d.context_click(&anchor("Password")).expect("right-clicks");
        assert_eq!(d.screen.double_clicks, vec![(55, 110)]);
        assert_eq!(d.screen.right_clicks, vec![(60, 68)]);
        assert!(d.screen.clicks.is_empty(), "not two single clicks");
    }

    #[test]
    fn a_line_splits_where_its_words_stand_controls_apart() {
        // A keypad row: four 17px glyphs about 80px apart.
        let keypad = [
            (40, 300, 10, 17),
            (118, 300, 10, 17),
            (196, 300, 10, 17),
            (272, 300, 12, 17),
        ];
        assert_eq!(split_at_gaps(&keypad), vec![0..1, 1..2, 2..3, 3..4]);
        // Running text: words a fraction of a height apart stay one line.
        let phrase = [(20, 20, 40, 16), (66, 20, 30, 16), (102, 20, 50, 16)];
        assert_eq!(split_at_gaps(&phrase), vec![0..3]);
        // A label and its value across a form's column gap come apart.
        let row = [(20, 60, 50, 16), (76, 60, 30, 16), (300, 60, 60, 16)];
        assert_eq!(split_at_gaps(&row), vec![0..2, 2..3]);
        assert!(split_at_gaps(&[]).is_empty());
    }

    /// Calculator's `×` as OCR reads it: `X`. The author writes the symbol
    /// the key shows, and it must still be found - but never on the
    /// caption's close button, and a letter never stands in for a digit.
    #[test]
    fn keypad_symbols_match_what_ocr_reads_them_as() {
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![blank_frame()]),
            FakeOcr::with_lines(vec![
                // The caption buttons as OCR read them live: maximize's
                // square as `O`, close as `X`. Matched first, top to bottom,
                // the close button shut Calculator.
                OcrLine::new("O", (350, 10, 11, 11)),
                OcrLine::new("X", (390, 9, 16, 14)),
                // The keys above the digits: `1/x` and the italic `x` of
                // `x²`, both read before the keypad, top to bottom.
                OcrLine::new("1x", (44, 52, 14, 13)),
                OcrLine::new("x", (118, 52, 18, 16)),
                OcrLine::new("X", (200, 60, 12, 16)),
                // Calculator's `9`, misread.
                OcrLine::new("O", (100, 120, 12, 16)),
            ]),
        );
        assert_eq!(
            d.resolve(&anchor("×")).expect("reads").map(|h| h.rect),
            Some((200, 60, 12, 16))
        );
        assert!(d.resolve(&anchor("0")).expect("reads").is_none());
        assert!(
            d.resolve(&anchor("1")).expect("reads").is_none(),
            "`1` is not `1/x`"
        );
    }

    /// OCR read Calculator's maximize box as a literal `0`, above the
    /// keypad, so `Click the "0" button` maximised the window mid-flow.
    #[test]
    fn the_window_caption_buttons_are_never_a_target() {
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![blank_frame()]),
            FakeOcr::with_lines(vec![
                OcrLine::new("Calculator", (26, 8, 70, 14)),
                OcrLine::new("0", (250, 9, 15, 13)),
                OcrLine::new("0", (125, 140, 10, 14)),
            ]),
        );
        assert_eq!(
            d.resolve(&anchor("0")).expect("reads").map(|h| h.rect),
            Some((125, 140, 10, 14))
        );
        assert!(
            d.resolve(&anchor("Calculator")).expect("reads").is_some(),
            "a word in the caption band is still text"
        );
    }

    /// An engine that sees Calculator's `1` only at three times the size,
    /// as `I`, and remembers which scales it was asked for.
    struct CloserStill {
        first: Vec<OcrLine>,
        at_3x: Vec<OcrLine>,
        scales: Vec<u32>,
    }

    impl OcrEngine for CloserStill {
        fn recognize(&mut self, _frame: &RgbaImage) -> Result<Vec<OcrLine>, DriverError> {
            Ok(self.first.clone())
        }
        fn recognize_closer(
            &mut self,
            _frame: &RgbaImage,
            scale: u32,
        ) -> Result<Vec<OcrLine>, DriverError> {
            self.scales.push(scale);
            Ok(if scale == 3 {
                self.at_3x.clone()
            } else {
                self.first.clone()
            })
        }
    }

    /// Calculator's `1` was too thin to be found at screen size, found at
    /// twice it in one framing and only at three times - as `I` - in
    /// another. Closer looks are taken only on a miss, and an unchanged
    /// frame is not read twice.
    #[test]
    fn a_target_missed_at_screen_size_is_found_on_a_closer_look() {
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![blank_frame()]),
            CloserStill {
                first: vec![OcrLine::new("2", (120, 100, 10, 17))],
                at_3x: vec![
                    OcrLine::new("I", (40, 100, 6, 17)),
                    OcrLine::new("2", (120, 100, 10, 17)),
                ],
                scales: Vec::new(),
            },
        );
        d.invoke(&anchor("1")).expect("clicks the 1");
        assert_eq!(d.screen.clicks, vec![(43, 108)]);
        assert_eq!(d.ocr.scales, vec![2, 3]);
        d.invoke(&anchor("1")).expect("clicks the 1 again");
        d.invoke(&anchor("2")).expect("clicks the 2");
        assert_eq!(
            d.ocr.scales,
            vec![2, 3],
            "the same frame is not read again, and a hit needs no closer look"
        );
    }

    /// OCR read Calculator's Keep-on-top icon, beside "Standard", as `5`,
    /// and missed the `5` key at screen size. The key is looked for closer
    /// before the `5` inside the phrase may stand in for it.
    #[test]
    fn a_short_anchor_prefers_a_key_found_closer_to_a_glyph_inside_a_phrase() {
        let header = OcrLine::new("E Standard 5", (20, 48, 140, 17));
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![blank_frame()]),
            CloserStill {
                first: vec![header.clone()],
                at_3x: vec![header, OcrLine::new("5", (124, 140, 10, 14))],
                scales: Vec::new(),
            },
        );
        d.invoke(&anchor("5")).expect("clicks the 5 key");
        assert_eq!(d.screen.clicks, vec![(129, 147)]);
    }

    #[test]
    fn pressing_a_symbol_types_it() {
        let mut d = driver();
        d.press_key("*", &[]).expect("presses");
        d.press_key("Enter", &[]).expect("presses");
        d.press_key("a", &[KeyMod::Ctrl]).expect("presses");
        assert_eq!(d.screen.typed, vec!["*"]);
        assert_eq!(
            d.screen.keys,
            vec![
                ("Enter".to_string(), vec![]),
                ("a".to_string(), vec![KeyMod::Ctrl])
            ]
        );
    }

    #[test]
    fn keystrokes_for_a_calculation_are_keyed_by_their_operands() {
        type D = VisionAppDriver<FakeScreen, FakeOcr>;
        // Calculator shows `42` for `12+30=`, never `1230`: no key, no
        // false "never appeared".
        assert!(D::readback_keys("12+30=").is_empty());
        assert_eq!(D::readback_keys("1234*5678="), vec!["1234", "5678"]);
        // Text keeps its keys across the punctuation it carries.
        assert_eq!(
            D::readback_keys("slow.typing@email.com"),
            vec!["slowty", "emailc"]
        );
        assert_eq!(D::readback_keys("ORD-1234"), vec!["ord1234"]);
    }

    /// A keypad row OCR'd as one line: the model reaches the `8` in
    /// `7 8 9 x` by a point along the line. That used to be refused as
    /// "clicking at an offset is not supported by this driver".
    #[test]
    fn click_at_lands_at_that_fraction_of_the_text() {
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![blank_frame()]),
            FakeOcr::with_lines(vec![OcrLine::new("7 8 9 x", (20, 60, 300, 40))]),
        );
        d.click_at(&anchor("7 8 9 x"), 37.5, 50.0).expect("clicks");
        assert_eq!(d.screen.clicks, vec![(133, 80)]);
    }

    /// A screen whose second frame is the first with a block drawn: what
    /// clicking "Sign in" leads to, when the click is taken.
    fn checked_click_screen(swallow: usize) -> FakeScreen {
        let before = blank_frame();
        let mut after = before.clone();
        for x in 200..240 {
            for y in 150..190 {
                after.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
            }
        }
        FakeScreen {
            advance_on_click: Some((20, 100, 70, 20)),
            click_wait: Duration::from_millis(20),
            swallow_clicks: swallow,
            ..FakeScreen::with_frames(vec![before, after])
        }
    }

    #[test]
    fn a_click_the_screen_answers_is_made_once() {
        let mut d = VisionAppDriver::with_parts(
            checked_click_screen(0),
            FakeOcr::with_lines(login_lines()),
        );
        d.invoke(&anchor("Sign in")).expect("clicks");
        assert_eq!(d.screen.clicks.len(), 1);
    }

    /// Over Citrix a click can go nowhere; a computer-use model sees
    /// nothing happened and clicks again, and so does vision - once.
    #[test]
    fn a_click_that_changed_nothing_is_made_once_more() {
        let mut d = VisionAppDriver::with_parts(
            checked_click_screen(1),
            FakeOcr::with_lines(login_lines()),
        );
        d.invoke(&anchor("Sign in")).expect("clicks");
        assert_eq!(d.screen.clicks.len(), 2);
        assert_eq!(d.screen.current, 1, "the second click was taken");
    }

    /// `0` on a Calculator showing 0 changes nothing, rightly: one retry,
    /// then the step goes on rather than failing.
    #[test]
    fn a_click_that_rightly_changes_nothing_is_retried_once_and_does_not_fail() {
        let mut d = VisionAppDriver::with_parts(
            checked_click_screen(5),
            FakeOcr::with_lines(login_lines()),
        );
        d.invoke(&anchor("Sign in")).expect("goes on");
        assert_eq!(d.screen.clicks.len(), 2);
    }

    #[test]
    fn a_blinking_caret_is_not_a_click_answered_but_a_ticked_box_is() {
        let before = blank_frame();
        let mut caret = before.clone();
        for y in 40..60 {
            caret.put_pixel(100, y, image::Rgba([0, 0, 0, 255]));
            caret.put_pixel(101, y, image::Rgba([0, 0, 0, 255]));
        }
        assert!(only_caret_differs(&before, &caret));
        let mut tick = before.clone();
        for i in 0..8 {
            tick.put_pixel(30 + i, 30 + i, image::Rgba([0, 0, 0, 255]));
        }
        assert!(
            !only_caret_differs(&before, &tick),
            "a few pixels of tick still count"
        );
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

    /// File Explorer, pixels only: the folder read with OCR's spaces after
    /// its underscores, and a file whose name starts with the folder's. The
    /// folder is the whole match, and the file must not be clicked for it.
    #[test]
    fn a_name_ocr_broke_after_its_underscores_beats_a_longer_name_it_begins() {
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![blank_frame()]),
            FakeOcr::with_lines(vec![
                OcrLine::new("ssis_pc_data.dtproj", (20, 20, 120, 12)),
                OcrLine::new("ssis_ pc_ data", (20, 60, 64, 12)),
            ]),
        );
        let rect = d
            .element_rect(&anchor("ssis_pc_data"))
            .expect("resolves")
            .expect("found");
        assert_eq!(rect, (20, 60, 64, 12));
        // The file is still found by its own name.
        let file = d
            .element_rect(&anchor("ssis_pc_data.dtproj"))
            .expect("resolves")
            .expect("found");
        assert_eq!(file, (20, 20, 120, 12));
    }

    /// File Explorer, pixels only: "Home" in the breadcrumb and again in the
    /// navigation pane. The scene names each by its place, and the ordinal
    /// it gives the lower one is the one resolution clicks.
    #[test]
    fn a_text_shown_twice_is_offered_by_ordinal_and_that_ordinal_clicks_it() {
        let mut d = VisionAppDriver::with_parts(
            FakeScreen::with_frames(vec![blank_frame()]),
            FakeOcr::with_lines(vec![
                OcrLine::new("Home", (20, 160, 40, 14)),
                OcrLine::new("Home", (200, 60, 40, 14)),
                OcrLine::new("Search Home", (400, 60, 90, 14)),
            ]),
        );
        let scene = d.scene().expect("scene").expect("some");
        let entries: Vec<serde_json::Value> = serde_json::from_str(&scene).expect("json");
        let target = |x: i64| {
            entries
                .iter()
                .find(|e| e["rect"][0] == x)
                .and_then(|e| e["target"].as_str())
                .expect("listed")
                .to_string()
        };
        assert_eq!(target(200), "text:Home");
        assert_eq!(target(20), "nth:2:text:Home");
        assert_eq!(target(400), "text:Search Home");
        d.invoke(&anchor("Home").with_nth(Some(2))).expect("clicks");
        let (x, y) = d.screen.clicks[0];
        assert!((20..60).contains(&x) && (160..174).contains(&y), "{x},{y}");
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
        // One and a half label-heights below its bottom edge (20 + 16 + 24).
        assert_eq!(d.screen.clicks, vec![(65, 60)]);
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

    /// A recorded clear is `type_text("")` with replace. Live, that left the
    /// old value selected and the next step saw it still there.
    #[test]
    fn typing_nothing_deletes_the_selection() {
        let mut d = driver();
        d.type_text(&anchor("User name"), "").expect("clears");
        assert_eq!(
            d.screen.keys,
            vec![
                ("a".to_string(), vec![KeyMod::Ctrl]),
                ("Delete".to_string(), vec![])
            ]
        );
        assert!(d.screen.typed.is_empty());
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

/// Plan 15, finding 7: a target below the fold. Each fake frame is one
/// scroll position, and its OCR is chosen by the frame itself, so the
/// driver only ever learns what is on screen by scrolling to it.
#[cfg(test)]
mod scroll_search_tests {
    use super::fake::FakeScreen;
    use super::*;

    /// OCR that reads the page index a frame was painted with.
    struct PagedOcr(Vec<Vec<OcrLine>>);

    impl OcrEngine for PagedOcr {
        fn recognize(&mut self, frame: &RgbaImage) -> Result<Vec<OcrLine>, DriverError> {
            Ok(self.0[frame.get_pixel(0, 0)[0] as usize].clone())
        }
    }

    fn page_driver(
        pages: Vec<Vec<OcrLine>>,
        start: usize,
    ) -> VisionAppDriver<FakeScreen, PagedOcr> {
        let frames = (0..pages.len())
            .map(|i| RgbaImage::from_pixel(400, 200, image::Rgba([i as u8, 255, 255, 255])))
            .collect();
        let mut screen = FakeScreen::with_frames(frames);
        screen.current = start;
        VisionAppDriver::with_parts(screen, PagedOcr(pages))
    }

    fn form() -> Vec<Vec<OcrLine>> {
        vec![
            vec![OcrLine::new("Actions", (20, 20, 80, 20))],
            vec![
                OcrLine::new("Email address", (20, 20, 120, 16)),
                OcrLine::new("Password", (20, 60, 80, 16)),
            ],
            vec![
                OcrLine::new("Coupon Code", (20, 40, 100, 16)),
                OcrLine::new("Submit", (20, 90, 60, 20)),
            ],
        ]
    }

    fn anchor(text: &str) -> UiaSelector {
        UiaSelector {
            name: Some(text.into()),
            ..UiaSelector::default()
        }
    }

    #[test]
    fn a_field_below_the_fold_is_found_by_scrolling_down() {
        let mut d = page_driver(form(), 0);
        d.type_text(&anchor("Coupon Code"), "HALFOFF")
            .expect("found two screens down");
        assert_eq!(d.screen.scrolls, vec![-5, -5], "down a page at a time");
        // The click is in the frame the target was FOUND in: right of the
        // label as it sits on that screen.
        assert_eq!(d.screen.clicks, vec![(136, 48)]);
        assert_eq!(d.screen.typed, vec!["HALFOFF".to_string()]);
    }

    #[test]
    fn a_target_above_is_found_on_the_way_back_up() {
        let mut d = page_driver(form(), 2);
        d.invoke(&anchor("Actions")).expect("found above");
        // Already at the bottom: one scroll down proves the edge, then up.
        assert_eq!(d.screen.scrolls, vec![-5, 5, 5]);
        assert_eq!(d.screen.current, 0);
    }

    #[test]
    fn a_missing_target_searches_both_ways_then_fails_cleanly() {
        let mut d = page_driver(form(), 0);
        let err = d.invoke(&anchor("Logout")).expect_err("not on any screen");
        assert!(err.to_string().contains("after scrolling"), "{err}");
        // Down: 0→1, 1→2, then 2→2 reads the same, which is the edge. Up
        // the same way. Bounded by the page, not by MAX_SCROLL_PAGES.
        assert_eq!(d.screen.scrolls, vec![-5, -5, -5, 5, 5, 5]);
        assert!(d.screen.clicks.is_empty());
    }

    /// Plan 15, finding 21. Frame 1 is the screen after the click, frame 2
    /// after typing: `arrives` decides whether the value shows up there.
    fn email_form(arrives: bool) -> VisionAppDriver<FakeScreen, PagedOcr> {
        let sample = OcrLine::new("cy.type('fake@email.com')", (20, 200, 220, 16));
        let label = OcrLine::new("Email address", (20, 20, 120, 16));
        let mut after = vec![sample.clone(), label.clone()];
        if arrives {
            after.push(OcrLine::new("fake@email.com", (160, 20, 110, 16)));
        }
        let before = vec![sample, label];
        let mut d = page_driver(vec![before.clone(), before, after], 0);
        d.screen.advance_on_click = Some((0, 0, 400, 200));
        d.screen.advance_on_type = true;
        d
    }

    #[test]
    fn a_typed_value_that_reached_the_field_reads_back() {
        let mut d = email_form(true);
        d.type_text(&anchor("Email address"), "fake@email.com")
            .expect("types");
        assert_eq!(
            d.typed_value_arrived("fake@email.com").expect("reads"),
            Some(true)
        );
    }

    /// The code sample prints the same value, so "is it on screen" would
    /// say yes; only "is it on screen MORE than before" tells the truth.
    #[test]
    fn a_value_typed_into_nothing_does_not_read_back_off_a_code_sample() {
        let mut d = email_form(false);
        d.type_text(&anchor("Email address"), "fake@email.com")
            .expect("types");
        assert_eq!(
            d.typed_value_arrived("fake@email.com").expect("reads"),
            Some(false)
        );
    }

    /// Live: `Quokka77` in the field read back as `Quokka/Right...` - the
    /// value's head still counts.
    #[test]
    fn a_value_whose_last_characters_ocr_garbles_still_reads_back() {
        let label = OcrLine::new("Name", (20, 20, 40, 16));
        let before = vec![label.clone()];
        let after = vec![label, OcrLine::new("Quokka/Right click", (80, 20, 160, 16))];
        let mut d = page_driver(vec![before.clone(), before, after], 0);
        d.screen.advance_on_click = Some((0, 0, 400, 200));
        d.screen.advance_on_type = true;
        d.type_text(&anchor("Name"), "Quokka77").expect("types");
        assert_eq!(
            d.typed_value_arrived("Quokka77").expect("reads"),
            Some(true)
        );
    }

    /// A bare `Type X` goes wherever focus is. Frame 1 is after typing.
    #[test]
    fn a_value_typed_into_focus_reads_back_only_if_it_appeared() {
        let label = OcrLine::new("Double click to edit", (20, 20, 160, 16));
        for (arrives, expected) in [(true, Some(true)), (false, Some(false))] {
            let mut after = vec![label.clone()];
            if arrives {
                after.push(OcrLine::new(
                    "Zebra42Double click to edit",
                    (20, 60, 200, 16),
                ));
            }
            let mut d = page_driver(vec![vec![label.clone()], after], 0);
            d.screen.advance_on_type = true;
            d.type_focused("Zebra42").expect("types");
            assert_eq!(d.typed_value_arrived("Zebra42").expect("reads"), expected);
        }
    }

    #[test]
    fn a_value_too_short_or_untyped_cannot_be_judged() {
        let mut d = email_form(false);
        d.type_text(&anchor("Email address"), "ab").expect("types");
        assert_eq!(d.typed_value_arrived("ab").expect("reads"), None);
        assert_eq!(
            d.typed_value_arrived("something else").expect("reads"),
            None,
            "only the value last typed"
        );
    }

    /// A box left of its label: frame "U" draws it empty, "C" with a dark
    /// tick. The page index rides in pixel (0,0) for `PagedOcr`.
    fn checkbox_frames(states: &[bool]) -> VisionAppDriver<FakeScreen, PagedOcr> {
        let label = vec![OcrLine::new("Checkbox one", (60, 20, 100, 16))];
        let mut d = page_driver(vec![label.clone(); states.len()], 0);
        for (i, (frame, checked)) in d.screen.frames.iter_mut().zip(states).enumerate() {
            // A white page, as a real one is: `page_driver` tints the whole
            // frame with its index, and that tint reads as ink. Only pixel
            // (0,0) needs to carry the index.
            *frame = RgbaImage::from_pixel(400, 200, image::Rgba([255, 255, 255, 255]));
            frame.put_pixel(0, 0, image::Rgba([i as u8, 255, 255, 255]));
            if *checked {
                for py in 22..34 {
                    for px in 40..52 {
                        frame.put_pixel(px, py, image::Rgba([0, 0, 0, 255]));
                    }
                }
            }
        }
        d.screen.advance_on_click = Some((0, 0, 400, 200));
        d
    }

    #[test]
    fn checking_an_unchecked_box_clicks_once_and_sees_the_tick() {
        let mut d = checkbox_frames(&[false, true]);
        d.set_checked(&anchor("Checkbox one"), true)
            .expect("checks");
        assert_eq!(d.screen.clicks.len(), 1);
    }

    /// SET, not toggle: a box already checked is flipped away by the click,
    /// seen to go the wrong way, and clicked back.
    #[test]
    fn checking_a_box_already_checked_leaves_it_checked() {
        let mut d = checkbox_frames(&[true, false, true]);
        d.set_checked(&anchor("Checkbox one"), true)
            .expect("stays checked");
        assert_eq!(d.screen.clicks.len(), 2);
        assert_eq!(d.screen.current, 2);
    }

    #[test]
    fn unchecking_a_checked_box_clicks_once() {
        let mut d = checkbox_frames(&[true, false]);
        d.set_checked(&anchor("Checkbox one"), false)
            .expect("unchecks");
        assert_eq!(d.screen.clicks.len(), 1);
    }

    #[test]
    fn a_click_that_changes_nothing_is_refused_not_assumed() {
        let mut d = checkbox_frames(&[false]);
        let err = d
            .set_checked(&anchor("Checkbox one"), true)
            .expect_err("disabled, or missed");
        assert!(err.to_string().contains("changed nothing visible"), "{err}");
    }

    /// Plan 15: the caret beside freshly typed `Quokka77` made OCR read
    /// `Quokka/`. The garbled reading is one frame; the caret-off frame,
    /// half a blink later, reads true - and both reach the check.
    #[test]
    fn a_blinking_caret_does_not_hide_freshly_typed_text() {
        let mut d = page_driver(
            vec![
                vec![OcrLine::new(
                    "Quokka/Right click to edit",
                    (20, 20, 200, 16),
                )],
                vec![OcrLine::new(
                    "Quokka77Right click to edit",
                    (20, 20, 200, 16),
                )],
            ],
            0,
        );
        let caret_off = d.screen.frames[1].clone();
        d.screen.frames.truncate(1);
        d.screen.blink_frame = Some(caret_off);
        let surface = d.surface_text().expect("reads");
        assert!(surface.contains("Quokka77"), "{surface}");
    }

    /// Plan 15: the search stopped with kitchensink's Coupon Code label on
    /// the last line and its box below the window. The point below the
    /// label is off the frame, so the driver nudges and looks again.
    #[test]
    fn an_action_point_past_the_window_edge_scrolls_back_into_view() {
        let mut d = page_driver(
            vec![
                vec![OcrLine::new("Coupon Code", (20, 190, 100, 16))],
                vec![OcrLine::new("Coupon Code", (20, 100, 100, 16))],
            ],
            0,
        );
        let below = anchor("Coupon Code").with_relation(Some("below".into()));
        d.type_text(&below, "HALFOFF").expect("types");
        assert_eq!(d.screen.scrolls, vec![-EDGE_NUDGE_NOTCHES]);
        assert_eq!(d.screen.clicks, vec![(70, 140)], "inside the frame");
    }

    /// Live: the box below Coupon Code sat just INSIDE the bottom edge,
    /// took its value, and showed it half cut off. Within two lines of the
    /// edge is too close.
    #[test]
    fn an_action_point_just_inside_the_edge_still_scrolls_clear_of_it() {
        let mut d = page_driver(
            vec![
                vec![OcrLine::new("Coupon Code", (20, 150, 100, 16))],
                vec![OcrLine::new("Coupon Code", (20, 60, 100, 16))],
            ],
            0,
        );
        let below = anchor("Coupon Code").with_relation(Some("below".into()));
        d.type_text(&below, "HALFOFF").expect("types");
        assert_eq!(d.screen.scrolls, vec![-EDGE_NUDGE_NOTCHES]);
        assert_eq!(d.screen.clicks, vec![(70, 100)]);
    }

    /// Live: OCR measured `Describe:` 10px tall, and the point one height
    /// below landed between the label and its box. The height is floored.
    #[test]
    fn below_a_label_ocr_measured_short_still_lands_in_the_box() {
        let mut d = page_driver(vec![vec![OcrLine::new("Describe:", (20, 20, 60, 10))]], 0);
        let below = anchor("Describe:").with_relation(Some("below".into()));
        d.type_text(&below, "Narwhal31").expect("types");
        // bottom edge 30, plus 1.5 x 14.
        assert_eq!(d.screen.clicks, vec![(50, 51)]);
    }

    /// Replay reveals once, then polls existence: the reveal is what finds
    /// an off-screen target, and leaves it on screen for the poll.
    #[test]
    fn reveal_brings_an_off_screen_target_into_the_frame() {
        let mut d = page_driver(form(), 0);
        d.reveal(&anchor("Submit")).expect("reveals");
        assert!(d.element_exists(&anchor("Submit")).expect("reads"));
        assert_eq!(d.screen.scrolls, vec![-5, -5]);
        // Not found anywhere is not an error here - the wait reports it.
        d.reveal(&anchor("Logout")).expect("quietly absent");
    }

    /// Live: OCR read the label `Describe:` without its colon, and the
    /// clear flow could not find its field anywhere on the page.
    #[test]
    fn a_label_whose_trailing_colon_ocr_dropped_still_matches() {
        let mut d = page_driver(
            vec![vec![
                OcrLine::new("Describe", (20, 20, 70, 16)),
                OcrLine::new("Coupon Code;", (20, 60, 100, 16)),
            ]],
            0,
        );
        let rect = d.element_rect(&anchor("Describe:")).expect("reads");
        assert_eq!(rect, Some((20, 20, 70, 16)));
        let rect = d.element_rect(&anchor("Coupon Code:")).expect("reads");
        assert_eq!(rect, Some((20, 60, 100, 16)), "a misread colon too");
        // Exact still wins, and nothing matches what is not there.
        assert!(d
            .element_rect(&anchor("Describe it"))
            .expect("reads")
            .is_none());
    }

    #[test]
    fn ocr_case_wobble_falls_back_to_a_case_insensitive_match() {
        let mut d = page_driver(
            vec![vec![
                OcrLine::new("Fake@email.com", (20, 20, 110, 16)),
                OcrLine::new("SAVE", (20, 60, 40, 16)),
                OcrLine::new("Save", (20, 100, 40, 16)),
            ]],
            0,
        );
        // Recorded as `Fake@...`, read back as `fake@...` (or vice versa).
        let rect = d
            .element_rect(&anchor("fake@email.com"))
            .expect("reads")
            .expect("folded match");
        assert_eq!(rect, (20, 20, 110, 16));
        assert_eq!(
            d.read_text(&anchor("fake@email.com")).expect("reads"),
            "Fake@email.com",
            "the screen's own spelling, not the folded one"
        );
        // Exact case still wins over an earlier line that only folds equal.
        let save = d.element_rect(&anchor("Save")).expect("reads");
        assert_eq!(save, Some((20, 100, 40, 16)));
    }

    /// Auto-wait polls `element_exists`; a poll that scrolled would move
    /// the page on every tick.
    #[test]
    fn existence_checks_never_scroll() {
        let mut d = page_driver(form(), 0);
        assert!(!d.element_exists(&anchor("Submit")).expect("reads"));
        assert!(d.screen.scrolls.is_empty());
    }

    #[test]
    fn scroll_steps_reach_the_edges_and_into_view() {
        let mut d = page_driver(form(), 1);
        d.scroll(None, ScrollTo::Bottom).expect("to bottom");
        assert_eq!(d.screen.current, 2);
        d.scroll(None, ScrollTo::Top).expect("to top");
        assert_eq!(d.screen.current, 0);
        d.scroll(Some(&anchor("Password")), ScrollTo::IntoView)
            .expect("into view");
        assert_eq!(d.screen.current, 1);
        let err = d
            .scroll(None, ScrollTo::Offset(300))
            .expect_err("pixels have no offset to scroll to");
        assert!(err.to_string().contains("cannot scroll"), "{err}");
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
