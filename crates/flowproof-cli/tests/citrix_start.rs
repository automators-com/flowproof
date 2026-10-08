//! A desktop-free recording/replay regression: only clicking the real pixel
//! target changes the screen. The menu check fails if that click misses.
use flowproof_adapters::vision::{fake::FakeScreen, OcrEngine, OcrLine, VisionAppDriver};
use flowproof_agent::{AgentError, Author, FlowSpec, ModelClient};
use flowproof_driver::DriverError;
use image::{Rgba, RgbaImage};

struct MenuOcr;
impl OcrEngine for MenuOcr {
    fn recognize(&mut self, frame: &RgbaImage) -> Result<Vec<OcrLine>, DriverError> {
        let mut lines = vec![OcrLine::new("ERGO - Desktop Viewer", (8, 4, 200, 20))];
        if frame.get_pixel(0, 100)[0] == 200 {
            lines.push(OcrLine::new("Programs", (40, 200, 100, 20)));
        }
        Ok(lines)
    }
}

struct MouseAuthor;
impl ModelClient for MouseAuthor {
    fn identity(&self) -> (String, String) {
        ("test".into(), "test".into())
    }
    fn complete(&mut self, _: &str, scene: &str) -> Result<String, AgentError> {
        assert!(scene.contains("text:Windows Start button"));
        assert!(scene.contains("visual_icon"));
        Ok(r#"{"action":"click","target":"text:Windows Start button"}"#.into())
    }
}

fn desktop(scale: u32, x: u32, has_icon: bool) -> FakeScreen {
    let mut before = RgbaImage::from_pixel(640 * scale, 360 * scale, Rgba([30, 30, 30, 255]));
    let (x, y, pane, gap) = (x * scale, 334 * scale, 7 * scale, 2 * scale);
    if has_icon {
        for (dx, dy) in [
            (0, 0),
            (pane + gap, 0),
            (0, pane + gap),
            (pane + gap, pane + gap),
        ] {
            for px in x + dx..x + dx + pane {
                for py in y + dy..y + dy + pane {
                    before.put_pixel(px, py, Rgba([240, 240, 240, 255]));
                }
            }
        }
    }
    let mut after = before.clone();
    after.put_pixel(0, 100, Rgba([200, 0, 0, 255]));
    let mut screen = FakeScreen::with_frames(vec![before, after]);
    screen.advance_on_click = Some((x as i32, y as i32, pane * 2 + gap, pane * 2 + gap));
    screen
}

#[test]
fn citrix_icon_recording_replays_at_a_new_scale_and_fails_when_the_icon_is_removed() {
    let spec = FlowSpec::parse("name: Citrix Start\napp: vision\nwindow: Desktop Viewer\nsteps:\n  - Click the Windows Start button using the mouse\n  - assert: page shows Programs\n").expect("Citrix regression fixture");
    let dir = std::env::temp_dir().join(format!("flowproof-citrix-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("Citrix regression fixture");
    let trace = dir.join("start.trace.jsonl");
    let mut driver = VisionAppDriver::with_parts(desktop(1, 12, true), MenuOcr);
    flowproof_agent::recorder::record_with_client(
        &spec,
        &mut driver,
        &trace,
        Author::Auto,
        Some(&mut MouseAuthor),
    )
    .expect("record observes the menu opened");
    let text = std::fs::read_to_string(&trace).expect("Citrix regression fixture");
    assert!(text.contains("Windows Start button"));
    assert!(text.contains(r#""provenance":"vision""#));
    assert!(!text.contains(r#""text":"ERGO - Desktop Viewer""#));
    let mut moved = VisionAppDriver::with_parts(desktop(2, 310, true), MenuOcr);
    let (report, _) =
        flowproof_replay::run_trace(&trace, &mut moved).expect("Citrix regression fixture");
    assert!(
        report.passed,
        "menu opened after geometry changed: {report:#?}"
    );
    let mut broken = VisionAppDriver::with_parts(desktop(1, 12, false), MenuOcr);
    let (report, _) =
        flowproof_replay::run_trace(&trace, &mut broken).expect("Citrix regression fixture");
    assert!(!report.passed, "removing the target must break replay");
    std::fs::remove_dir_all(dir).expect("Citrix regression fixture");
}
