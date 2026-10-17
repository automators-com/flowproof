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
            if (here - lum[i + 1]).abs() >= EDGE_STEP || (here - lum[i + w as usize]).abs() >= EDGE_STEP
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
        px >= 0 && py >= 0 && px < w as i32 && py < rows && map[(py as u32 * w + px as u32) as usize]
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
    let mut icons = Vec::new();
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
        } else if (6..=48).contains(&rw.max(rh))
            && rw.min(rh) >= 4
            && !text.iter().any(|t| overlap(*t, rect) > 0.5)
        {
            icons.push(Element {
                kind: ElementKind::Icon,
                rect,
            });
        }
    }
    let order = |e: &Element| (e.rect.1, e.rect.0);
    boxes.sort_by_key(order);
    icons.sort_by_key(order);
    boxes.extend(icons);
    boxes
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
        assert_eq!(kinds, vec![ElementKind::Box, ElementKind::Icon], "{found:?}");
        let (x, y, w, h) = found[0].rect;
        assert!((119..=121).contains(&x) && (39..=41).contains(&y), "{found:?}");
        assert!((198..=202).contains(&w) && (26..=30).contains(&h), "{found:?}");
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
        assert_eq!(field_box(&elements, (300, 60, 60, 16), "below"), Some((300, 100, 150, 26)));
        assert_eq!(field_box(&elements, (40, 150, 80, 16), "right_of"), None);
    }
}
