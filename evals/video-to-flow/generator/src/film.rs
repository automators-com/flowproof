//! Filming a fixture: a virtual clock in 100ms ticks, a screenshot whenever
//! the page may have changed, the pointer drawn on top, and one PNG per
//! distinct picture with how long it stays on screen.
//!
//! Time is the film's own, not the wall clock's: a screenshot takes however
//! long it takes, and the frame is stamped with the tick it belongs to. So a
//! regenerated video is the same video, and an action's millisecond in
//! `actions.json` is exact.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context};
use headless_chrome::browser::tab::point::Point;
use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
use headless_chrome::Tab;
use serde::Deserialize;

pub const TICK_MS: u64 = 100;
/// Ticks a pointer takes to travel to its next target.
const TRAVEL_TICKS: u64 = 4;
/// Ticks the click rings stay drawn, as macOS's "show clicks" does.
const RING_TICKS: u64 = 2;

pub struct Film<'a> {
    tab: &'a Tab,
    dir: PathBuf,
    scale: f64,
    taskbar: bool,
    pointer: Option<(f64, f64)>,
    /// The film's clock.
    pub now_ms: u64,
    /// The page may differ from the last screenshot.
    dirty: bool,
    base: Option<image::RgbaImage>,
    last_second: Option<u64>,
    /// Written pictures and how long each shows, in ms.
    shots: Vec<(String, u64)>,
    last: Option<Vec<u8>>,
}

/// Where an element is, in CSS pixels of the whole page, and its label as
/// a step would name it.
#[derive(Debug, Deserialize)]
pub struct Target {
    pub x: f64,
    pub y: f64,
    pub label: String,
}

/// Finds `selector` in the app (inside `chrome.html`'s iframe when there is
/// one) or, with `top`, in the outer page; `text` narrows it to the match
/// whose text is exactly that. A field is named by its `<label>`, anything
/// else by its visible text, then its `aria-label`.
const FIND_JS: &str = r#"(() => {
  const frame = document.getElementById("app");
  let doc = document, ox = 0, oy = 0;
  if (!TOP && frame && frame.tagName === "IFRAME") {
    const r = frame.getBoundingClientRect();
    ox = r.left; oy = r.top; doc = frame.contentDocument;
  }
  const all = [...doc.querySelectorAll(SELECTOR)];
  const e = all.find((e) => TEXT === null || e.textContent.trim() === TEXT);
  if (!e) return JSON.stringify(null);
  const r = e.getBoundingClientRect();
  const label = e.id && doc.querySelector(`label[for="${e.id}"]`);
  const name = label ? label.textContent : (e.textContent.trim() || e.getAttribute("aria-label") || "");
  return JSON.stringify({ x: ox + r.left + r.width / 2, y: oy + r.top + r.height / 2, label: name.trim() });
})()"#;

/// The label of the field that has the focus, for typing with no click.
const FOCUS_JS: &str = r#"(() => {
  const frame = document.getElementById("app");
  const doc = frame && frame.tagName === "IFRAME" ? frame.contentDocument : document;
  const e = doc.activeElement;
  const label = e && e.id && doc.querySelector(`label[for="${e.id}"]`);
  return JSON.stringify(label ? label.textContent.trim() : "");
})()"#;

impl<'a> Film<'a> {
    pub fn new(
        tab: &'a Tab,
        dir: &Path,
        scale: f64,
        taskbar: bool,
        pointer: Option<(f64, f64)>,
    ) -> Self {
        Self {
            tab,
            dir: dir.to_path_buf(),
            scale,
            taskbar,
            pointer,
            now_ms: 0,
            dirty: true,
            base: None,
            last_second: None,
            shots: Vec::new(),
            last: None,
        }
    }

    fn eval_json<T: for<'de> Deserialize<'de>>(&self, js: &str) -> anyhow::Result<T> {
        let value = self
            .tab
            .evaluate(js, false)
            .map_err(|e| anyhow!("evaluating in the page: {e}"))?
            .value
            .ok_or_else(|| anyhow!("the page returned nothing"))?;
        let text = value
            .as_str()
            .ok_or_else(|| anyhow!("the page returned {value}"))?;
        Ok(serde_json::from_str(text)?)
    }

    /// Runs page script that changes what is shown.
    pub fn script(&mut self, js: &str) -> anyhow::Result<()> {
        self.tab
            .evaluate(js, false)
            .map_err(|e| anyhow!("running {js}: {e}"))?;
        self.dirty = true;
        Ok(())
    }

    pub fn find(&self, selector: &str, text: Option<&str>, top: bool) -> anyhow::Result<Target> {
        let js = FIND_JS
            .replace("TOP", if top { "true" } else { "false" })
            .replace("SELECTOR", &serde_json::to_string(selector)?)
            .replace("TEXT", &serde_json::to_string(&text)?);
        self.eval_json::<Option<Target>>(&js)?
            .ok_or_else(|| anyhow!("nothing on the page matches {selector}"))
    }

    pub fn focused_label(&self) -> anyhow::Result<String> {
        self.eval_json(FOCUS_JS)
    }

    /// One tick of film: a fresh screenshot if anything may have changed,
    /// the pointer on top, and the picture kept or the last one held longer.
    fn tick(&mut self, ring: bool) -> anyhow::Result<()> {
        if self.taskbar {
            let second = self.now_ms / 1000;
            if self.last_second != Some(second) {
                self.last_second = Some(second);
                self.script(&format!("window.__clock({})", self.now_ms))?;
            }
        }
        if self.dirty || self.base.is_none() {
            let png = self
                .tab
                .capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, true)
                .map_err(|e| anyhow!("taking a screenshot: {e}"))?;
            self.base = Some(image::load_from_memory(&png)?.to_rgba8());
            self.dirty = false;
        }
        let mut picture = self.base.clone().context("a screenshot")?;
        if let Some((x, y)) = self.pointer {
            let (x, y) = (
                (x * self.scale).round() as i32,
                (y * self.scale).round() as i32,
            );
            flowproof_driver::recording::draw_cursor(&mut picture, x, y, ring);
        }
        let raw = picture.as_raw();
        if self.last.as_deref() == Some(raw.as_slice()) {
            self.shots.last_mut().context("a shot")?.1 += TICK_MS;
        } else {
            let name = format!("{:06}.png", self.shots.len() + 1);
            picture.save(self.dir.join(&name))?;
            self.shots.push((name, TICK_MS));
            self.last = Some(raw.clone());
        }
        self.now_ms += TICK_MS;
        Ok(())
    }

    /// Film `ms` of whatever is on screen. A still screen is one picture
    /// shown for longer, unless the taskbar clock ticks over.
    pub fn hold(&mut self, ms: u64) -> anyhow::Result<()> {
        let end = self.now_ms + ms;
        while self.now_ms < end {
            if !self.taskbar && !self.dirty && self.base.is_some() {
                let rest = end - self.now_ms;
                self.tick(false)?;
                self.shots.last_mut().context("a shot")?.1 += rest.saturating_sub(TICK_MS);
                self.now_ms = end.max(self.now_ms);
                break;
            }
            self.tick(false)?;
        }
        Ok(())
    }

    /// Moves the pointer to `(x, y)` over a few ticks, as a hand would.
    pub fn travel(&mut self, x: f64, y: f64) -> anyhow::Result<()> {
        let Some((x0, y0)) = self.pointer else {
            return Ok(());
        };
        for step in 1..=TRAVEL_TICKS {
            let t = step as f64 / TRAVEL_TICKS as f64;
            let t = t * t * (3.0 - 2.0 * t);
            self.pointer = Some((x0 + (x - x0) * t, y0 + (y - y0) * t));
            if step == TRAVEL_TICKS {
                self.tab
                    .move_mouse_to_point(Point { x, y })
                    .map_err(|e| anyhow!("moving the mouse: {e}"))?;
                self.dirty = true;
            }
            self.tick(false)?;
        }
        Ok(())
    }

    /// Clicks at `(x, y)`; returns when the click landed.
    pub fn click(&mut self, x: f64, y: f64) -> anyhow::Result<u64> {
        self.travel(x, y)?;
        let at = self.now_ms;
        self.tab
            .click_point(Point { x, y })
            .map_err(|e| anyhow!("clicking: {e}"))?;
        self.dirty = true;
        for _ in 0..RING_TICKS {
            self.tick(true)?;
        }
        Ok(at)
    }

    /// Types `text` one key at a time; returns when the first and the last
    /// key landed.
    pub fn keys(&mut self, text: &str, key_ms: u64) -> anyhow::Result<(u64, u64)> {
        let first = self.now_ms;
        let mut last = first;
        for c in text.chars() {
            last = self.now_ms;
            self.tab
                .send_character(&c.to_string())
                .map_err(|e| anyhow!("typing: {e}"))?;
            self.dirty = true;
            self.hold(key_ms.max(TICK_MS))?;
        }
        Ok((first, last))
    }

    pub fn press(&mut self, key: &str) -> anyhow::Result<u64> {
        let at = self.now_ms;
        self.tab
            .press_key(key)
            .map_err(|e| anyhow!("pressing {key}: {e}"))?;
        self.dirty = true;
        self.tick(false)?;
        Ok(at)
    }

    /// Muxes the pictures into `out` with the system ffmpeg (the one that
    /// ships with flowproof decodes only), at 10 frames a second.
    pub fn mux(&self, ffmpeg: &str, out: &Path) -> anyhow::Result<()> {
        let mut list = String::new();
        for (name, ms) in &self.shots {
            list.push_str(&format!(
                "file '{name}'\nduration {:.3}\n",
                *ms as f64 / 1000.0
            ));
        }
        // The concat demuxer drops the last duration unless the last file
        // is named once more.
        if let Some((name, _)) = self.shots.last() {
            list.push_str(&format!("file '{name}'\n"));
        }
        let list_path = self.dir.join("frames.txt");
        std::fs::write(&list_path, list)?;
        let status = Command::new(ffmpeg)
            .args(["-v", "error", "-y", "-f", "concat", "-safe", "0", "-i"])
            .arg(&list_path)
            .args([
                "-vf",
                "fps=10,format=yuv420p",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "20",
                "-movflags",
                "+faststart",
            ])
            .arg(out)
            .status()
            .with_context(|| format!("running {ffmpeg}"))?;
        if !status.success() {
            bail!("{ffmpeg} could not mux {}", out.display());
        }
        Ok(())
    }
}
