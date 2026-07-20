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
