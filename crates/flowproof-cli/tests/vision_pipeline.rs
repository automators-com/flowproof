//! The vision pipeline with REAL OCR and no real window: synthetic
//! screens are rendered to pixels, the production `OcrsEngine` reads them
//! back, and the full spec → rules → record → trace → replay pipeline
//! runs against a scripted screen. This is what CI proves on every
//! platform with a desktop-free runner; `vision_notepad_e2e` adds the
//! real capture + SendInput proof on windows.
//!
//! Gated on FLOWPROOF_E2E=1: the first run downloads the ocrs models
//! (~10 MB, cached under ~/.cache/flowproof/ocrs).

use ab_glyph::{FontRef, PxScale};
use flowproof_adapters::vision::fake::FakeScreen;
use flowproof_adapters::vision::{OcrsEngine, VisionAppDriver};
use flowproof_agent::FlowSpec;
use flowproof_driver::PixelRect;
use image::{Rgba, RgbaImage};

const SPEC: &str = "\
name: Post order
app: vision
window: Fake Terminal
steps:
  - Type ZOR into the \"Order Type\" field
  - Press the \"Submit\" button
  - assert: page shows saved
";

fn font_bytes() -> Option<Vec<u8>> {
    let candidates = [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "C:\\Windows\\Fonts\\arial.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
    ];
    candidates.iter().find_map(|path| std::fs::read(path).ok())
}

/// Render a screen: white background, black text lines at fixed spots —
/// a terminal the OCR engine must actually read.
fn render(font: &FontRef<'_>, lines: &[(&str, i32, i32)]) -> RgbaImage {
    let mut frame = RgbaImage::from_pixel(640, 360, Rgba([255, 255, 255, 255]));
    for (text, x, y) in lines {
        imageproc::drawing::draw_text_mut(
            &mut frame,
            Rgba([10, 10, 10, 255]),
            *x,
            *y,
            PxScale::from(32.0),
            font,
            text,
        );
    }
    frame
}

fn screens(font: &FontRef<'_>) -> FakeScreen {
    let before = render(font, &[("Order Type", 40, 60), ("Submit", 40, 180)]);
    let after = render(
        font,
        &[
            ("Order Type", 40, 60),
            ("Submit", 40, 180),
            ("Order 4711 saved", 40, 260),
        ],
    );
    let mut screen = FakeScreen::with_frames(vec![before, after]);
    // A click anywhere on the rendered Submit line posts the order.
    screen.advance_on_click = Some((30, 170, 200, 50));
    screen
}

#[test]
fn real_ocr_records_and_replays_a_vision_flow() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping vision pipeline test: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let Some(bytes) = font_bytes() else {
        eprintln!("skipping vision pipeline test: no system TTF font found to render screens");
        return;
    };
    let font = FontRef::try_from_slice(&bytes).expect("font parses");

    let dir = std::env::temp_dir().join("flowproof-vision-pipeline");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("order.trace.jsonl");

    let spec = FlowSpec::parse(SPEC).expect("spec parses");

    // Record: real OCR over the rendered frames.
    let mut driver = VisionAppDriver::with_parts(
        screens(&font),
        OcrsEngine::new().expect("OCR models load (downloaded on first use)"),
    );
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("rules author the whole flow");

    // The trace speaks the vision provenance end to end.
    let trace = std::fs::read_to_string(&trace_path).expect("trace written");
    let header = trace.lines().next().expect("header");
    assert!(
        header.contains("\"adapter\":\"vision\""),
        "header: {header}"
    );
    assert!(
        header.contains("Fake Terminal"),
        "window travels in the header: {header}"
    );
    assert!(
        trace.contains(r#""provenance":"vision""#),
        "selectors carry vision provenance"
    );
    assert!(
        trace.contains(r#""relation":"right_of""#),
        "typing anchors record their spatial relation"
    );

    // Replay on a fresh screen (frames reset), fresh engine (cached models).
    let mut driver = VisionAppDriver::with_parts(
        screens(&font),
        OcrsEngine::new().expect("OCR models load from cache"),
    );
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    for step in &report.steps {
        eprintln!("{:?} {} {}", step.status, step.id, step.intent);
    }
    assert!(report.passed, "vision flow must replay: {report:#?}");
    assert!(
        !report.degraded,
        "primary selectors must match: {report:#?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// #69, the proof Fable asked for: a 3x3 digit grid where EVERY digit
/// resolves uniquely, with real OCR supplying the word boxes.
///
/// A grid like this is exactly what line-level addressing cannot reach.
/// The engine reads each row as ONE line ("1 2 3"), so `Click "2"` has no
/// line to match: not exactly, and not by prefix either, since the row
/// starts with "1". Every cell in the middle and right columns was
/// unaddressable. With word matching each cell is its own anchor, and the
/// click has to land in that cell rather than merely somewhere on the row.
#[test]
fn every_digit_in_a_grid_resolves_to_its_own_cell() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping vision digit-grid test: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let Some(bytes) = font_bytes() else {
        eprintln!("skipping vision digit-grid test: no system TTF font found");
        return;
    };
    let font = FontRef::try_from_slice(&bytes).expect("font parses");

    // Three rows, three digits each, widely spaced so a wrong cell is an
    // unmistakable miss rather than a rounding error.
    let cell_w = 120;
    let cell_h = 100;
    let (x0, y0) = (60, 40);
    let mut placed = Vec::new();
    for (row, digits) in [["1", "2", "3"], ["4", "5", "6"], ["7", "8", "9"]]
        .iter()
        .enumerate()
    {
        for (col, digit) in digits.iter().enumerate() {
            placed.push((
                *digit,
                x0 + (col as i32 * cell_w),
                y0 + (row as i32 * cell_h),
            ));
        }
    }
    let frame = render(&font, &placed);

    // ONE OCR pass. Real OCR in a debug build costs seconds per frame,
    // and one pass is all this test needs to prove: that the production
    // engine reports a box PER WORD. Which anchor wins which box is the
    // matcher's job, and the fake-driven unit tests cover that exhaustively.
    let mut engine = OcrsEngine::new().expect("ocrs engine");
    let lines = flowproof_adapters::vision::OcrEngine::recognize(&mut engine, &frame)
        .expect("OCR reads the grid");
    for line in &lines {
        eprintln!("line {:?} at {:?}", line.text, line.rect);
        for word in &line.words {
            eprintln!("    word {:?} at {:?}", word.text, word.rect);
        }
    }

    // Each row OCRs as ONE line, which is exactly why line-level
    // addressing could not reach the middle and right columns: `Click "2"`
    // matches no line exactly, and no line by prefix either, since every
    // row starts with a different digit.
    let mut boxes: Vec<(&str, PixelRect)> = Vec::new();
    for (digit, cell_x, cell_y) in &placed {
        let matches: Vec<_> = lines
            .iter()
            .flat_map(|l| l.words.iter())
            .filter(|w| w.text.trim() == *digit)
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "digit {digit} must be exactly one word, got {matches:?}"
        );
        let (rx, ry, rw, rh) = matches[0].rect;
        let (cx, cy) = (rx + rw as i32 / 2, ry + rh as i32 / 2);
        // The glyph hangs below its baseline y, so the row band is
        // generous; the column band is tight, since columns are what a
        // line-level match could never tell apart.
        assert!(
            cx >= *cell_x - 20 && cx < cell_x + cell_w - 20,
            "digit {digit} sits at x={cx}, outside its column at {cell_x}"
        );
        assert!(
            cy >= *cell_y - 20 && cy < cell_y + cell_h,
            "digit {digit} sits at y={cy}, outside its row at {cell_y}"
        );
        boxes.push((digit, matches[0].rect));
    }

    // "Every digit resolves UNIQUELY": nine anchors, nine distinct boxes.
    for (i, (digit, rect)) in boxes.iter().enumerate() {
        for (other, other_rect) in boxes.iter().skip(i + 1) {
            assert_ne!(
                rect, other_rect,
                "digits {digit} and {other} share the box {rect:?}"
            );
        }
    }
}
