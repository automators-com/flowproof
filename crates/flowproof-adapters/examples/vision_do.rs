//! Drive a live window through the vision adapter, one action per argument,
//! and print what the screen reads after - the action half of what
//! `vision_bench` scores, against a real app.
//!
//! ```text
//! cargo run --release -p flowproof-adapters --features vision --example vision_do -- \
//!     Calculator click:7 click:+ click:8 click:= expect:15
//! ```
//!
//! Actions: `click:<anchor>`, `dclick:<anchor>`, `rclick:<anchor>`,
//! `hover:<anchor>`, `type:<anchor>=<text>`, `typefocus:<text>`,
//! `press:<key>` (`press:Ctrl+a`), `drag:<from>><to>`, `expect:<text>` (the
//! screen must show it), `wait:<ms>`. Exits non-zero at the first failure.

#[cfg(windows)]
fn main() {
    use flowproof_adapters::VisionAppDriver;
    use flowproof_driver::{AppDriver, KeyMod, UiaSelector};

    let mut args = std::env::args().skip(1);
    let Some(window) = args.next() else {
        eprintln!("usage: vision_do <window title substring> <action>...");
        std::process::exit(2);
    };
    let anchor = |text: &str| UiaSelector {
        name: Some(text.to_string()),
        ..UiaSelector::default()
    };
    let mut driver = VisionAppDriver::new().expect("vision driver");
    driver
        .launch("", &window, std::time::Duration::from_secs(5))
        .expect("attach");
    for action in args {
        let started = std::time::Instant::now();
        let (verb, arg) = action.split_once(':').unwrap_or((action.as_str(), ""));
        let result = match verb {
            "click" => driver.invoke(&anchor(arg)),
            "dclick" => driver.double_click(&anchor(arg)),
            "rclick" => driver.context_click(&anchor(arg)),
            "hover" => driver.hover(&anchor(arg)),
            "type" => {
                let (field, text) = arg.split_once('=').unwrap_or((arg, ""));
                driver.type_text(&anchor(field), text)
            }
            "typefocus" => driver.type_focused(arg),
            "press" => {
                let mut parts: Vec<&str> = arg.split('+').collect();
                let key = parts.pop().unwrap_or_default();
                let mods: Vec<KeyMod> = parts
                    .iter()
                    .filter_map(|m| match m.to_lowercase().as_str() {
                        "ctrl" => Some(KeyMod::Ctrl),
                        "shift" => Some(KeyMod::Shift),
                        "alt" => Some(KeyMod::Alt),
                        _ => None,
                    })
                    .collect();
                driver.press_key(key, &mods)
            }
            "drag" => {
                let (from, to) = arg.split_once('>').unwrap_or((arg, ""));
                driver.drag(&anchor(from), &anchor(to))
            }
            "wait" => {
                std::thread::sleep(std::time::Duration::from_millis(arg.parse().unwrap_or(500)));
                Ok(())
            }
            "expect" => {
                let text = driver.surface_text().unwrap_or_default();
                if flowproof_driver::ocr_text_contains(&text, arg) {
                    Ok(())
                } else {
                    Err(flowproof_driver::DriverError::Uia(format!(
                        "screen does not show '{arg}'; it reads: {}",
                        text.replace('\n', " | ")
                    )))
                }
            }
            other => Err(flowproof_driver::DriverError::Uia(format!(
                "unknown action '{other}'"
            ))),
        };
        match result {
            Ok(()) => eprintln!("ok   {action}   [{:.1}s]", started.elapsed().as_secs_f64()),
            Err(e) => {
                eprintln!("FAIL {action}: {e}");
                std::process::exit(1);
            }
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("vision_do drives live windows, which exists only on Windows");
}
