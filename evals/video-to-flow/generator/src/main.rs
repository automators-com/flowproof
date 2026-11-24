//! `flowproof-video-fixture`: generates the video-to-flow eval corpus.
//!
//! For each case in `evals/video-to-flow/cases/`, drives the case's invented
//! fixture in headless Chrome, films it with the pointer drawn in, and
//! writes into `<out>/<case>/`:
//!
//! - `video.mp4`, muxed by the system ffmpeg;
//! - `actions.json`, every action with the millisecond it happened;
//! - `expected.flow.yaml`, the draft a perfect reading would give;
//! - `case.json`, what the scorers need to know about the case.
//!
//! The answer key comes from the script that caused the actions, so it
//! cannot drift from the picture. See `evals/video-to-flow/README.md`.

mod case;
mod film;
mod serve;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use headless_chrome::protocol::cdp::Emulation;
use headless_chrome::{Browser, LaunchOptions};
use serde_json::json;

use case::{Action, Case};
use film::Film;

/// Menu bar and taskbar heights in `chrome.html`, in CSS pixels.
const MENUBAR_PX: u32 = 26;
const TASKBAR_PX: u32 = 40;

struct Args {
    cases: Vec<PathBuf>,
    out: PathBuf,
    port: u16,
    ffmpeg: String,
}

fn args() -> anyhow::Result<Args> {
    let mut cases = Vec::new();
    let mut out = None;
    let mut port = 8765;
    let mut ffmpeg = std::env::var("FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" => out = it.next().map(PathBuf::from),
            "--port" => port = it.next().context("--port needs a number")?.parse()?,
            "--ffmpeg" => ffmpeg = it.next().context("--ffmpeg needs a path")?,
            "-h" | "--help" => {
                println!(
                    "flowproof-video-fixture <case.yaml | cases-dir>... --out <dir> \
                     [--port 8765] [--ffmpeg ffmpeg]"
                );
                std::process::exit(0);
            }
            _ => cases.push(PathBuf::from(arg)),
        }
    }
    let mut files = Vec::new();
    for path in cases {
        if path.is_dir() {
            let mut found: Vec<PathBuf> = std::fs::read_dir(&path)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
                .collect();
            found.sort();
            files.extend(found);
        } else {
            files.push(path);
        }
    }
    if files.is_empty() {
        bail!("name at least one case file or folder of cases");
    }
    Ok(Args {
        cases: files,
        out: out.context("--out <dir> is required")?,
        port,
        ffmpeg,
    })
}

fn main() -> anyhow::Result<()> {
    let args = args()?;
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixture");
    serve::start(fixtures, args.port)?;
    let browser = Browser::new(
        LaunchOptions::default_builder()
            .headless(true)
            .sandbox(false)
            .idle_browser_timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| anyhow!("launch options: {e}"))?,
    )
    .map_err(|e| anyhow!("starting Chrome: {e}"))?;
    for file in &args.cases {
        let case: Case = serde_yaml::from_str(&std::fs::read_to_string(file)?)
            .with_context(|| format!("reading {}", file.display()))?;
        let id = file
            .file_stem()
            .and_then(|s| s.to_str())
            .context("a case file name")?
            .to_string();
        eprintln!("generating {id}");
        let dir = args.out.join(&id);
        generate(&browser, &case, &id, &dir, &args)
            .with_context(|| format!("generating {}", file.display()))?;
    }
    Ok(())
}

fn generate(
    browser: &Browser,
    case: &Case,
    id: &str,
    dir: &Path,
    args: &Args,
) -> anyhow::Result<()> {
    let shots = dir.join("shots");
    if shots.exists() {
        std::fs::remove_dir_all(&shots)?;
    }
    std::fs::create_dir_all(&shots)?;
    let vp = case.viewport.or_default();
    let tab = browser
        .new_tab()
        .map_err(|e| anyhow!("opening a tab: {e}"))?;
    tab.call_method(Emulation::SetDeviceMetricsOverride {
        width: vp.width,
        height: vp.height,
        device_scale_factor: vp.scale,
        mobile: false,
        scale: None,
        screen_width: None,
        screen_height: None,
        position_x: None,
        position_y: None,
        dont_set_visible_size: None,
        screen_orientation: None,
        viewport: None,
        display_feature: None,
        device_posture: None,
    })
    .map_err(|e| anyhow!("setting the viewport: {e}"))?;
    let base = format!("http://127.0.0.1:{}", args.port);
    let start_url = format!("{base}/{}", case.fixture);
    let wrapped =
        case.menubar || case.taskbar || case.actions.iter().any(|a| matches!(a, Action::Mail(_)));
    let page = if wrapped {
        format!(
            "{base}/chrome.html?app={}&menubar={}&taskbar={}",
            case.fixture,
            u8::from(case.menubar),
            u8::from(case.taskbar)
        )
    } else {
        start_url.clone()
    };
    tab.navigate_to(&page)
        .and_then(|t| t.wait_until_navigated())
        .map_err(|e| anyhow!("loading {page}: {e}"))?;
    // The iframe loads after its parent.
    std::thread::sleep(std::time::Duration::from_millis(300));

    let pointer = case
        .pointer
        .then_some((vp.width as f64 * 0.62, vp.height as f64 * 0.55));
    let mut film = Film::new(&tab, &shots, vp.scale, case.taskbar, pointer);
    let mut steps: Vec<String> = Vec::new();
    let mut actions = Vec::new();
    let mut masked = Vec::new();
    let mut out_of_scope = 0;
    if case.go_to {
        steps.push(format!("Go to {start_url}"));
    }
    film.hold(case.gap_ms)?;
    for action in &case.actions {
        let (kind, step, at, end) = match action {
            Action::Click(selector) => {
                let t = film.find(selector, None, false)?;
                let at = film.click(t.x, t.y)?;
                ("click", Some(format!("Click \"{}\"", t.label)), at, at)
            }
            Action::Type(ty) => {
                let t = film.find(&ty.field, None, false)?;
                film.click(t.x, t.y)?;
                let (at, end) = film.keys(&ty.text, case.key_ms)?;
                let value = masked_value(&ty.text, ty.secret.as_deref(), &mut masked);
                (
                    "type",
                    Some(format!("Type {value} into the \"{}\" field", t.label)),
                    at,
                    end,
                )
            }
            Action::Pick(pick) => {
                let t = film.find(&pick.field, None, false)?;
                film.click(t.x, t.y)?;
                let (at, _) = film.keys(&pick.text, case.key_ms)?;
                film.hold(600)?;
                let entry = film.find("#suggestions li", Some(&pick.entry), false)?;
                let end = film.click(entry.x, entry.y)?;
                let step = format!("Type {} into the \"{}\" field", pick.entry, t.label);
                ("pick", Some(step), at, end)
            }
            Action::Press(key) => {
                let at = film.press(key)?;
                ("press", Some(format!("Press {key}")), at, at)
            }
            Action::Keys(keys) => {
                let label = film.focused_label()?;
                if label.is_empty() {
                    bail!("keys: no labelled field has the focus to type into");
                }
                let (at, end) = film.keys(&keys.text, case.key_ms)?;
                let value = masked_value(&keys.text, keys.secret.as_deref(), &mut masked);
                (
                    "keys",
                    Some(format!("Type {value} into the \"{label}\" field")),
                    at,
                    end,
                )
            }
            Action::Tab => {
                let at = film.press("Tab")?;
                ("tab", None, at, at)
            }
            Action::Wait(ms) => {
                let at = film.now_ms;
                film.hold(*ms)?;
                ("wait", None, at, film.now_ms)
            }
            Action::Mail(mail) => {
                let at = film.now_ms;
                film.script("window.__mail(true)")?;
                film.hold(800)?;
                let t = film.find(&mail.click, None, true)?;
                film.click(t.x, t.y)?;
                film.hold(mail.for_ms)?;
                film.script("window.__mail(false)")?;
                out_of_scope += 1;
                ("out_of_scope", None, at, film.now_ms)
            }
            Action::Slide => {
                let at = film.now_ms;
                film.script("window.__next()")?;
                ("slide", None, at, at)
            }
        };
        actions.push(json!({ "kind": kind, "step": step, "at_ms": at, "end_ms": end }));
        if let Some(step) = step {
            steps.push(step);
        }
        film.hold(case.gap_ms)?;
    }
    let duration_ms = film.now_ms;
    film.mux(&args.ffmpeg, &dir.join("video.mp4"))?;
    std::fs::remove_dir_all(&shots)?;
    let _ = tab.close(true);

    write_json(&dir.join("actions.json"), &json!(actions))?;
    let bands = json!({
        "top": if case.menubar { MENUBAR_PX } else { 0 },
        "bottom": if case.taskbar { TASKBAR_PX } else { 0 },
    });
    write_json(
        &dir.join("case.json"),
        &json!({
            "id": id,
            "name": case.name,
            "catches": case.catches,
            "fixture": case.fixture,
            "app": case.app,
            "start_url": case.go_to.then_some(&start_url),
            "duration_ms": duration_ms,
            "viewport": { "width": vp.width, "height": vp.height, "scale": vp.scale },
            "bands_css_px": bands,
            "expect": {
                "not_app": case.not_app,
                "out_of_scope": out_of_scope,
                "masked_literals": masked,
            },
        }),
    )?;
    let expected = dir.join("expected.flow.yaml");
    if case.not_app {
        if expected.exists() {
            std::fs::remove_file(&expected)?;
        }
    } else {
        std::fs::write(&expected, expected_flow(case, id, &steps))?;
    }
    Ok(())
}

/// What a draft writes for a typed value: `${NAME}` for a masked field,
/// whose literal is remembered so the scorer can check it never leaks.
fn masked_value(text: &str, secret: Option<&str>, masked: &mut Vec<String>) -> String {
    match secret {
        Some(name) => {
            masked.push(text.to_string());
            format!("${{{name}}}")
        }
        None => text.to_string(),
    }
}

/// The draft a perfect reading of the video would give, in the shape
/// `author-from-video` writes.
fn expected_flow(case: &Case, id: &str, steps: &[String]) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut yaml = format!(
        "# Expected draft for the {id} case, written by flowproof-video-fixture from\n\
         # the actions that made its video. Regenerate rather than edit it.\n\
         name: {}\napp: {}\nsteps:\n",
        quote(&case.name),
        quote(&case.app)
    );
    for step in steps {
        yaml.push_str(&format!("  - {}\n", quote(step)));
    }
    yaml
}

fn write_json(path: &Path, value: &serde_json::Value) -> anyhow::Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(value)? + "\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_expected_flow_quotes_every_step() {
        let case: Case = serde_yaml::from_str(
            "name: Order \"one\"\ncatches: x\nfixture: app.html\nactions: []\n",
        )
        .expect("case");
        let yaml = expected_flow(&case, "smoke", &["Click \"Sign in\"".into()]);
        assert!(yaml.contains("name: \"Order \\\"one\\\"\""), "{yaml}");
        assert!(yaml.contains("  - \"Click \\\"Sign in\\\"\""), "{yaml}");
    }
}
