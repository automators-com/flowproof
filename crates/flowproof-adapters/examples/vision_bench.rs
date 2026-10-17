//! Grounding benchmark for the vision adapter against live windows.
//!
//! ```text
//! cargo run --release -p flowproof-adapters --features vision --example vision_bench -- \
//!     Calculator Notepad Paint [--json] [--misses]
//! ```
//!
//! Each argument is a window title substring. The window is raised, vision
//! reads it the way a run would, and every interactive control its UIA
//! tree reports is asked for by name - see `flowproof_adapters::vision_bench`
//! for what is scored. Reads the screen and moves no input but the raise.

#[cfg(windows)]
fn main() {
    use flowproof_adapters::vision_bench::{answer_key, score, GroundTruth, Outcome};
    use flowproof_adapters::VisionAppDriver;
    use flowproof_driver::AppDriver;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let misses = args.iter().any(|a| a == "--misses");
    let dump = args.iter().any(|a| a == "--dump");
    let windows: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if windows.is_empty() {
        eprintln!("usage: vision_bench <window title substring>... [--json] [--misses]");
        std::process::exit(2);
    }
    let mut driver = VisionAppDriver::new().expect("vision driver");
    let mut reports = Vec::new();
    for title in windows {
        let Some(info) = flowproof_driver::window::find_window(title).expect("window search")
        else {
            eprintln!("{title}: no such window - skipped");
            continue;
        };
        if let Err(e) = driver.launch("", title, std::time::Duration::from_secs(5)) {
            eprintln!("{title}: could not attach ({e}) - skipped");
            continue;
        }
        let controls = flowproof_driver::uia_scan::window_controls(info.hwnd())
            .expect("UIA scan")
            .into_iter()
            // A disabled control is not a target a step can use.
            .filter(|c| c.is_interactive() && c.enabled)
            .map(|c| GroundTruth {
                name: c.name,
                control_type: c.control_type,
                rect: c.rect,
            })
            .collect();
        let started = std::time::Instant::now();
        let (ox, oy) = driver.window_origin().expect("origin");
        let (w, h) = driver
            .capture()
            .ok()
            .flatten()
            .map_or((info.rect.2, info.rect.3), |f| (f.width(), f.height()));
        let key = answer_key(controls, (ox, oy, w, h));
        let offered = driver.targets().expect("targets");
        if dump {
            for t in &offered {
                eprintln!("    read '{}' at {:?}", t.text, t.rect);
            }
            if let Ok(Some(frame)) = driver.capture() {
                let text: Vec<_> = offered.iter().map(|t| t.rect).collect();
                for (symbol, rect) in
                    flowproof_adapters::vision_elements::find_symbols(&frame, &text)
                {
                    eprintln!("    drawn {symbol:?} at {rect:?}");
                }
                let path = std::env::temp_dir().join("vision_bench_frame.png");
                if frame.save(&path).is_ok() {
                    eprintln!("    frame saved to {}", path.display());
                }
            }
        }
        if args.iter().any(|a| a == "--scene") {
            let scene = driver.scene().ok().flatten().unwrap_or_default();
            check_scene_ordinals(&scene, &info.title, |name, n| {
                driver.locate_point(name, n, "inside").ok().flatten()
            });
        }
        let targets: Vec<(i32, i32)> = offered
            .iter()
            .map(|t| (t.point.0 + ox, t.point.1 + oy))
            .collect();
        let report = score(&info.title, &key, &targets, |name, nth, relation| {
            driver
                .locate_point(name, nth, relation)
                .ok()
                .flatten()
                .map(|(x, y)| (x + ox, y + oy))
        });
        eprintln!(
            "{}   [{:.1}s]",
            report.summary(),
            started.elapsed().as_secs_f64()
        );
        if misses {
            for case in report.cases.iter().filter(|c| c.outcome != Outcome::Hit) {
                eprintln!(
                    "    {:?} {} '{}' at {:?}{}{}",
                    case.outcome,
                    case.control_type,
                    case.name,
                    case.rect,
                    if case.covered { " (covered)" } else { "" },
                    case.point
                        .map_or(String::new(), |p| format!(" -> clicked {p:?}"))
                );
            }
        }
        reports.push(report);
    }
    let (controls, covered, named, hits, wrong) = reports.iter().fold((0, 0, 0, 0, 0), |a, r| {
        (
            a.0 + r.controls,
            a.1 + r.covered,
            a.2 + r.named,
            a.3 + r.hits,
            a.4 + r.wrong_control,
        )
    });
    let pct = |n: usize, d: usize| {
        if d == 0 {
            0.0
        } else {
            100.0 * n as f64 / d as f64
        }
    };
    eprintln!(
        "TOTAL: coverage {covered}/{controls} ({:.0}%), name accuracy {hits}/{named} ({:.0}%), wrong clicks {wrong}",
        pct(covered, controls),
        pct(hits, named)
    );
    if json {
        println!("{}", serde_json::to_string_pretty(&reports).expect("json"));
    }
}

/// `--scene`: every entry the recording scene offers by ordinal
/// (`nth:2:text:Home`) must replay onto that same entry - the scene's count
/// and resolution's agreeing on a live screen, not only in a unit test.
#[cfg(windows)]
fn check_scene_ordinals(
    scene: &str,
    title: &str,
    mut locate: impl FnMut(&str, u32) -> Option<(i32, i32)>,
) {
    let entries: Vec<serde_json::Value> = serde_json::from_str(scene).unwrap_or_default();
    let (mut checked, mut agreed) = (0, 0);
    for entry in &entries {
        let Some(token) = entry["target"].as_str() else {
            continue;
        };
        let Some((n, inner)) = token
            .strip_prefix("nth:")
            .and_then(|rest| rest.split_once(':'))
            .and_then(|(n, inner)| Some((n.parse::<u32>().ok()?, inner)))
        else {
            continue;
        };
        let name = inner.strip_prefix("text:").unwrap_or(inner);
        let rect: Vec<i64> = entry["rect"]
            .as_array()
            .map(|r| r.iter().filter_map(serde_json::Value::as_i64).collect())
            .unwrap_or_default();
        let [x, y, w, h] = rect[..] else {
            continue;
        };
        checked += 1;
        let landed = locate(name, n);
        let inside = landed.is_some_and(|(px, py)| {
            let (px, py) = (i64::from(px), i64::from(py));
            px >= x && py >= y && px < x + w && py < y + h
        });
        if inside {
            agreed += 1;
        } else {
            eprintln!("    scene {token} at {rect:?} replays to {landed:?}");
        }
    }
    eprintln!("{title}: scene ordinals replay to their own entry {agreed}/{checked}");
}

#[cfg(not(windows))]
fn main() {
    eprintln!("vision_bench drives live windows, which exists only on Windows");
}
