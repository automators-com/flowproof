//! Conservative recognition of the four-pane Windows taskbar glyph.
//! Geometry is re-observed on every lookup; no screen coordinates are saved.
use flowproof_driver::PixelRect;
use image::RgbaImage;

pub(super) const LABEL: &str = "Windows Start button";

fn foreground(p: &image::Rgba<u8>) -> bool {
    let [r, g, b, a] = p.0;
    a >= 200
        && ((r.min(g).min(b) >= 200 && r.max(g).max(b) - r.min(g).min(b) <= 30)
            || (b >= 120 && b.saturating_sub(r) >= 40 && g.saturating_sub(r) >= 20))
}

pub(super) fn locate(frame: &RgbaImage) -> Option<PixelRect> {
    let (width, height) = frame.dimensions();
    if width < 64 || height < 64 {
        return None;
    }
    // Bottom taskbars only. Search the whole width, including centered Start.
    let top = height - (height / 8).min(120);
    let mut seen = vec![false; (width * (height - top)) as usize];
    let mut panes = Vec::new();
    for y in top..height {
        for x in 0..width {
            let index = ((y - top) * width + x) as usize;
            if seen[index] || !foreground(frame.get_pixel(x, y)) {
                continue;
            }
            seen[index] = true;
            let mut stack = vec![(x, y)];
            let (mut left, mut right, mut upper, mut lower, mut area) = (x, x, y, y, 0);
            while let Some((cx, cy)) = stack.pop() {
                left = left.min(cx);
                right = right.max(cx);
                upper = upper.min(cy);
                lower = lower.max(cy);
                area += 1;
                for (nx, ny) in [
                    (cx.wrapping_sub(1), cy),
                    (cx + 1, cy),
                    (cx, cy.wrapping_sub(1)),
                    (cx, cy + 1),
                ] {
                    if nx >= width || ny < top || ny >= height {
                        continue;
                    }
                    let i = ((ny - top) * width + nx) as usize;
                    if !seen[i] && foreground(frame.get_pixel(nx, ny)) {
                        seen[i] = true;
                        stack.push((nx, ny));
                    }
                }
            }
            let (w, h) = (right - left + 1, lower - upper + 1);
            if (3..=28).contains(&w) && (3..=28).contains(&h) && area * 10 >= w * h * 7 {
                panes.push((left as i32, upper as i32, w, h));
                if panes.len() > 128 {
                    return None; // Bound combinatorial matching on noisy taskbars.
                }
            }
        }
    }
    let mut candidate = None;
    for &(x, y, w, h) in &panes {
        let close = |a: i32, b: i32| (a - b).abs() <= (w.max(h) / 4).max(2) as i32;
        let similar = |a: u32, b: u32| a.abs_diff(b) <= (a.max(b) / 3).max(2);
        let gap = |a: i32, b: i32| (1..=6).contains(&(b - a));
        for &(rx, ry, rw, rh) in &panes {
            if !gap(x + w as i32, rx) || !close(y, ry) || !similar(w, rw) || !similar(h, rh) {
                continue;
            }
            for &(bx, by, bw, bh) in &panes {
                if !close(x, bx) || !gap(y + h as i32, by) || !similar(w, bw) || !similar(h, bh) {
                    continue;
                }
                for &(dx, dy, dw, dh) in &panes {
                    if close(rx, dx)
                        && close(by, dy)
                        && gap(ry + rh as i32, dy)
                        && similar(rw, dw)
                        && similar(bh, dh)
                    {
                        let rect = (
                            x,
                            y.min(ry),
                            (dx + dw as i32 - x) as u32,
                            (dy + dh as i32 - y.min(ry)) as u32,
                        );
                        if rect.2 >= 7 && rect.3 >= 7 && rect.2.abs_diff(rect.3) <= 8 {
                            if candidate.is_some_and(|previous| previous != rect) {
                                return None;
                            }
                            candidate = Some(rect);
                        }
                    }
                }
            }
        }
    }
    // Ambiguous pixels are never a reason to click the first guess.
    candidate
}
