//! Table cells on a character-grid screen: the host terminals that reach
//! vision through Citrix. A table there is a header line with rows of input
//! fields under it, one screen line per row. General OCR reads such a table
//! as column blocks, so a value loses the row it belongs to; a cell is
//! instead found the way a person finds it - the column under its header,
//! the row by a value it holds - and only that cell's box is read.

use flowproof_driver::{CellQuery, PixelRect};

use crate::vision::{union, OcrLine, OcrWord};

/// A located cell: its box on screen, where its first character sits, how
/// many characters wide it is, and the words the screen-size reading put
/// inside it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GridCell {
    pub rect: PixelRect,
    pub start: (i32, i32),
    pub chars: usize,
    pub words: Vec<String>,
}

/// Words on one screen line: their vertical centres are within half a
/// glyph height of each other. OCR splits a wide table row into several
/// lines at its gaps, so rows are rebuilt from words, not lines.
fn bands(lines: &[OcrLine]) -> Vec<Vec<OcrWord>> {
    let mut words: Vec<OcrWord> = lines.iter().flat_map(|l| l.words.clone()).collect();
    words.sort_by_key(|w| (w.rect.1 + w.rect.3 as i32 / 2, w.rect.0));
    let mut bands: Vec<Vec<OcrWord>> = Vec::new();
    for word in words {
        let centre = word.rect.1 + word.rect.3 as i32 / 2;
        match bands.last_mut() {
            Some(band) if (centre - middle(band)).abs() <= word.rect.3 as i32 / 2 => {
                band.push(word)
            }
            _ => bands.push(vec![word]),
        }
    }
    for band in &mut bands {
        band.sort_by_key(|w| w.rect.0);
    }
    bands
}

fn middle(band: &[OcrWord]) -> i32 {
    let sum: i32 = band.iter().map(|w| w.rect.1 + w.rect.3 as i32 / 2).sum();
    sum / band.len().max(1) as i32
}

fn same(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// The run of words in `band` reading `label`, as word indexes.
fn find_run(band: &[OcrWord], label: &str) -> Option<(usize, usize)> {
    let want = label.split_whitespace().count().max(1);
    (0..band.len()).find_map(|i| {
        let j = i + want - 1;
        let text: Vec<&str> = band.get(i..=j)?.iter().map(|w| w.text.as_str()).collect();
        same(&text.join(" "), label).then_some((i, j))
    })
}

/// The cell under the `column` header in the row holding `anchor` (and
/// every `also`). Rows are tried top to bottom; for each, the nearest
/// header line ABOVE it that names the column is its table's, so two
/// tables sharing a column name each resolve their own rows.
pub(crate) fn locate_cell(
    lines: &[OcrLine],
    query: &CellQuery,
    frame_width: u32,
) -> Option<GridCell> {
    let bands = bands(lines);
    for (r, row) in bands.iter().enumerate() {
        let holds = |label: &str| find_run(row, label).is_some();
        if !holds(&query.anchor) || !query.also.iter().all(|a| holds(a)) {
            continue;
        }
        let Some((header, (i, j))) = bands[..r]
            .iter()
            .rev()
            .find_map(|b| find_run(b, &query.column).map(|run| (b, run)))
        else {
            continue;
        };
        // A monospaced screen: one header word's width over its length is
        // the character pitch.
        let first = &header[i];
        let pitch = (first.rect.2 as f32 / first.text.chars().count().max(1) as f32).max(1.0);
        let half = (pitch / 2.0).round() as i32;
        // The column runs from its header to the next header on the line.
        let left = first.rect.0 - half;
        let right = header
            .get(j + 1)
            .map_or(frame_width as i32, |next| next.rect.0 - half);
        let top = row.iter().map(|w| w.rect.1).min().unwrap_or(0);
        let height = row.iter().map(|w| w.rect.3).max().unwrap_or(1);
        let pad = height as i32 / 4;
        let rect = (
            left,
            top - pad,
            (right - left).max(1) as u32,
            height + 2 * pad as u32,
        );
        let words: Vec<&OcrWord> = row
            .iter()
            .filter(|w| (left..right).contains(&(w.rect.0 + w.rect.2 as i32 / 2)))
            .collect();
        let rect = words.iter().fold(rect, |acc, w| union(acc, w.rect));
        return Some(GridCell {
            rect,
            start: (first.rect.0 + half, top + height as i32 / 2),
            chars: ((right - left) as f32 / pitch).round().max(1.0) as usize,
            words: words.iter().map(|w| w.text.clone()).collect(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line of a 10px-pitch monospaced screen: `text` starting at column
    /// `col` of screen line `row`, 20px per line, each word at its exact
    /// character position.
    fn at(row: i32, col: i32, text: &str) -> OcrLine {
        let chars: Vec<char> = text.chars().collect();
        let mut words = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == ' ' {
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && chars[i] != ' ' {
                i += 1;
            }
            words.push(OcrWord {
                text: chars[start..i].iter().collect(),
                rect: (
                    (col + start as i32) * 10,
                    row * 20,
                    (i - start) as u32 * 10,
                    16,
                ),
            });
        }
        let rect = (col * 10, row * 20, chars.len() as u32 * 10, 16);
        OcrLine {
            text: text.into(),
            rect,
            words,
        }
    }

    fn query(column: &str, anchor: &str) -> CellQuery {
        CellQuery {
            column: column.into(),
            anchor: anchor.into(),
            ..Default::default()
        }
    }

    /// Two tables share `K` and `LNr`; OCR split the upper table's rows at
    /// a wide gap, as it does on a real terminal screen.
    fn screen() -> Vec<OcrLine> {
        vec![
            at(1, 1, "K  LNr LA   Tarif"),
            at(2, 1, "1  001 BEL"),
            at(2, 13, "ZB30"),
            at(3, 1, "1  002 BEL"),
            at(3, 13, "BE70"),
            at(5, 1, "K  LNr Hinweis auf Entscheidung    Ent"),
            at(6, 1, "1  002 Kürzung nach GOZ §5         N"),
        ]
    }

    #[test]
    fn a_cell_is_found_by_its_header_and_a_value_in_its_row() {
        let cell = locate_cell(&screen(), &query("Tarif", "002"), 800).expect("cell");
        assert_eq!(cell.words, vec!["BE70"]);
        assert_eq!(
            cell.start,
            (135, 68),
            "the click lands on the first character"
        );
    }

    #[test]
    fn each_table_resolves_its_own_rows() {
        let cell = locate_cell(&screen(), &query("Ent", "002"), 800).expect("cell");
        assert_eq!(
            cell.words,
            vec!["N"],
            "the upper table has no Ent column above its 002"
        );
        let multi =
            locate_cell(&screen(), &query("Hinweis auf Entscheidung", "002"), 800).expect("cell");
        assert_eq!(multi.words.join(" "), "Kürzung nach GOZ §5");
        assert_eq!(multi.chars, 28, "the column runs to the next header");
    }

    #[test]
    fn an_empty_cell_is_still_a_cell() {
        let lines = vec![at(1, 1, "K  LNr Tarif"), at(2, 1, "1  001")];
        let cell = locate_cell(&lines, &query("Tarif", "001"), 800).expect("cell");
        assert!(cell.words.is_empty());
    }

    /// Reads the whole screen, and nothing in a crop: the cell must fall
    /// back to the words the screen-size reading put inside it.
    struct BlankCrops(Vec<OcrLine>);

    impl crate::vision::OcrEngine for BlankCrops {
        fn recognize(
            &mut self,
            frame: &image::RgbaImage,
        ) -> Result<Vec<OcrLine>, flowproof_driver::DriverError> {
            Ok(if frame.width() < 400 {
                Vec::new()
            } else {
                self.0.clone()
            })
        }
    }

    fn driver() -> crate::vision::VisionAppDriver<crate::vision::fake::FakeScreen, BlankCrops> {
        let frame = image::RgbaImage::from_pixel(800, 200, image::Rgba([255, 255, 255, 255]));
        crate::vision::VisionAppDriver::with_parts(
            crate::vision::fake::FakeScreen::with_frames(vec![frame]),
            BlankCrops(vec![
                at(1, 1, "K  LNr Tarif  LS"),
                at(2, 1, "1  001 ZB30   1"),
            ]),
        )
    }

    fn cell_selector(column: &str, anchor: &str) -> flowproof_driver::UiaSelector {
        flowproof_driver::UiaSelector {
            cell: Some(query(column, anchor)),
            ..Default::default()
        }
    }

    #[test]
    fn the_driver_reads_one_cell_not_its_neighbours() {
        use flowproof_driver::AppDriver;
        let mut d = driver();
        assert_eq!(
            d.read_text(&cell_selector("Tarif", "001")).expect("read"),
            "ZB30"
        );
        assert_eq!(d.read_text(&cell_selector("LS", "001")).expect("read"), "1");
    }

    #[test]
    fn typing_into_a_cell_empties_it_from_its_first_character() {
        use flowproof_driver::AppDriver;
        let mut d = driver();
        d.type_text(&cell_selector("Tarif", "001"), "ZB99")
            .expect("typed");
        let screen = d.screen_for_tests();
        assert_eq!(
            screen.clicks,
            vec![(85, 48)],
            "on the cell's first character"
        );
        let deletes = screen
            .keys
            .iter()
            .filter(|(k, m)| k == "Delete" && m.is_empty())
            .count();
        assert_eq!(deletes, 7, "once per character the cell holds");
        assert!(
            !screen.keys.iter().any(|(k, _)| k == "a"),
            "no select-all on a terminal"
        );
        assert_eq!(screen.typed, vec!["ZB99"]);
    }

    #[test]
    fn every_anchor_must_be_in_the_row() {
        let mut q = query("Tarif", "BEL");
        q.also = vec!["002".into()];
        let cell = locate_cell(&screen(), &q, 800).expect("cell");
        assert_eq!(cell.words, vec!["BE70"]);
        q.also = vec!["003".into()];
        assert_eq!(locate_cell(&screen(), &q, 800), None);
    }
}
