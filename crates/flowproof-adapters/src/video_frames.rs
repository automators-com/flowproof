//! The frames of a screen recording, decoded by the browser web flows
//! already use, so drafting from a video ships no video tooling of its own.
//!
//! Chrome opens the file directly (a media document), the page is pinned to
//! the video's size, and the video is stepped through: every sample is a
//! small thumbnail, a sample is kept when it differs enough from the last
//! kept one, and only the kept moments are captured again at full size as
//! JPEG for the model. A recording is mostly a still screen with short
//! bursts of change, so this keeps the moments a person acted and drops the
//! rest.
//!
//! The same thumbnails decide what is cut from the frames: a band at an edge
//! of the screen that stays still the whole recording (a taskbar, a Dock, a
//! menu bar) is trimmed before anything reaches the model. Only edges are
//! trimmed, and never more than a tenth of the screen from each: finding
//! "the app's window" anywhere on the screen would sooner or later cut away
//! a label the model needs.

use std::path::Path;

use headless_chrome::protocol::cdp::{Emulation, Page};
use headless_chrome::Tab;

use crate::AdapterError;

/// What the browser read from the file before any frame was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoInfo {
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    /// The part of the screen the frames keep, in the frames' own pixels,
    /// when still bands were trimmed from its edges.
    pub kept: Option<Area>,
}

/// A rectangle, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One kept moment.
pub struct Frame {
    pub at_ms: u64,
    pub jpeg: Vec<u8>,
}

pub struct FrameOptions {
    /// How often the video is sampled for change.
    pub every_ms: u64,
    /// The most frames kept; the largest changes win beyond it.
    pub max_frames: usize,
    /// Frames are scaled down to this width, keeping the aspect ratio.
    pub max_width: u32,
}

impl Default for FrameOptions {
    fn default() -> Self {
        Self {
            every_ms: 500,
            max_frames: 60,
            max_width: 1280,
        }
    }
}

/// A long recording is sampled less often rather than without end.
const MAX_SAMPLES: u64 = 720;
/// Thumbnail width the change test compares at.
const THUMB_WIDTH: f64 = 128.0;
/// Mean brightness difference (0..1) between a sample and the last kept
/// frame that counts as the screen having changed. Low on purpose: typing a
/// short value into one field moves only a sliver of the screen.
const CHANGE: f64 = 0.003;
const JPEG_QUALITY: u32 = 80;
/// A thumbnail pixel this much brighter or darker counts as changed.
const PIXEL_CHANGE: u8 = 24;
/// A row or column that changed this few times in the whole recording is
/// still: a clock ticking over, a pointer passing once.
const STILL_CHANGES: usize = 3;
/// The most trimmed from any one edge, as a share of the screen.
const EDGE_LIMIT: f64 = 0.10;

/// Duration and size only; nothing is captured.
pub fn probe(video: &Path) -> Result<VideoInfo, AdapterError> {
    let browser = crate::web::launch_browser(&[])?;
    let tab = crate::web::private_flow_tab(&browser)
        .map_err(|e| AdapterError::Web(format!("opening a tab for the video: {e}")))?;
    open(&tab, video)
}

/// The changed moments of `video`, in order, with the first and last frame
/// always among them. `on_sample(done, total)` reports the slow part.
pub fn extract(
    video: &Path,
    opts: &FrameOptions,
    on_sample: &mut dyn FnMut(usize, usize),
) -> Result<(VideoInfo, Vec<Frame>), AdapterError> {
    let browser = crate::web::launch_browser(&[])?;
    let tab = crate::web::private_flow_tab(&browser)
        .map_err(|e| AdapterError::Web(format!("opening a tab for the video: {e}")))?;
    let mut info = open(&tab, video)?;
    let width = info.width.min(opts.max_width).max(1);
    let height = ((info.height as f64) * (width as f64) / (info.width as f64))
        .round()
        .max(1.0) as u32;
    tab.call_method(Emulation::SetDeviceMetricsOverride {
        width,
        height,
        device_scale_factor: 1.0,
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
    .map_err(|e| AdapterError::Web(format!("sizing the page to the video: {e}")))?;

    let times = sample_times(info.duration_ms, opts.every_ms);
    let mut thumbs = Vec::with_capacity(times.len());
    let mut thumb_size = (0, 0);
    for (i, &at) in times.iter().enumerate() {
        on_sample(i, times.len());
        seek(&tab, at)?;
        let (thumb, w, h) = thumbnail(&tab, width, height)?;
        thumb_size = (w, h);
        thumbs.push(thumb);
    }
    // Thumbnail pixels to page pixels, per axis.
    info.kept = still_edges(&thumbs, thumb_size.0, thumb_size.1).map(|k| {
        let sx = width as f64 / thumb_size.0 as f64;
        let sy = height as f64 / thumb_size.1 as f64;
        Area {
            x: (k.x as f64 * sx).round() as u32,
            y: (k.y as f64 * sy).round() as u32,
            width: (k.width as f64 * sx).round() as u32,
            height: (k.height as f64 * sy).round() as u32,
        }
    });
    let clip = info.kept.map(|k| Page::Viewport {
        x: k.x as f64,
        y: k.y as f64,
        width: k.width as f64,
        height: k.height as f64,
        scale: 1.0,
    });
    let mut frames = Vec::new();
    for i in select_changes(&thumbs, opts.max_frames) {
        seek(&tab, times[i])?;
        let jpeg = tab
            .capture_screenshot(
                Page::CaptureScreenshotFormatOption::Jpeg,
                Some(JPEG_QUALITY),
                clip.clone(),
                true,
            )
            .map_err(|e| AdapterError::Web(format!("capturing a frame: {e}")))?;
        frames.push(Frame {
            at_ms: times[i],
            jpeg,
        });
    }
    Ok((info, frames))
}

/// Load the file and read what it is; a format the browser cannot play is
/// said plainly, with the way out.
fn open(tab: &Tab, video: &Path) -> Result<VideoInfo, AdapterError> {
    let url = file_url(video)?;
    tab.navigate_to(&url)
        .and_then(|t| t.wait_until_navigated())
        .map_err(|e| AdapterError::Web(format!("opening {}: {e}", video.display())))?;
    let reply = tab
        .evaluate(OPEN_JS, true)
        .map_err(|e| AdapterError::Web(format!("reading {}: {e}", video.display())))?
        .value
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let read: serde_json::Value = serde_json::from_str(&reply).unwrap_or_default();
    if let Some(reason) = read["error"].as_str() {
        return Err(AdapterError::Web(format!(
            "the browser could not play {} ({reason}). Export it as an H.264 .mp4 and \
             try again; some HEVC recordings only play on machines that decode them \
             in hardware",
            video.display()
        )));
    }
    let duration = read["duration"].as_f64().unwrap_or(0.0);
    Ok(VideoInfo {
        duration_ms: (duration * 1000.0).round() as u64,
        width: read["width"].as_u64().unwrap_or(0) as u32,
        height: read["height"].as_u64().unwrap_or(0) as u32,
        kept: None,
    })
}

/// Runs in Chrome's media document for the file. A recording made in a
/// browser (MediaRecorder webm) often states no duration until it has been
/// read to the end, so the end is visited once to learn it.
const OPEN_JS: &str = r#"(async () => {
    const v = document.querySelector('video');
    if (!v) { return JSON.stringify({ error: 'not a video' }); }
    const settle = (event) => new Promise(ok => {
        v.addEventListener(event, ok, { once: true });
        v.addEventListener('error', ok, { once: true });
        setTimeout(ok, 10000);
    });
    if (v.readyState < 1) { await settle('loadedmetadata'); }
    if (v.error) { return JSON.stringify({ error: 'decode error ' + v.error.code }); }
    if (!v.videoWidth) { return JSON.stringify({ error: 'no picture track' }); }
    v.pause();
    v.muted = true;
    v.controls = false;
    if (!isFinite(v.duration)) {
        const seeked = settle('seeked');
        v.currentTime = 1e9;
        await seeked;
        const back = settle('seeked');
        v.currentTime = 0;
        await back;
    }
    if (!isFinite(v.duration) || v.duration <= 0) { return JSON.stringify({ error: 'no length' }); }
    document.documentElement.style.cssText = 'margin:0;background:#000;overflow:hidden';
    document.body.style.cssText = 'margin:0;background:#000';
    v.style.cssText = 'position:fixed;left:0;top:0;width:100vw;height:100vh;'
        + 'max-width:none;max-height:none;object-fit:contain;background:#000';
    return JSON.stringify({ duration: v.duration, width: v.videoWidth, height: v.videoHeight });
})()"#;

/// Seek and wait until the frame at `at_ms` is on screen.
fn seek(tab: &Tab, at_ms: u64) -> Result<(), AdapterError> {
    let js = format!(
        "(async () => {{
            const v = document.querySelector('video');
            const t = {seconds};
            if (Math.abs(v.currentTime - t) > 0.0005) {{
                await new Promise(ok => {{
                    v.addEventListener('seeked', ok, {{ once: true }});
                    setTimeout(ok, 5000);
                    v.currentTime = t;
                }});
            }}
            await new Promise(ok => requestAnimationFrame(() => requestAnimationFrame(ok)));
            return 'ok';
        }})()",
        seconds = at_ms as f64 / 1000.0,
    );
    tab.evaluate(&js, true)
        .map(|_| ())
        .map_err(|e| AdapterError::Web(format!("seeking to {at_ms} ms: {e}")))
}

/// The page as a small greyscale picture, for the change test, with its
/// width and height.
fn thumbnail(tab: &Tab, width: u32, height: u32) -> Result<(Vec<u8>, usize, usize), AdapterError> {
    let png = tab
        .capture_screenshot(
            Page::CaptureScreenshotFormatOption::Png,
            None,
            Some(Page::Viewport {
                x: 0.0,
                y: 0.0,
                width: width as f64,
                height: height as f64,
                scale: (THUMB_WIDTH / width as f64).min(1.0),
            }),
            true,
        )
        .map_err(|e| AdapterError::Web(format!("capturing a thumbnail: {e}")))?;
    let luma = image::load_from_memory(&png)
        .map_err(|e| AdapterError::Web(format!("decoding a thumbnail: {e}")))?
        .to_luma8();
    let (w, h) = (luma.width() as usize, luma.height() as usize);
    Ok((luma.into_raw(), w, h))
}

/// The thumbnail area left once still bands are trimmed from the edges, or
/// `None` when nothing at an edge stayed still (or everything did: a video
/// with no visible activity is sent whole rather than guessed at).
fn still_edges(thumbs: &[Vec<u8>], w: usize, h: usize) -> Option<Area> {
    if thumbs.len() < 2 || w == 0 || h == 0 || thumbs.iter().any(|t| t.len() != w * h) {
        return None;
    }
    let mut row_changes = vec![0usize; h];
    let mut col_changes = vec![0usize; w];
    for pair in thumbs.windows(2) {
        let mut rows = vec![false; h];
        let mut cols = vec![false; w];
        for y in 0..h {
            for x in 0..w {
                if pair[0][y * w + x].abs_diff(pair[1][y * w + x]) > PIXEL_CHANGE {
                    rows[y] = true;
                    cols[x] = true;
                }
            }
        }
        rows.iter()
            .zip(&mut row_changes)
            .for_each(|(c, n)| *n += usize::from(*c));
        cols.iter()
            .zip(&mut col_changes)
            .for_each(|(c, n)| *n += usize::from(*c));
    }
    let band = |changes: &mut dyn Iterator<Item = &usize>, len: usize| {
        let limit = (len as f64 * EDGE_LIMIT) as usize;
        changes
            .take(limit)
            .take_while(|&&n| n <= STILL_CHANGES)
            .count()
    };
    let top = band(&mut row_changes.iter(), h);
    let bottom = band(&mut row_changes.iter().rev(), h);
    let left = band(&mut col_changes.iter(), w);
    let right = band(&mut col_changes.iter().rev(), w);
    let active = row_changes.iter().any(|&n| n > STILL_CHANGES);
    if !active || top + bottom + left + right == 0 {
        return None;
    }
    Some(Area {
        x: left as u32,
        y: top as u32,
        width: (w - left - right) as u32,
        height: (h - top - bottom) as u32,
    })
}

/// Sample times from the start to just before the end, at `every_ms` or
/// more sparsely for a long video, always ending on the last moment.
fn sample_times(duration_ms: u64, every_ms: u64) -> Vec<u64> {
    if duration_ms == 0 {
        return vec![0];
    }
    let every = every_ms.max(duration_ms.div_ceil(MAX_SAMPLES)).max(1);
    // The very end of a file can be a black or missing frame.
    let last = duration_ms.saturating_sub(100);
    let mut times: Vec<u64> = (0..=last).step_by(every as usize).collect();
    if times.last() != Some(&last) {
        times.push(last);
    }
    times
}

/// Mean absolute difference of two greyscale thumbnails, 0 (same) to 1.
fn difference(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 1.0;
    }
    let total: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    total as f64 / (a.len() as f64 * 255.0)
}

/// Indices of the samples to keep: the first, every one that changed by
/// more than [`CHANGE`] from the last kept, and the last. Beyond `max`, the
/// largest changes are kept, still in order, with the first and last.
fn select_changes(thumbs: &[Vec<u8>], max: usize) -> Vec<usize> {
    if thumbs.is_empty() {
        return Vec::new();
    }
    let mut kept = vec![(0usize, f64::INFINITY)];
    for (i, thumb) in thumbs.iter().enumerate().skip(1) {
        let since = difference(&thumbs[kept.last().expect("first kept").0], thumb);
        if since > CHANGE {
            kept.push((i, since));
        }
    }
    // The last frame is where the recording ended up: kept whatever its
    // own change, and never trimmed by the cap below.
    let last = thumbs.len() - 1;
    match kept.last_mut() {
        Some(k) if k.0 == last => k.1 = f64::INFINITY,
        _ => kept.push((last, f64::INFINITY)),
    }
    if kept.len() > max.max(2) {
        let mut by_change = kept.clone();
        by_change.sort_by(|a, b| b.1.total_cmp(&a.1));
        by_change.truncate(max.max(2));
        kept.retain(|k| by_change.iter().any(|b| b.0 == k.0));
    }
    kept.into_iter().map(|k| k.0).collect()
}

/// A `file://` address for a local path, percent-encoded so a name with
/// spaces (most screen recorders' default) loads.
fn file_url(path: &Path) -> Result<String, AdapterError> {
    let full = std::fs::canonicalize(path)
        .map_err(|e| AdapterError::Web(format!("reading {}: {e}", path.display())))?;
    let text = full.to_string_lossy();
    // Windows canonical paths carry a verbatim prefix the browser rejects.
    let text = text
        .strip_prefix(r"\\?\")
        .unwrap_or(&text)
        .replace('\\', "/");
    Ok(file_url_of(&text))
}

fn file_url_of(path: &str) -> String {
    let mut url = String::from("file://");
    if !path.starts_with('/') {
        url.push('/');
    }
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b':' | b'-' | b'_' | b'.' | b'~' => {
                url.push(byte as char)
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(level: u8) -> Vec<u8> {
        vec![level; 100]
    }

    #[test]
    fn a_still_screen_keeps_only_its_first_and_last_frame() {
        let thumbs = vec![flat(10); 20];
        assert_eq!(select_changes(&thumbs, 60), vec![0, 19]);
    }

    #[test]
    fn each_change_is_kept_once_against_the_last_kept_frame() {
        // 0..3 still, a change at 4 that holds, another at 8.
        let mut thumbs = vec![flat(10); 4];
        thumbs.extend(vec![flat(60); 4]);
        thumbs.extend(vec![flat(200); 3]);
        assert_eq!(select_changes(&thumbs, 60), vec![0, 4, 8, 10]);
    }

    /// Small steps that each fall under the threshold still add up: a
    /// value typed one key at a time is caught once it has moved enough.
    #[test]
    fn slow_change_adds_up_against_the_last_kept_frame() {
        let thumbs: Vec<Vec<u8>> = (0..10u8).map(|i| flat(100 + i / 3)).collect();
        let kept = select_changes(&thumbs, 60);
        assert!(kept.len() > 2, "{kept:?}");
    }

    #[test]
    fn beyond_the_cap_the_largest_changes_stay_in_order() {
        let levels = [0u8, 2, 100, 104, 30, 250, 252];
        let thumbs: Vec<Vec<u8>> = levels.iter().map(|&l| flat(l)).collect();
        let kept = select_changes(&thumbs, 4);
        assert_eq!(kept.len(), 4);
        assert_eq!(kept.first(), Some(&0));
        assert_eq!(kept.last(), Some(&6));
        assert!(kept.windows(2).all(|w| w[0] < w[1]), "{kept:?}");
        assert!(
            kept.contains(&5),
            "the jump to 250 is the biggest change: {kept:?}"
        );
    }

    /// A 40x20 screen: a taskbar two rows high at the bottom whose clock
    /// ticks once, and a page above it that changes every sample.
    fn screen_with_taskbar(samples: usize) -> Vec<Vec<u8>> {
        (0..samples)
            .map(|i| {
                let mut t = vec![200u8; 40 * 20];
                for y in 0..18 {
                    for x in 0..40 {
                        t[y * 40 + x] = if (x + i) % 2 == 0 { 30 } else { 220 };
                    }
                }
                // The clock in the taskbar's corner ticks over once.
                if i >= samples / 2 {
                    t[19 * 40 + 38] = 0;
                }
                t
            })
            .collect()
    }

    #[test]
    fn a_still_taskbar_is_trimmed_and_the_page_kept() {
        let kept = still_edges(&screen_with_taskbar(12), 40, 20).expect("trims");
        assert_eq!(
            kept,
            Area {
                x: 0,
                y: 0,
                width: 40,
                height: 18
            }
        );
    }

    #[test]
    fn no_edge_loses_more_than_a_tenth_of_the_screen() {
        // Only a small patch in the middle ever changes.
        let thumbs: Vec<Vec<u8>> = (0..10u8)
            .map(|i| {
                let mut t = vec![100u8; 40 * 20];
                t[10 * 40 + 20] = i * 25;
                t
            })
            .collect();
        let kept = still_edges(&thumbs, 40, 20).expect("trims");
        assert_eq!((kept.x, kept.y), (4, 2));
        assert_eq!((kept.width, kept.height), (32, 16));
    }

    #[test]
    fn a_screen_with_no_activity_or_no_still_edge_is_sent_whole() {
        assert_eq!(still_edges(&vec![vec![50u8; 800]; 6], 40, 20), None);
        let busy: Vec<Vec<u8>> = (0..6u8).map(|i| vec![i * 40; 800]).collect();
        assert_eq!(still_edges(&busy, 40, 20), None);
    }

    #[test]
    fn samples_cover_the_video_and_thin_out_when_it_is_long() {
        assert_eq!(sample_times(2_000, 500), vec![0, 500, 1000, 1500, 1900]);
        let long = sample_times(60 * 60 * 1000, 500);
        assert!(long.len() as u64 <= MAX_SAMPLES + 1, "{}", long.len());
        assert_eq!(sample_times(0, 500), vec![0]);
    }

    #[test]
    fn a_recorders_file_name_with_spaces_becomes_a_loadable_address() {
        assert_eq!(
            file_url_of("/Users/me/SWFL Aufzeichnung 2026-11-06 120541.mp4"),
            "file:///Users/me/SWFL%20Aufzeichnung%202026-11-06%20120541.mp4"
        );
        assert_eq!(
            file_url_of("C:/Users/me/Videos/demo.mp4"),
            "file:///C:/Users/me/Videos/demo.mp4"
        );
    }
}
