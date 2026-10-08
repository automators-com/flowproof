//! Record once, replay against fresh pixels and window origins; no OCR models needed.
use std::{cell::RefCell, rc::Rc, time::Duration};

use flowproof_adapters::vision::{fake::FakeOcr, OcrLine, VisionAppDriver, VisionScreen};
use flowproof_agent::{record, FlowSpec};
use flowproof_driver::{DriverError, KeyMod, RecordingDetail, RecordingOptions};
use flowproof_replay::{run_trace_with_options, StepStatus};
use image::{Rgba, RgbaImage};

const LABEL: &str = "Windows Start button";

#[derive(Clone)]
struct Screen {
    frame: RgbaImage,
    origin: (i32, i32),
    clicks: Rc<RefCell<Vec<(i32, i32)>>>,
    keys: Rc<RefCell<Vec<String>>>,
}

impl VisionScreen for Screen {
    fn attach(&mut self, _: &str, _: Duration) -> Result<(), DriverError> {
        Ok(())
    }
    fn frame(&mut self) -> Result<RgbaImage, DriverError> {
        Ok(self.frame.clone())
    }
    fn click(&mut self, x: i32, y: i32) -> Result<(), DriverError> {
        self.clicks
            .borrow_mut()
            .push((x + self.origin.0, y + self.origin.1));
        Ok(())
    }
    fn type_text(&mut self, _: &str) -> Result<(), DriverError> {
        panic!("unexpected typing")
    }
    fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError> {
        assert!(modifiers.is_empty());
        self.keys.borrow_mut().push(key.into());
        Ok(())
    }
    fn window_origin(&mut self) -> Result<(i32, i32), DriverError> {
        Ok(self.origin)
    }
    fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
        Ok((3840, 2160))
    }
}

fn screen(pane: u32, gap: u32, icons: &[u32], origin: (i32, i32)) -> Screen {
    let mut frame = RgbaImage::from_pixel(640 * pane / 7, 360 * pane / 7, Rgba([30, 30, 30, 255]));
    let y = frame.height() - (2 * pane + gap) - 10;
    for &x in icons {
        for (dx, dy) in [
            (0, 0),
            (pane + gap, 0),
            (0, pane + gap),
            (pane + gap, pane + gap),
        ] {
            for px in x + dx..x + dx + pane {
                for py in y + dy..y + dy + pane {
                    frame.put_pixel(px, py, Rgba([0, 160, 230, 255]));
                }
            }
        }
    }
    Screen {
        frame,
        origin,
        clicks: Rc::default(),
        keys: Rc::default(),
    }
}

#[test]
fn recorded_start_target_replays_at_new_scales_and_origins_or_fails_without_input() {
    let dir = std::env::temp_dir().join(format!("flowproof-start-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("fixture");
    let trace = dir.join("start.trace.jsonl");
    let spec = FlowSpec::parse(&format!(
        "name: Open Start\napp: vision\nwindow: Desktop Viewer\nsteps:\n  - Press the \"{LABEL}\" button\n  - Press Meta\n"
    )).expect("fixture");
    let mut driver = VisionAppDriver::with_parts(screen(7, 2, &[12], (80, 90)), FakeOcr::default());
    record(&spec, &mut driver, &trace).expect("records pixel target and standalone Windows key");
    let (header, mut steps) = flowproof_replay::load_trace(&trace).expect("fixture");
    assert_eq!(steps[0].selectors[0].payload["text"], LABEL);
    // Keep the test's unavailable-target wait immediate, without changing production defaults.
    for condition in &mut steps[0].sync.pre {
        if let flowproof_trace::format::Condition::ElementExists { timeout_ms, .. } = condition {
            *timeout_ms = 0;
        }
    }
    let mut lines = vec![serde_json::to_string(&header).expect("fixture")];
    lines.extend(
        steps
            .iter()
            .map(|step| serde_json::to_string(step).expect("fixture")),
    );
    std::fs::write(&trace, lines.join("\n") + "\n").expect("fixture");

    let options = RecordingOptions {
        detail: RecordingDetail::Off,
        ..RecordingOptions::default()
    };
    // Downscale, fractional scale, 2x/3x, centered taskbar, and moved host window.
    for (pane, gap, x, origin) in [
        (4, 1, 8, (500, 240)),
        (10, 3, 300, (90, 10)),
        (14, 4, 24, (900, 400)),
        (21, 6, 500, (0, 0)),
    ] {
        let s = screen(pane, gap, &[x], origin);
        let y = s.frame.height() - (2 * pane + gap) - 10;
        let mut driver = VisionAppDriver::with_parts(s.clone(), FakeOcr::default());
        let (report, _) = run_trace_with_options(&trace, &mut driver, options).expect("fixture");
        assert!(report.passed && !report.degraded, "{report:#?}");
        assert_eq!(
            *s.clicks.borrow(),
            vec![(
                origin.0 + x as i32 + (2 * pane + gap) as i32 / 2,
                origin.1 + y as i32 + (2 * pane + gap) as i32 / 2
            )]
        );
        assert_eq!(*s.keys.borrow(), vec!["Meta"]);
    }
    for icons in [vec![], vec![12, 300]] {
        let s = screen(7, 2, &icons, (500, 240));
        // Even OCR text with the reserved label must not supply a fallback click.
        let mut driver = VisionAppDriver::with_parts(
            s.clone(),
            FakeOcr::with_lines(vec![OcrLine::new(LABEL, (8, 4, 200, 20))]),
        );
        let (report, _) = run_trace_with_options(&trace, &mut driver, options)
            .expect("unavailable pixels produce a report, not a driver crash");
        assert!(!report.passed, "{report:#?}");
        assert_eq!(report.steps[0].status, StepStatus::Failed);
        assert!(report.steps[0]
            .detail
            .as_deref()
            .expect("fixture")
            .contains("did not appear"));
        assert_eq!(report.steps[1].status, StepStatus::Skipped);
        assert!(s.clicks.borrow().is_empty() && s.keys.borrow().is_empty());
    }
    std::fs::remove_dir_all(dir).expect("fixture");
}
