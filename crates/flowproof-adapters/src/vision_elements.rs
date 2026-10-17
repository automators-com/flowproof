//! Controls found from pixels alone: the boxes a screen is drawn with.
//!
//! OCR finds text, and text is only half of what a person aims at. An input
//! box beside its label is a rectangle with nothing in it; a toolbar button
//! is an icon. Frontier computer-use agents see both because a model looks at
//! the screenshot. A deterministic replay cannot ask a model, so this is the
//! classical version of the same perception (what OmniParser's detector does
//! with a network, done with edges): every edge pixel of the frame, joined
//! into connected shapes, each shape's bounding box classified by how much
//! of its outline is drawn.
//!
//! - a **box** - wide, and most of its rectangle's border is edge: an input
//!   field, a bordered or filled button, a dropdown;
//! - an **icon** - small and not text: a glyph-only button, a checkbox.
//!
//! What uses them: a field addressed by its label (`right_of`, `below`)
//! lands inside the box drawn there instead of at a fixed offset, so a wide
//! label gap or a tall field no longer misses; and the grounding benchmark
//! counts icons as addressable once something can name them.

use image::RgbaImage;

use flowproof_driver::PixelRect;

/// What a detected shape looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementKind {
    Box,
    Icon,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub kind: ElementKind,
    pub rect: PixelRect,
}

/// Brightness change between neighbouring pixels that counts as an edge.
/// A field's 1px grey border on white is a step of 40-80; anti-aliased
/// text edges are steeper; a gradient fill changes a few levels a pixel.
const EDGE_STEP: i32 = 28;

/// Of a box's perimeter, the share that must be drawn for it to be a box.
/// A field's border is drawn all the way round; a focused one may have a
/// thicker bottom edge, a dropdown an arrow cut into its right side.
const BORDER_DRAWN: f32 = 0.82;

fn luma(frame: &RgbaImage, x: u32, y: u32) -> i32 {
    let [r, g, b, _] = frame.get_pixel(x, y).0;
    (i32::from(r) * 299 + i32::from(g) * 587 + i32::from(b) * 114) / 1000
}

/// The frame's edge map: a pixel is an edge when it differs from its right
/// or lower neighbour by [`EDGE_STEP`] or more.
fn edges(frame: &RgbaImage) -> (Vec<bool>, u32, u32) {
    let (w, h) = frame.dimensions();
    let mut map = vec![false; (w * h) as usize];
    if w < 2 || h < 2 {
        return (map, w, h);
    }
    let lum: Vec<i32> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| luma(frame, x, y))
        .collect();
    for y in 0..h - 1 {
        for x in 0..w - 1 {
            let i = (y * w + x) as usize;
            let here = lum[i];
            if (here - lum[i + 1]).abs() >= EDGE_STEP
                || (here - lum[i + w as usize]).abs() >= EDGE_STEP
            {
                map[i] = true;
            }
        }
    }
    (map, w, h)
}

/// Bounding boxes of the edge map's 8-connected shapes.
fn shapes(map: &[bool], w: u32, h: u32) -> Vec<PixelRect> {
    let mut seen = vec![false; map.len()];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..map.len() {
        if !map[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        while let Some(i) = stack.pop() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let j = (ny as u32 * w + nx as u32) as usize;
                    if map[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        out.push((x0 as i32, y0 as i32, x1 - x0 + 1, y1 - y0 + 1));
    }
    out
}

/// The share of a rectangle's perimeter that is edge, allowing the border
/// to sit a pixel inside or outside the bounding box (anti-aliasing, the
/// edge map's one-pixel shift).
fn border_drawn(map: &[bool], w: u32, (x, y, rw, rh): PixelRect) -> f32 {
    let rows = (map.len() / w.max(1) as usize) as i32;
    let edge = |px: i32, py: i32| {
        px >= 0
            && py >= 0
            && px < w as i32
            && py < rows
            && map[(py as u32 * w + px as u32) as usize]
    };
    let at = |px: i32, py: i32| {
        [(0, 0), (-1, 0), (1, 0), (0, -1), (0, 1)]
            .iter()
            .any(|(dx, dy)| edge(px + dx, py + dy))
    };
    let (right, bottom) = (x + rw as i32 - 1, y + rh as i32 - 1);
    let mut drawn = 0usize;
    let mut total = 0usize;
    for px in x..=right {
        total += 2;
        drawn += usize::from(at(px, y)) + usize::from(at(px, bottom));
    }
    for py in y + 1..bottom {
        total += 2;
        drawn += usize::from(at(x, py)) + usize::from(at(right, py));
    }
    if total == 0 {
        0.0
    } else {
        drawn as f32 / total as f32
    }
}

/// Every box and icon on the frame, boxes first, each in reading order.
/// `text` are the boxes OCR read text in: a shape that is one of them (a
/// word's own outline) is text, not an icon.
pub fn detect(frame: &RgbaImage, text: &[PixelRect]) -> Vec<Element> {
    let (map, w, h) = edges(frame);
    let mut boxes = Vec::new();
    let mut pieces: Vec<PixelRect> = Vec::new();
    for rect in shapes(&map, w, h) {
        let (_, _, rw, rh) = rect;
        // The frame's own border, a window-wide divider: structure.
        if rw >= w * 9 / 10 || rh >= h * 9 / 10 {
            continue;
        }
        // Wide and outlined all round: a field or a button. A small square
        // outline is a checkbox or a filled glyph - an icon, below.
        if rw >= 40 && rh >= 12 && rw >= 2 * rh && border_drawn(&map, w, rect) >= BORDER_DRAWN {
            boxes.push(Element {
                kind: ElementKind::Box,
                rect,
            });
        } else if rw.max(rh) <= ICON_MAX && !text.iter().any(|t| overlap(*t, rect) > 0.5) {
            pieces.push(rect);
        }
    }
    let mut icons: Vec<Element> = icon_clusters(pieces)
        .into_iter()
        .filter(|&(_, _, rw, rh)| rw.max(rh) >= 8 && rw.min(rh) >= 6)
        .map(|rect| Element {
            kind: ElementKind::Icon,
            rect,
        })
        .collect();
    let order = |e: &Element| (e.rect.1, e.rect.0);
    boxes.sort_by_key(order);
    icons.sort_by_key(order);
    boxes.extend(icons);
    boxes
}

/// The largest side an icon is drawn at: toolbar icons are 16-32px, at
/// 150% scaling up to 48.
const ICON_MAX: u32 = 56;

/// An icon is often several shapes - a pencil's tip and body, a folder's
/// tab - so shapes within a few pixels of one another are one icon, as long
/// as the whole stays icon-sized.
fn icon_clusters(mut pieces: Vec<PixelRect>) -> Vec<PixelRect> {
    let near = |a: PixelRect, b: PixelRect| {
        let gap_x = (a.0 - (b.0 + b.2 as i32)).max(b.0 - (a.0 + a.2 as i32));
        let gap_y = (a.1 - (b.1 + b.3 as i32)).max(b.1 - (a.1 + a.3 as i32));
        gap_x <= 3 && gap_y <= 3
    };
    let mut merged = true;
    while merged {
        merged = false;
        'outer: for i in 0..pieces.len() {
            for j in i + 1..pieces.len() {
                let joined = union(pieces[i], pieces[j]);
                if near(pieces[i], pieces[j]) && joined.2.max(joined.3) <= ICON_MAX {
                    pieces[i] = joined;
                    pieces.swap_remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
    }
    pieces
}

/// A name for an icon made from its pixels: a 64-bit difference hash, as
/// 16 hex digits. Brightness rising or falling between neighbouring cells
/// of the icon shrunk to 9x8 - what a person recognises an icon by, its
/// shape and shading, and stable across the small rendering differences
/// between two runs. Over Citrix an icon-only button has no text and no
/// accessibility name; this is the name a step can use for it.
pub fn icon_hash(frame: &RgbaImage, (x, y, w, h): PixelRect) -> String {
    let (fw, fh) = frame.dimensions();
    let x0 = x.clamp(0, fw as i32 - 1) as u32;
    let y0 = y.clamp(0, fh as i32 - 1) as u32;
    let crop =
        image::imageops::crop_imm(frame, x0, y0, w.min(fw - x0).max(1), h.min(fh - y0).max(1))
            .to_image();
    let small = image::imageops::resize(&crop, 9, 8, image::imageops::FilterType::Triangle);
    let mut bits: u64 = 0;
    for row in 0..8 {
        for col in 0..8 {
            let l = luma(&small, col, row);
            let r = luma(&small, col + 1, row);
            bits = (bits << 1) | u64::from(l < r);
        }
    }
    format!("{bits:016x}")
}

/// Bits two icon hashes differ in.
pub fn hash_distance(a: &str, b: &str) -> Option<u32> {
    let a = u64::from_str_radix(a, 16).ok()?;
    let b = u64::from_str_radix(b, 16).ok()?;
    Some((a ^ b).count_ones())
}

/// The operator glyphs OCR does not read: a key drawn only as strokes is
/// not text to a text detector. On Windows Calculator neither engine ever
/// read `=` or `−`, so over Citrix - where there is no accessibility tree to
/// ask - those keys could not be clicked at all. Each is recognised here by
/// its geometry: `−` one short bar; `=` two bars of a width, one above the
/// other; `÷` a bar with a dot above and below; `+` and `×` a cross square
/// and diagonal. Shapes OCR already read as text are left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Symbol {
    Minus,
    Equals,
    Divide,
    Plus,
    Times,
}

impl Symbol {
    /// The symbol an anchor names, in any of the ways it is written.
    pub fn named(anchor: &str) -> Option<Self> {
        Some(match anchor.trim() {
            "-" | "\u{2212}" | "\u{2013}" => Self::Minus,
            "=" => Self::Equals,
            "\u{f7}" | "/" => Self::Divide,
            "+" => Self::Plus,
            "\u{d7}" | "*" | "x" | "X" => Self::Times,
            _ => return None,
        })
    }
}

/// A shape's ink - pixels contrasting with the background around it - as a
/// mask over its box, trimmed to the ink. `None` when the box has none.
fn ink(frame: &RgbaImage, (x, y, w, h): PixelRect) -> Option<(Vec<bool>, PixelRect)> {
    let (fw, fh) = (frame.width() as i32, frame.height() as i32);
    let (x0, y0) = ((x - 2).max(0), (y - 2).max(0));
    let (x1, y1) = ((x + w as i32 + 2).min(fw), (y + h as i32 + 2).min(fh));
    if x1 - x0 < 3 || y1 - y0 < 3 {
        return None;
    }
    // The background is the commonest brightness on the ring around the box.
    let mut ring: Vec<i32> = Vec::new();
    for px in x0..x1 {
        ring.push(luma(frame, px as u32, y0 as u32));
        ring.push(luma(frame, px as u32, (y1 - 1) as u32));
    }
    for py in y0..y1 {
        ring.push(luma(frame, x0 as u32, py as u32));
        ring.push(luma(frame, (x1 - 1) as u32, py as u32));
    }
    ring.sort_unstable();
    let background = ring[ring.len() / 2];
    // Ink is counted inside the shape's own box only: the ring is for the
    // background. Counted on the ring too, the dot of a `÷` two pixels above
    // its bar joined the bar, and the bar was no longer a bar.
    let (bx0, by0) = (x.max(0), y.max(0));
    let (bx1, by1) = ((x + w as i32).min(fw), (y + h as i32).min(fh));
    let (mut ix0, mut iy0, mut ix1, mut iy1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for py in by0..by1 {
        for px in bx0..bx1 {
            if (luma(frame, px as u32, py as u32) - background).abs() >= 48 {
                ix0 = ix0.min(px);
                iy0 = iy0.min(py);
                ix1 = ix1.max(px);
                iy1 = iy1.max(py);
            }
        }
    }
    if ix0 > ix1 {
        return None;
    }
    let (iw, ih) = ((ix1 - ix0 + 1) as u32, (iy1 - iy0 + 1) as u32);
    let mut mask = vec![false; (iw * ih) as usize];
    for py in iy0..=iy1 {
        for px in ix0..=ix1 {
            mask[((py - iy0) as u32 * iw + (px - ix0) as u32) as usize] =
                (luma(frame, px as u32, py as u32) - background).abs() >= 48;
        }
    }
    Some((mask, (ix0, iy0, iw, ih)))
}

/// Inked share of a sub-box of a mask, in mask coordinates.
fn filled(mask: &[bool], w: u32, (x0, y0, x1, y1): (u32, u32, u32, u32)) -> f32 {
    let mut on = 0usize;
    let mut all = 0usize;
    for y in y0..y1 {
        for x in x0..x1 {
            all += 1;
            on += usize::from(mask[(y * w + x) as usize]);
        }
    }
    if all == 0 {
        0.0
    } else {
        on as f32 / all as f32
    }
}

/// `+` or `×` from one square-ish shape's ink.
fn cross(mask: &[bool], (_, _, w, h): PixelRect) -> Option<Symbol> {
    if w < 7 || h < 7 || w * 10 < h * 7 || h * 10 < w * 7 {
        return None;
    }
    let (qw, qh) = (w / 4, h / 4);
    let corners = [
        (0, 0, qw, qh),
        (w - qw, 0, w, qh),
        (0, h - qh, qw, h),
        (w - qw, h - qh, w, h),
    ]
    .map(|r| filled(mask, w, r));
    // Two pixels square at the middle: a 1px stroke crossing it fills a
    // quarter or more of that.
    let centre = filled(mask, w, (w / 2 - 1, h / 2 - 1, w / 2 + 1, h / 2 + 1));
    let mid_row_ends = filled(mask, w, (0, h / 2 - 1, qw, h / 2 + 1)).min(filled(
        mask,
        w,
        (w - qw, h / 2 - 1, w, h / 2 + 1),
    ));
    let mid_col_ends = filled(mask, w, (w / 2 - 1, 0, w / 2 + 1, qh)).min(filled(
        mask,
        w,
        (w / 2 - 1, h - qh, w / 2 + 1, h),
    ));
    if centre < 0.25 {
        return None;
    }
    if corners.iter().all(|c| *c < 0.12) && mid_row_ends > 0.4 && mid_col_ends > 0.4 {
        return Some(Symbol::Plus);
    }
    if corners.iter().all(|c| *c > 0.12) && mid_row_ends < 0.15 && mid_col_ends < 0.15 {
        return Some(Symbol::Times);
    }
    None
}

/// The rows of a mask that hold ink, as bands: each band's box, in mask
/// coordinates. `=` is two bands, `÷` three - whether or not their strokes
/// sit close enough to read as one shape.
fn bands(mask: &[bool], (_, _, w, h): PixelRect) -> Vec<PixelRect> {
    let mut out = Vec::new();
    let mut start: Option<u32> = None;
    let row_ink = |y: u32| {
        (0..w)
            .filter(|&x| mask[(y * w + x) as usize])
            .collect::<Vec<_>>()
    };
    for y in 0..=h {
        let inked = y < h && !row_ink(y).is_empty();
        match (inked, start) {
            (true, None) => start = Some(y),
            (false, Some(y0)) => {
                let cols: Vec<u32> = (y0..y).flat_map(row_ink).collect();
                let (x0, x1) = (
                    *cols.iter().min().unwrap_or(&0),
                    *cols.iter().max().unwrap_or(&0),
                );
                out.push((x0 as i32, y0 as i32, x1 - x0 + 1, y - y0));
                start = None;
            }
            _ => {}
        }
    }
    out
}

fn is_bar((_, _, w, h): PixelRect) -> bool {
    w >= 6 && w >= 3 * h && h <= 6
}

/// The symbol a shape's ink bands spell, if any.
fn banded(bands: &[PixelRect]) -> Option<Symbol> {
    let alike = |a: PixelRect, b: PixelRect| {
        (a.0 - b.0).abs() <= 3 && a.2.abs_diff(b.2) * 10 <= a.2.max(b.2) * 3
    };
    let centred_dot = |d: PixelRect, bar: PixelRect| {
        d.2 * 2 <= bar.2 && ((d.0 + d.2 as i32 / 2) - (bar.0 + bar.2 as i32 / 2)).abs() <= 2
    };
    match *bands {
        [bar] if is_bar(bar) => Some(Symbol::Minus),
        [a, b] if is_bar(a) && is_bar(b) && alike(a, b) => Some(Symbol::Equals),
        [a, bar, b] if is_bar(bar) && centred_dot(a, bar) && centred_dot(b, bar) => {
            Some(Symbol::Divide)
        }
        _ => None,
    }
}

/// Every operator glyph on the frame and its box, in reading order. `text`
/// are the boxes OCR read: shapes inside them are letters, not operators.
pub fn find_symbols(frame: &RgbaImage, text: &[PixelRect]) -> Vec<(Symbol, PixelRect)> {
    let (map, w, h) = edges(frame);
    let mut bars: Vec<PixelRect> = Vec::new();
    let mut dots: Vec<PixelRect> = Vec::new();
    let mut found: Vec<(Symbol, PixelRect)> = Vec::new();
    for rect in shapes(&map, w, h) {
        let (_, _, rw, rh) = rect;
        if rw.max(rh) > 60 || rw.max(rh) < 3 || text.iter().any(|t| overlap(*t, rect) > 0.3) {
            continue;
        }
        let Some((mask, inked)) = ink(frame, rect) else {
            continue;
        };
        let (_, _, iw, ih) = inked;
        let symbol = banded(&bands(&mask, inked));
        if symbol == Some(Symbol::Minus) {
            // Maybe one bar of an `=` whose other bar is its own shape.
            bars.push(inked);
        } else if let Some(symbol) = symbol {
            found.push((symbol, inked));
        } else if iw <= 6 && ih <= 6 && iw * 2 >= ih && ih * 2 >= iw {
            dots.push(inked);
        } else if let Some(symbol) = cross(&mask, inked) {
            found.push((symbol, inked));
        }
    }
    let mut used = vec![false; bars.len()];
    for i in 0..bars.len() {
        for j in 0..bars.len() {
            let ((ax, ay, aw, ah), (bx, by, bw, _)) = (bars[i], bars[j]);
            if i == j || used[i] || used[j] || by <= ay {
                continue;
            }
            let gap = by - (ay + ah as i32);
            if (ax - bx).abs() <= 3
                && aw.abs_diff(bw) * 10 <= aw.max(bw) * 3
                && gap >= 1
                && gap as u32 <= aw
            {
                used[i] = true;
                used[j] = true;
                found.push((Symbol::Equals, union(bars[i], bars[j])));
            }
        }
    }
    for (i, bar) in bars.iter().enumerate() {
        if used[i] {
            continue;
        }
        let (bx, by, bw, bh) = *bar;
        let centre = bx + bw as i32 / 2;
        let near = |d: &&PixelRect, above: bool| {
            let (dx, dy, dw, dh) = **d;
            let dc = dx + dw as i32 / 2;
            let vertical = if above {
                by - (dy + dh as i32)
            } else {
                dy - (by + bh as i32)
            };
            (dc - centre).abs() <= 2 && vertical >= 1 && vertical as u32 <= bw
        };
        let above = dots.iter().find(|d| near(d, true));
        let below = dots.iter().find(|d| near(d, false));
        match (above, below) {
            (Some(a), Some(b)) => found.push((Symbol::Divide, union(union(*a, *bar), *b))),
            _ => found.push((Symbol::Minus, *bar)),
        }
    }
    // A bar inside an `=` or `÷` found whole is part of it, not a `−`.
    let whole: Vec<PixelRect> = found
        .iter()
        .filter(|(s, _)| matches!(s, Symbol::Equals | Symbol::Divide))
        .map(|(_, r)| *r)
        .collect();
    found.retain(|(s, r)| *s != Symbol::Minus || !whole.iter().any(|w| overlap(*w, *r) > 0.5));
    found.sort_by_key(|(_, (x, y, _, _))| (*y, *x));
    found
}

fn union(a: PixelRect, b: PixelRect) -> PixelRect {
    let left = a.0.min(b.0);
    let top = a.1.min(b.1);
    let right = (a.0 + a.2 as i32).max(b.0 + b.2 as i32);
    let bottom = (a.1 + a.3 as i32).max(b.1 + b.3 as i32);
    (left, top, (right - left) as u32, (bottom - top) as u32)
}

/// Share of `b`'s area inside `a`.
fn overlap(a: PixelRect, b: PixelRect) -> f32 {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    let ix = ((ax + aw as i32).min(bx + bw as i32) - ax.max(bx)).max(0);
    let iy = ((ay + ah as i32).min(by + bh as i32) - ay.max(by)).max(0);
    let area = (bw * bh).max(1) as f32;
    (ix * iy) as f32 / area
}

/// The box a label's field is drawn in: `relation` is where the field sits
/// from the label (`right_of` or `below`), and the box chosen is the nearest
/// one there that shares the label's row (or column), not the label's own
/// outline. `None` when nothing is drawn there - a borderless field - and the
/// caller keeps its offset guess.
pub fn field_box(elements: &[Element], label: PixelRect, relation: &str) -> Option<PixelRect> {
    let (lx, ly, lw, lh) = label;
    let (label_right, label_bottom) = (lx + lw as i32, ly + lh as i32);
    let label_mid_y = ly + lh as i32 / 2;
    let reach = 12 * lh.max(10) as i32;
    elements
        .iter()
        .filter(|e| e.kind == ElementKind::Box)
        .map(|e| e.rect)
        // Not a box around the label itself (a bordered button's caption).
        .filter(|r| overlap(*r, label) < 0.5 && overlap(label, *r) < 0.5)
        .filter_map(|r| {
            let (x, y, w, h) = r;
            let (right, bottom) = (x + w as i32, y + h as i32);
            let distance = match relation {
                "right_of" => {
                    let same_row = y <= label_mid_y && label_mid_y <= bottom;
                    (same_row && x >= label_right - 2 && x - label_right <= reach)
                        .then_some(x - label_right)
                }
                "below" => {
                    let same_column = x <= lx + lw as i32 / 2 && lx <= right;
                    (same_column && y >= label_bottom - 2 && y - label_bottom <= 4 * lh as i32 + 24)
                        .then_some(y - label_bottom)
                }
                _ => None,
            }?;
            Some((distance, r))
        })
        .min_by_key(|(d, _)| *d)
        .map(|(_, r)| r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn screen() -> RgbaImage {
        RgbaImage::from_pixel(400, 200, Rgba([255, 255, 255, 255]))
    }

    fn outline(img: &mut RgbaImage, (x, y, w, h): (u32, u32, u32, u32)) {
        let grey = Rgba([150, 150, 150, 255]);
        for px in x..x + w {
            img.put_pixel(px, y, grey);
            img.put_pixel(px, y + h - 1, grey);
        }
        for py in y..y + h {
            img.put_pixel(x, py, grey);
            img.put_pixel(x + w - 1, py, grey);
        }
    }

    fn fill(img: &mut RgbaImage, (x, y, w, h): (u32, u32, u32, u32)) {
        for py in y..y + h {
            for px in x..x + w {
                img.put_pixel(px, py, Rgba([30, 30, 30, 255]));
            }
        }
    }

    #[test]
    fn an_outlined_field_is_a_box_and_a_solid_glyph_is_an_icon() {
        let mut img = screen();
        outline(&mut img, (120, 40, 200, 28));
        fill(&mut img, (30, 120, 16, 16));
        let found = detect(&img, &[]);
        let kinds: Vec<_> = found.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![ElementKind::Box, ElementKind::Icon],
            "{found:?}"
        );
        let (x, y, w, h) = found[0].rect;
        assert!(
            (119..=121).contains(&x) && (39..=41).contains(&y),
            "{found:?}"
        );
        assert!(
            (198..=202).contains(&w) && (26..=30).contains(&h),
            "{found:?}"
        );
    }

    /// Calculator's operator keys, drawn as strokes: dark glyphs, a little
    /// anti-aliasing aside, centred in light keys.
    #[test]
    fn operator_keys_drawn_as_strokes_are_recognised_by_their_shape() {
        let mut img = RgbaImage::from_pixel(400, 120, Rgba([243, 243, 243, 255]));
        let ink =
            |img: &mut RgbaImage, x: u32, y: u32| img.put_pixel(x, y, Rgba([30, 30, 30, 255]));
        // `=` at x 20: two 16x2 bars, 4px apart.
        for x in 20..36 {
            for y in [50, 51, 56, 57] {
                ink(&mut img, x, y);
            }
        }
        // `−` at x 80: one 16x2 bar.
        for x in 80..96 {
            for y in [53, 54] {
                ink(&mut img, x, y);
            }
        }
        // `+` at x 140: 15x15, 2px strokes.
        for i in 0..15 {
            for t in 7..9 {
                ink(&mut img, 140 + i, 46 + t);
                ink(&mut img, 140 + t, 46 + i);
            }
        }
        // `×` at x 200: 15x15 diagonals, 2px.
        for i in 0..15 {
            for t in 0..2 {
                ink(&mut img, (200 + i + t).min(214), 46 + i);
                ink(&mut img, (214 - i + t).min(214), 46 + i);
            }
        }
        // `÷` at x 260: a bar with a 3x3 dot 3px above and below.
        for x in 260..276 {
            for y in [53, 54] {
                ink(&mut img, x, y);
            }
        }
        for x in 267..270 {
            for y in (47..50).chain(58..61) {
                ink(&mut img, x, y);
            }
        }
        let found: Vec<Symbol> = find_symbols(&img, &[])
            .into_iter()
            .map(|(s, _)| s)
            .collect();
        let mut sorted = found.clone();
        sorted.sort_by_key(|s| *s as u8);
        assert_eq!(
            sorted,
            vec![
                Symbol::Minus,
                Symbol::Equals,
                Symbol::Divide,
                Symbol::Plus,
                Symbol::Times
            ],
            "{found:?}"
        );
        // A word's strokes are OCR's to read.
        assert!(find_symbols(&img, &[(10, 40, 400, 30)]).is_empty());
    }

    /// Over Citrix a toolbar button drawn as an icon is clicked by the
    /// name its pixels give it, and found again wherever it has moved.
    #[test]
    fn an_icon_is_named_by_its_pixels_and_found_again_after_moving() {
        let draw = |img: &mut RgbaImage, x: u32, y: u32, kind: u8| {
            for i in 0..18 {
                for j in 0..18 {
                    let on = match kind {
                        0 => i < 3 || j < 3 || i > 14 || j > 14, // a hollow square
                        _ => (i as i32 - j as i32).abs() < 3 || i > 14, // a slash on a base
                    };
                    if on {
                        img.put_pixel(x + i, y + j, Rgba([40, 40, 40, 255]));
                    }
                }
            }
        };
        let mut before = screen();
        draw(&mut before, 30, 30, 0);
        draw(&mut before, 80, 30, 1);
        let icons: Vec<PixelRect> = detect(&before, &[])
            .into_iter()
            .filter(|e| e.kind == ElementKind::Icon)
            .map(|e| e.rect)
            .collect();
        assert_eq!(icons.len(), 2, "{icons:?}");
        let square = icon_hash(&before, icons[0]);
        let slash = icon_hash(&before, icons[1]);
        assert!(
            hash_distance(&square, &slash).expect("hex") > 12,
            "{square} {slash}"
        );

        let mut after = screen();
        draw(&mut after, 200, 120, 1);
        draw(&mut after, 250, 120, 0);
        let moved: Vec<String> = detect(&after, &[])
            .into_iter()
            .filter(|e| e.kind == ElementKind::Icon)
            .map(|e| icon_hash(&after, e.rect))
            .collect();
        assert_eq!(hash_distance(&moved[0], &slash), Some(0));
        assert_eq!(hash_distance(&moved[1], &square), Some(0));
    }

    #[test]
    fn a_shape_ocr_read_as_text_is_not_an_icon() {
        let mut img = screen();
        fill(&mut img, (30, 120, 16, 16));
        assert!(detect(&img, &[(28, 118, 22, 22)]).is_empty());
    }

    #[test]
    fn a_label_finds_the_box_beside_it_not_one_on_another_row() {
        let elements = vec![
            Element {
                kind: ElementKind::Box,
                rect: (300, 100, 150, 26),
            },
            Element {
                kind: ElementKind::Box,
                rect: (160, 38, 200, 28),
            },
        ];
        let label = (40, 44, 80, 16);
        assert_eq!(
            field_box(&elements, label, "right_of"),
            Some((160, 38, 200, 28))
        );
        assert_eq!(
            field_box(&elements, (300, 60, 60, 16), "below"),
            Some((300, 100, 150, 26))
        );
        assert_eq!(field_box(&elements, (40, 150, 80, 16), "right_of"), None);
    }
}
