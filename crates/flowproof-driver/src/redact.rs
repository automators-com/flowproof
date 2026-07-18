//! Redaction: the single implementation for masking sensitive regions in
//! every persisted pixel (recording frames now, stored screenshots later).
//!
//! Rules resolve to screen rectangles at capture time and are filled in the
//! in-memory frame BEFORE encoding — no unmasked bytes ever reach disk.
//! Password fields are always masked, regardless of configured rules.

use serde::{Deserialize, Serialize};

use crate::app::{AppDriver, PixelRect, UiaSelector};
use crate::DriverError;

/// What a redaction rule targets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactTarget {
    /// CSS selector (web).
    Css(String),
    /// UIA automation id (Windows).
    AutomationId(String),
    /// Fixed screen rectangle `[x, y, width, height]`.
    Rect(PixelRect),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactMode {
    /// Solid fill.
    Mask,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RedactionRule {
    pub target: RedactTarget,
    pub mode: RedactMode,
}

impl RedactionRule {
    pub fn css(selector: impl Into<String>) -> Self {
        Self {
            target: RedactTarget::Css(selector.into()),
            mode: RedactMode::Mask,
        }
    }

    pub fn automation_id(id: impl Into<String>) -> Self {
        Self {
            target: RedactTarget::AutomationId(id.into()),
            mode: RedactMode::Mask,
        }
    }
}

/// Resolve the configured rules (plus the always-on password rule) to the
/// rectangles that must be masked right now.
///
/// Fail-closed contract: a driver ERROR while resolving any rule aborts the
/// resolution — the caller must then drop the frame rather than persist it
/// unmasked. A rule whose element simply isn't on screen contributes no
/// rect (nothing to mask).
pub fn resolve_rects<D: AppDriver>(
    driver: &mut D,
    rules: &[RedactionRule],
) -> Result<Vec<PixelRect>, DriverError> {
    let mut rects = driver.password_rects()?;
    for rule in rules {
        match &rule.target {
            RedactTarget::Rect(rect) => rects.push(*rect),
            RedactTarget::Css(css) => {
                if let Some(rect) = driver.element_rect(&UiaSelector::css(css.clone()))? {
                    rects.push(rect);
                }
            }
            RedactTarget::AutomationId(id) => {
                if let Some(rect) = driver.element_rect(&UiaSelector::automation_id(id.clone()))? {
                    rects.push(rect);
                }
            }
        }
    }
    Ok(rects)
}

/// Fill the given rectangles with solid black, clamped to the frame bounds.
pub fn apply(frame: &mut image::RgbaImage, rects: &[PixelRect]) {
    let (width, height) = (frame.width() as i64, frame.height() as i64);
    for &(x, y, w, h) in rects {
        let x0 = (x as i64).clamp(0, width);
        let y0 = (y as i64).clamp(0, height);
        let x1 = (x as i64 + w as i64).clamp(0, width);
        let y1 = (y as i64 + h as i64).clamp(0, height);
        for py in y0..y1 {
            for px in x0..x1 {
                frame.put_pixel(px as u32, py as u32, image::Rgba([0, 0, 0, 255]));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_masks_exactly_the_requested_region() {
        let mut frame = image::RgbaImage::from_pixel(10, 10, image::Rgba([200, 10, 10, 255]));
        apply(&mut frame, &[(2, 3, 4, 2)]);
        for y in 0..10u32 {
            for x in 0..10u32 {
                let inside = (2..6).contains(&x) && (3..5).contains(&y);
                let expected = if inside {
                    image::Rgba([0, 0, 0, 255])
                } else {
                    image::Rgba([200, 10, 10, 255])
                };
                assert_eq!(*frame.get_pixel(x, y), expected, "at {x},{y}");
            }
        }
    }

    #[test]
    fn apply_clamps_out_of_bounds_rects() {
        let mut frame = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 255, 255, 255]));
        apply(&mut frame, &[(-2, -2, 100, 3)]);
        assert_eq!(*frame.get_pixel(0, 0), image::Rgba([0, 0, 0, 255]));
        assert_eq!(*frame.get_pixel(3, 3), image::Rgba([255, 255, 255, 255]));
    }

    #[test]
    fn rule_serde_shape() {
        let rule = RedactionRule::css("#ssn");
        let json = serde_json::to_string(&rule).expect("serializes");
        assert_eq!(json, r##"{"target":{"css":"#ssn"},"mode":"mask"}"##);
    }
}
