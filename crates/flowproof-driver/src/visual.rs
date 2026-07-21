//! Screenshot-assertion machinery shared by record (baseline minting) and
//! replay (comparison): mask application, pixel diffing, and the baseline
//! directory convention. One implementation for both executions — masking
//! or compare drift between record and replay would make the assertion lie.

use std::path::{Path, PathBuf};

use crate::{PixelRect, UiaSelector};

/// The baselines directory for a trace: sibling `<stem>.baselines/`,
/// where `<stem>` is the trace file name without its `.trace.jsonl`
/// (or, failing that, its last) extension. Baselines live NEXT to the
/// trace so the pair relocates as one bundle.
pub fn baselines_dir(trace_path: &Path) -> PathBuf {
    let name = trace_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("trace");
    let stem = name
        .strip_suffix(".trace.jsonl")
        .or_else(|| name.rsplit_once('.').map(|(s, _)| s))
        .unwrap_or(name);
    trace_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{stem}.baselines"))
}

/// Interpret a mask selector string the way quoted labels resolve:
/// `css:<sel>` and `id:<native id>` escape hatches, otherwise a text
/// anchor (accessible name).
pub fn mask_selector(mask: &str) -> UiaSelector {
    if let Some(css) = mask.strip_prefix("css:") {
        UiaSelector {
            css: Some(css.trim().to_string()),
            ..UiaSelector::default()
        }
    } else if let Some(id) = mask.strip_prefix("id:") {
        UiaSelector {
            automation_id: Some(id.trim().to_string()),
            ..UiaSelector::default()
        }
    } else {
        UiaSelector {
            name: Some(mask.to_string()),
            ..UiaSelector::default()
        }
    }
}

/// Blank `rects` (clamped to the image) to opaque black — applied to the
/// baseline at mint time and to the actual at compare time, so a masked
/// region can never differ.
pub fn apply_masks(img: &mut image::RgbaImage, rects: &[PixelRect]) {
    let (w, h) = (img.width() as i64, img.height() as i64);
    for &(rx, ry, rw, rh) in rects {
        let x0 = i64::from(rx).max(0).min(w);
        let y0 = i64::from(ry).max(0).min(h);
        let x1 = (i64::from(rx) + i64::from(rw)).max(0).min(w);
        let y1 = (i64::from(ry) + i64::from(rh)).max(0).min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                img.put_pixel(x as u32, y as u32, image::Rgba([0, 0, 0, 255]));
            }
        }
    }
}

/// Outcome of a pixel comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualCompare {
    pub total_pixels: u64,
    pub differing_pixels: u64,
}

impl VisualCompare {
    pub fn differing_fraction(&self) -> f64 {
        if self.total_pixels == 0 {
            0.0
        } else {
            self.differing_pixels as f64 / self.total_pixels as f64
        }
    }
}

/// Exact per-pixel comparison. `None` = dimension mismatch (always a
/// failure — the caller reports both sizes).
pub fn compare(baseline: &image::RgbaImage, actual: &image::RgbaImage) -> Option<VisualCompare> {
    if baseline.dimensions() != actual.dimensions() {
        return None;
    }
    let differing = baseline
        .pixels()
        .zip(actual.pixels())
        .filter(|(a, b)| a != b)
        .count() as u64;
    Some(VisualCompare {
        total_pixels: u64::from(baseline.width()) * u64::from(baseline.height()),
        differing_pixels: differing,
    })
}

/// Write `img` as `<dir>/<name>.png`, creating the directory. Returns the
/// written path.
pub fn save_baseline(dir: &Path, name: &str, img: &image::RgbaImage) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = dir.join(format!("{name}.png"));
    img.save(&path)
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(path)
}

/// Load `<dir>/<name>.png` as RGBA.
pub fn load_baseline(dir: &Path, name: &str) -> Result<image::RgbaImage, String> {
    let path = dir.join(format!("{name}.png"));
    let img = image::open(&path).map_err(|e| {
        format!(
            "no usable baseline at {} ({e}) — record the flow to mint it",
            path.display()
        )
    })?;
    Ok(img.to_rgba8())
}

/// Write any PNG (diff/actual artifacts), creating parent directories.
pub fn save_png(path: &Path, img: &image::RgbaImage) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    img.save(path)
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

/// A reviewable diff image: matching pixels dimmed, differing pixels red.
pub fn diff_image(baseline: &image::RgbaImage, actual: &image::RgbaImage) -> image::RgbaImage {
    let mut out = actual.clone();
    for (x, y, pixel) in out.enumerate_pixels_mut() {
        let same = baseline.get_pixel(x, y) == actual.get_pixel(x, y);
        if same {
            let image::Rgba([r, g, b, _]) = *pixel;
            *pixel = image::Rgba([r / 3 + 128, g / 3 + 128, b / 3 + 128, 255]);
        } else {
            *pixel = image::Rgba([220, 30, 30, 255]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, fill: [u8; 4]) -> image::RgbaImage {
        image::RgbaImage::from_pixel(w, h, image::Rgba(fill))
    }

    #[test]
    fn baseline_dir_strips_the_trace_suffix() {
        assert_eq!(
            baselines_dir(Path::new("/x/calc.trace.jsonl")),
            Path::new("/x/calc.baselines")
        );
        assert_eq!(
            baselines_dir(Path::new("odd.jsonl")),
            Path::new("odd.baselines")
        );
    }

    #[test]
    fn masks_clamp_and_blank_so_masked_changes_never_differ() {
        let mut a = img(10, 10, [10, 20, 30, 255]);
        let mut b = img(10, 10, [10, 20, 30, 255]);
        // b differs inside the masked region (a volatile clock)…
        b.put_pixel(5, 5, image::Rgba([200, 0, 0, 255]));
        // Extends past the image edge — clamped.
        let rects: [PixelRect; 1] = [(4, 4, 30, 3)];
        apply_masks(&mut a, &rects);
        apply_masks(&mut b, &rects);
        let result = compare(&a, &b).expect("same dims");
        assert_eq!(result.differing_pixels, 0, "masked change is invisible");
    }

    #[test]
    fn compare_counts_and_dimension_mismatch_is_none() {
        let a = img(4, 4, [0, 0, 0, 255]);
        let mut b = a.clone();
        b.put_pixel(0, 0, image::Rgba([1, 0, 0, 255]));
        b.put_pixel(3, 3, image::Rgba([1, 0, 0, 255]));
        let result = compare(&a, &b).expect("same dims");
        assert_eq!(result.differing_pixels, 2);
        assert_eq!(result.total_pixels, 16);
        assert!((result.differing_fraction() - 0.125).abs() < 1e-9);
        assert!(compare(&a, &img(5, 4, [0, 0, 0, 255])).is_none());
    }

    #[test]
    fn mask_selectors_honor_the_escape_hatches() {
        assert_eq!(mask_selector("css:.clock").css.as_deref(), Some(".clock"));
        assert_eq!(
            mask_selector("id:clockBox").automation_id.as_deref(),
            Some("clockBox")
        );
        assert_eq!(mask_selector("Sync now").name.as_deref(), Some("Sync now"));
    }
}
