//! Grounding accuracy for the vision adapter, scored against ground truth:
//! a ScreenSpot-style benchmark run on a live desktop window.
//!
//! Frontier computer-use agents are judged by one question - asked to
//! click X, does the click land inside X? - and so is this. The ground
//! truth is the window's own accessibility tree (on Windows, UIA): every
//! interactive control it reports, with its name and rectangle. Vision
//! never sees that tree when it drives an app; here it is only the answer
//! key.
//!
//! Three numbers per window:
//!
//! - **coverage** - of the controls on screen, how many vision can address
//!   at all: some element vision offers lands inside it. A button drawn as
//!   an icon has no text for OCR, and is not covered.
//! - **name accuracy** - of the named controls, how many `Click "<name>"`
//!   lands inside. The accessible name is what an author writes.
//! - **wrong clicks** - of those attempts, how many land inside a
//!   DIFFERENT control. Worse than a miss: a miss fails the step, a wrong
//!   click acts - on Calculator it put the window in compact mode.

use serde::Serialize;

use flowproof_driver::PixelRect;

/// One control of the answer key, in screen coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroundTruth {
    pub name: String,
    pub control_type: String,
    pub rect: PixelRect,
}

/// How vision did on one control.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroundingCase {
    pub name: String,
    pub control_type: String,
    pub rect: PixelRect,
    /// Some element vision offers lands inside this control.
    pub covered: bool,
    /// Where `Click "<name>"` landed, screen coordinates; `None` when vision
    /// found no match, or the control has no name.
    pub point: Option<(i32, i32)>,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Landed inside the control.
    Hit,
    /// Landed inside a different control.
    WrongControl,
    /// Landed somewhere no control is.
    Stray,
    /// Vision found no match.
    NotFound,
    /// The control has no name to ask for.
    Unnamed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroundingReport {
    pub window: String,
    pub controls: usize,
    pub covered: usize,
    pub named: usize,
    pub hits: usize,
    pub wrong_control: usize,
    pub cases: Vec<GroundingCase>,
}

impl GroundingReport {
    pub fn coverage(&self) -> f64 {
        ratio(self.covered, self.controls)
    }

    pub fn name_accuracy(&self) -> f64 {
        ratio(self.hits, self.named)
    }

    /// One line per window, for a terminal.
    pub fn summary(&self) -> String {
        format!(
            "{}: coverage {}/{} ({:.0}%), name accuracy {}/{} ({:.0}%), wrong clicks {}",
            self.window,
            self.covered,
            self.controls,
            100.0 * self.coverage(),
            self.hits,
            self.named,
            100.0 * self.name_accuracy(),
            self.wrong_control
        )
    }
}

fn ratio(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        n as f64 / d as f64
    }
}

pub fn contains((x, y, w, h): PixelRect, (px, py): (i32, i32)) -> bool {
    px >= x && py >= y && px < x + w as i32 && py < y + h as i32
}

/// The controls worth scoring: inside the window, big enough to aim at, not
/// the window's own chrome (see [`is_window_chrome`]), and one per rectangle - UIA nests a button's text inside the button, and a
/// list item inside its row, and one target should count once. The
/// innermost NAMED control of a rectangle is kept, since that is the name
/// a step would use.
pub fn answer_key(controls: Vec<GroundTruth>, window: PixelRect) -> Vec<GroundTruth> {
    let (wx, wy, ww, wh) = window;
    let mut kept: Vec<GroundTruth> = Vec::new();
    for c in controls {
        let (x, y, w, h) = c.rect;
        let (cx, cy) = (x + w as i32 / 2, y + h as i32 / 2);
        if w < 6 || h < 6 || !contains((wx, wy, ww, wh), (cx, cy)) {
            continue;
        }
        // A window-sized "control" is the app's own frame.
        if w >= ww * 9 / 10 && h >= wh * 9 / 10 {
            continue;
        }
        if is_window_chrome(&c, window) {
            continue;
        }
        match kept.iter_mut().find(|k| k.rect == c.rect) {
            Some(k) if k.name.is_empty() => *k = c,
            Some(_) => {}
            None => kept.push(c),
        }
    }
    kept
}

/// The window's system menu and caption buttons, which vision refuses on
/// purpose - no step means them, and OCR reads their glyphs as `0` and `X`,
/// so Calculator was maximised and closed by steps that meant a key. Scored,
/// they were a fifth of the misses on every window, for a behaviour that is
/// right. Known by name AND place - the caption band, the buttons at its
/// right end - since an app's own buttons in the title bar (File Explorer's
/// tabs) are not chrome, and a "Close" lower down is the app's own.
pub fn is_window_chrome(control: &GroundTruth, (wx, wy, ww, _): PixelRect) -> bool {
    let (x, y, w, h) = control.rect;
    let (cx, cy) = (x + w as i32 / 2, y + h as i32 / 2);
    if cy - wy >= CAPTION_HEIGHT {
        return false;
    }
    // Calculator names them for itself: "Close Calculator".
    let first = control.name.split_whitespace().next().unwrap_or("");
    match control.control_type.as_str() {
        "MenuItem" => control.name == "System",
        "Button" => {
            matches!(first, "Minimize" | "Maximize" | "Restore" | "Close")
                && cx >= wx + ww as i32 - CAPTION_BUTTONS_WIDTH
        }
        _ => false,
    }
}

/// The caption band, as vision's driver draws it (48px at 150% scaling).
const CAPTION_HEIGHT: i32 = 48;

/// Three caption buttons at their widest, 47px each at 150%, with room.
const CAPTION_BUTTONS_WIDTH: i32 = 160;

/// What a keypad key SHOWS, for the name an accessibility tree speaks it
/// by: an author looking at Calculator writes `Click "7"`, not "Seven", and
/// pixels can only ever find the former. Scoring asks for the glyph when
/// the name is not found.
pub fn shown_glyph(name: &str) -> Option<&'static str> {
    Some(match name.to_lowercase().as_str() {
        "zero" => "0",
        "one" => "1",
        "two" => "2",
        "three" => "3",
        "four" => "4",
        "five" => "5",
        "six" => "6",
        "seven" => "7",
        "eight" => "8",
        "nine" => "9",
        "plus" => "+",
        "minus" => "-",
        "multiply by" => "×",
        "divide by" => "÷",
        "equals" => "=",
        "decimal separator" => ".",
        "percent" => "%",
        "clear entry" => "CE",
        "clear" => "C",
        "positive negative" => "+/-",
        "reciprocal" => "1/x",
        "memory add" => "M+",
        "memory subtract" => "M-",
        "memory store" => "MS",
        "memory recall" => "MR",
        "clear all memory" => "MC",
        _ => return None,
    })
}

/// Score one window. `targets` are the points vision offers (screen
/// coordinates); `locate(name, nth, relation)` answers where an action on
/// the nth "<name>" lands - `nth` the control's place among those of its
/// name, top to bottom then left to right, as an author disambiguates;
/// `relation` `right_of` for a field, where `Type` aims - asked
/// for a keypad key's glyph too (see [`shown_glyph`]) when its name is
/// not found.
pub fn score(
    window: &str,
    key: &[GroundTruth],
    targets: &[(i32, i32)],
    mut locate: impl FnMut(&str, u32, &str) -> Option<(i32, i32)>,
) -> GroundingReport {
    let mut cases = Vec::new();
    for control in key {
        let mut namesakes: Vec<PixelRect> = key
            .iter()
            .filter(|k| k.name == control.name)
            .map(|k| k.rect)
            .collect();
        namesakes.sort_by_key(|(x, y, _, _)| (*y, *x));
        let nth = namesakes
            .iter()
            .position(|r| *r == control.rect)
            .unwrap_or(0) as u32
            + 1;
        let covered = targets.iter().any(|p| contains(control.rect, *p));
        let (point, outcome) = if control.name.is_empty() {
            (None, Outcome::Unnamed)
        } else {
            // A field is typed into, and `Type` lands beside its label.
            let relation = if matches!(control.control_type.as_str(), "Edit" | "ComboBox") {
                "right_of"
            } else {
                "inside"
            };
            let found = locate(&control.name, nth, relation).or_else(|| {
                shown_glyph(&control.name).and_then(|glyph| locate(glyph, nth, relation))
            });
            match found {
                None => (None, Outcome::NotFound),
                Some(p) if contains(control.rect, p) => (Some(p), Outcome::Hit),
                Some(p) => {
                    // Inside some other control - the smallest that holds it,
                    // since controls nest - is a wrong click.
                    let other = key
                        .iter()
                        .filter(|k| k.rect != control.rect && contains(k.rect, p))
                        .min_by_key(|k| u64::from(k.rect.2) * u64::from(k.rect.3));
                    match other {
                        Some(_) => (Some(p), Outcome::WrongControl),
                        None => (Some(p), Outcome::Stray),
                    }
                }
            }
        };
        cases.push(GroundingCase {
            name: control.name.clone(),
            control_type: control.control_type.clone(),
            rect: control.rect,
            covered,
            point,
            outcome,
        });
    }
    let count = |o: Outcome| cases.iter().filter(|c| c.outcome == o).count();
    GroundingReport {
        window: window.to_string(),
        controls: cases.len(),
        covered: cases.iter().filter(|c| c.covered).count(),
        named: cases.len() - count(Outcome::Unnamed),
        hits: count(Outcome::Hit),
        wrong_control: count(Outcome::WrongControl),
        cases,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gt(name: &str, rect: PixelRect) -> GroundTruth {
        GroundTruth {
            name: name.into(),
            control_type: "Button".into(),
            rect,
        }
    }

    #[test]
    fn a_click_inside_another_control_is_a_wrong_click_not_a_miss() {
        let key = vec![gt("Seven", (0, 0, 10, 10)), gt("Eight", (20, 0, 10, 10))];
        let report = score("calc", &key, &[(5, 5)], |name, _, _| match name {
            "Seven" => Some((25, 5)),
            _ => None,
        });
        assert_eq!(report.cases[0].outcome, Outcome::WrongControl);
        assert_eq!(report.cases[1].outcome, Outcome::NotFound);
        assert_eq!(report.covered, 1);
        assert_eq!(report.wrong_control, 1);
        assert_eq!(report.hits, 0);
    }

    #[test]
    fn a_repeated_name_is_asked_for_by_its_place_among_namesakes() {
        let key = vec![gt("Start", (0, 50, 10, 10)), gt("Start", (0, 0, 10, 10))];
        let report = score("clock", &key, &[], |_, nth, _| {
            Some((5, 5 + 50 * (nth as i32 - 1)))
        });
        assert_eq!(report.hits, 2, "the lower Start is the 2nd: {report:?}");
        let report = score("clock", &key, &[], |_, nth, _| {
            Some((5, 55 - 50 * (nth as i32 - 1)))
        });
        assert_eq!(report.hits, 0, "the other way round, both miss: {report:?}");
    }

    #[test]
    fn the_answer_key_keeps_one_named_control_per_rectangle() {
        let key = answer_key(
            vec![
                gt("", (10, 10, 20, 20)),
                gt("Back", (10, 10, 20, 20)),
                gt("tiny", (50, 50, 3, 3)),
                gt("frame", (0, 0, 200, 200)),
                gt("outside", (500, 500, 20, 20)),
            ],
            (0, 0, 200, 200),
        );
        assert_eq!(key, vec![gt("Back", (10, 10, 20, 20))]);
    }

    #[test]
    fn the_answer_key_leaves_out_the_window_chrome_but_not_an_apps_own_title_bar_buttons() {
        let mut system = gt("System", (2, 2, 20, 20));
        system.control_type = "MenuItem".into();
        let key = answer_key(
            vec![
                system,
                gt("Minimize", (300, 2, 40, 20)),
                gt("Close Calculator", (350, 2, 40, 20)),
                gt("Close Tab", (120, 2, 20, 20)),
                gt("Close document", (60, 100, 20, 20)),
            ],
            (0, 0, 400, 300),
        );
        let names: Vec<&str> = key.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(names, ["Close Tab", "Close document"]);
    }

    #[test]
    fn a_memory_key_is_asked_for_by_what_it_shows() {
        let key = vec![gt("Memory add", (0, 0, 40, 20))];
        let report = score("calc", &key, &[], |name, _, _| {
            (name == "M+").then_some((10, 10))
        });
        assert_eq!(report.hits, 1, "{report:?}");
    }
}
