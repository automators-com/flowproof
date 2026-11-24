//! The frames of a screen recording, decoded by the ffmpeg that ships with
//! flowproof ([`crate::ffmpeg`]), so reading a video needs no browser,
//! whatever app it shows.
//!
//! The video is decoded once into small greyscale thumbnails, one per
//! sample; a sample is kept when it differs enough from the last kept one,
//! and only the kept moments are decoded again at full size as JPEG for the
//! model. A recording is mostly a still screen with short bursts of change,
//! so this keeps the moments a person acted and drops the rest.
//!
//! The same thumbnails decide what is cut from the frames: a band at an edge
//! of the screen that stays still the whole recording (a taskbar, a Dock, a
//! menu bar) is trimmed before anything reaches the model. Only edges are
//! trimmed, and never more than a tenth of the screen from each: finding
//! "the app's window" anywhere on the screen would sooner or later cut away
//! a label the model needs.

use std::path::Path;

use crate::ffmpeg::{Crop, Ffmpeg, Format, Probe};
use crate::AdapterError;

/// What ffmpeg read from the file before any frame was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoInfo {
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    /// The part of the screen the frames keep, in the frames' own pixels,
    /// when still bands were trimmed from its edges.
    pub kept: Option<Area>,
    /// How many samples were compared, and how many of them changed the
    /// screen enough to keep before `max_frames` capped them: when
    /// `changes` is past the cap, frames were dropped. Both 0 from
    /// [`probe`], which compares nothing.
    pub samples: usize,
    pub changes: usize,
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
const THUMB_WIDTH: u32 = 128;
/// Mean brightness difference (0..1) between a sample and the last kept
/// frame that counts as the screen having changed. Low on purpose: typing a
/// short value into one field moves only a sliver of the screen.
const CHANGE: f64 = 0.003;
/// A thumbnail pixel this much brighter or darker counts as changed.
const PIXEL_CHANGE: u8 = 24;
/// A row or column that changed this few times in the whole recording is
/// still: a clock ticking over, a pointer passing once.
const STILL_CHANGES: usize = 3;
/// The most trimmed from any one edge, as a share of the screen.
const EDGE_LIMIT: f64 = 0.10;

/// Duration and size, once ffmpeg has shown it can decode the picture;
/// nothing is captured.
pub fn probe(video: &Path) -> Result<VideoInfo, AdapterError> {
    readable(&Ffmpeg::find()?, video).map(|read| video_info(&read))
}

/// The changed moments of `video`, in order, with the first and last frame
/// always among them. `on_sample(done, total)` reports the slow part.
pub fn extract(
    video: &Path,
    opts: &FrameOptions,
    on_sample: &mut dyn FnMut(usize, usize),
) -> Result<(VideoInfo, Vec<Frame>), AdapterError> {
    let ffmpeg = Ffmpeg::find()?;
    let read = readable(&ffmpeg, video)?;
    let mut info = video_info(&read);
    let width = info.width.min(opts.max_width).max(1);
    let height = scaled(info.height, width, info.width);
    let thumb = (
        THUMB_WIDTH.min(width),
        scaled(height, THUMB_WIDTH.min(width), width),
    );

    // Every sample but a last one off the regular step comes from a single
    // decoding pass; that last one is decoded on its own.
    let planned = sample_times(info.duration_ms, opts.every_ms);
    let every = sample_every(info.duration_ms, opts.every_ms);
    let regular = planned.iter().take_while(|&&t| t % every == 0).count();
    let total = planned.len();
    let mut thumbs = ffmpeg
        .thumbnails(video, every, thumb, regular, &mut |n| on_sample(n, total))
        .map_err(|e| unreadable(&read, video, e))?;
    let mut times: Vec<u64> = (0..thumbs.len() as u64).map(|k| k * every).collect();
    let last = *planned.last().expect("sample_times is never empty");
    if times.last() != Some(&last) {
        on_sample(total - 1, total);
        thumbs.push(ffmpeg.picture_at(video, last, thumb, None, Format::Gray)?);
        times.push(last);
    }

    // Thumbnail pixels to frame pixels, per axis.
    info.kept = still_edges(&thumbs, thumb.0 as usize, thumb.1 as usize).map(|k| {
        let sx = width as f64 / thumb.0 as f64;
        let sy = height as f64 / thumb.1 as f64;
        Area {
            x: (k.x as f64 * sx).round() as u32,
            y: (k.y as f64 * sy).round() as u32,
            width: (k.width as f64 * sx).round() as u32,
            height: (k.height as f64 * sy).round() as u32,
        }
    });
    let crop = info.kept.map(|k| Crop {
        x: k.x,
        y: k.y,
        width: k.width,
        height: k.height,
    });
    info.samples = thumbs.len();
    info.changes = select_changes(&thumbs, usize::MAX).len();
    let mut frames = Vec::new();
    for i in select_changes(&thumbs, opts.max_frames) {
        let jpeg = ffmpeg.picture_at(video, times[i], (width, height), crop, Format::Jpeg)?;
        frames.push(Frame {
            at_ms: times[i],
            jpeg,
        });
    }
    Ok((info, frames))
}

/// What `video` is, once its first picture has been decoded: a file ffmpeg
/// can describe but not decode fails here, before anything else is spent.
fn readable(ffmpeg: &Ffmpeg, video: &Path) -> Result<Probe, AdapterError> {
    let read = ffmpeg.probe(video)?;
    ffmpeg
        .picture_at(video, 0, (16, 16), None, Format::Gray)
        .map_err(|e| unreadable(&read, video, e))?;
    Ok(read)
}

/// A decoding failure, said with the way out when the format is one
/// flowproof's ffmpeg leaves out on purpose.
fn unreadable(read: &Probe, video: &Path, error: AdapterError) -> AdapterError {
    if read.codec != "hevc" {
        return error;
    }
    AdapterError::Video(format!(
        "{} is an HEVC (H.265) video, which flowproof does not read. Export it as \
         H.264 (.mp4) and try again; iPhones and Macs record H.264 when the camera \
         or screen recording format is set to Most Compatible.",
        video.display()
    ))
}

fn video_info(read: &Probe) -> VideoInfo {
    VideoInfo {
        duration_ms: read.duration_ms,
        width: read.width,
        height: read.height,
        kept: None,
        samples: 0,
        changes: 0,
    }
}

/// `length` for a picture `from` wide scaled to `to` wide.
fn scaled(length: u32, to: u32, from: u32) -> u32 {
    ((length as f64) * (to as f64) / (from.max(1) as f64))
        .round()
        .max(1.0) as u32
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
    let every = sample_every(duration_ms, every_ms);
    // The very end of a file can be a black or missing frame.
    let last = duration_ms.saturating_sub(100);
    let mut times: Vec<u64> = (0..=last).step_by(every as usize).collect();
    if times.last() != Some(&last) {
        times.push(last);
    }
    times
}

/// The step between samples: `every_ms`, or wider for a long video.
fn sample_every(duration_ms: u64, every_ms: u64) -> u64 {
    every_ms.max(duration_ms.div_ceil(MAX_SAMPLES)).max(1)
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

    /// What `changes` reports: with no cap, every change is kept, so a
    /// count past `max_frames` means the cap dropped some.
    #[test]
    fn with_no_cap_every_change_counts() {
        let thumbs: Vec<Vec<u8>> = (0..10).map(|i| flat(i * 20)).collect();
        assert_eq!(select_changes(&thumbs, usize::MAX).len(), 10);
        assert_eq!(select_changes(&thumbs, 4).len(), 4);
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
    fn an_hevc_video_that_will_not_decode_says_how_to_export_it() {
        let hevc = Probe {
            duration_ms: 1000,
            width: 1920,
            height: 1080,
            codec: "hevc".into(),
        };
        let failed = || AdapterError::Video("Decoder (codec hevc) not found".into());
        let err = unreadable(&hevc, Path::new("clip.mov"), failed());
        assert!(err.to_string().contains("Export it as H.264"), "{err}");
        // Any other codec keeps ffmpeg's own reason.
        let h264 = Probe {
            codec: "h264".into(),
            ..hevc
        };
        let err = unreadable(&h264, Path::new("clip.mp4"), failed());
        assert_eq!(err.to_string(), "Decoder (codec hevc) not found");
    }

    #[test]
    fn samples_cover_the_video_and_thin_out_when_it_is_long() {
        assert_eq!(sample_times(2_000, 500), vec![0, 500, 1000, 1500, 1900]);
        let long = sample_times(60 * 60 * 1000, 500);
        assert!(long.len() as u64 <= MAX_SAMPLES + 1, "{}", long.len());
        assert_eq!(sample_times(0, 500), vec![0]);
    }
}
