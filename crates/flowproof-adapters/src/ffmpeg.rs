//! The ffmpeg flowproof reads screen recordings with.
//!
//! flowproof ships a decode-only ffmpeg next to its own binary: it reads the
//! formats screen recorders produce and nothing else, so drafting from a
//! video needs no browser and no ffmpeg of the user's own. `FLOWPROOF_FFMPEG`
//! names another one (a packager's, a developer's); an ffmpeg on `PATH` is
//! the last resort, for builds from source.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::AdapterError;

/// The file name of the binary on this platform.
const BINARY: &str = if cfg!(windows) {
    "ffmpeg.exe"
} else {
    "ffmpeg"
};

/// What a person reads when there is no ffmpeg: it normally comes with
/// flowproof, so a missing one is a broken install, not a missing download.
const NO_FFMPEG_HELP: &str = "reading a video needs the ffmpeg that ships next to flowproof, \
     and it is not there. Reinstall flowproof, or set FLOWPROOF_FFMPEG to the path of an \
     ffmpeg binary.";

/// An ffmpeg binary that was found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ffmpeg {
    path: PathBuf,
}

impl Ffmpeg {
    /// The ffmpeg to use. `FLOWPROOF_FFMPEG` wins when set, and one naming a
    /// file that is not there is an error rather than a quiet fall-back: the
    /// person asked for that binary. Then the one beside flowproof, then
    /// `PATH`.
    pub fn find() -> Result<Self, AdapterError> {
        let beside = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(BINARY)));
        pick(
            std::env::var_os("FLOWPROOF_FFMPEG"),
            beside,
            std::env::var_os("PATH"),
            |path| path.is_file(),
        )
        .map(|path| Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What `video` is: its length, picture size and video codec. A file
    /// ffmpeg cannot open, or one with no picture in it, is an error that
    /// says so.
    pub fn probe(&self, video: &Path) -> Result<Probe, AdapterError> {
        let mut command = self.command();
        command.arg("-i").arg(input(video));
        // With no output named ffmpeg always exits non-zero; what it read
        // is on stderr either way.
        let out = self.output(command)?;
        let read = parse_probe(&String::from_utf8_lossy(&out.stderr)).map_err(|why| {
            AdapterError::Video(format!("could not read {}: {why}", video.display()))
        })?;
        let duration_ms = match read.duration_ms {
            0 => self.measure(video)?,
            ms => ms,
        };
        Ok(Probe {
            duration_ms,
            ..read
        })
    }

    /// A recording made in a browser (MediaRecorder webm) states no length,
    /// so its video track is read to the end, without decoding, to learn it.
    fn measure(&self, video: &Path) -> Result<u64, AdapterError> {
        let mut command = self.command();
        command
            .args(["-v", "error", "-i"])
            .arg(input(video))
            .args(["-map", "0:v:0", "-c", "copy", "-f", "null"])
            .args(["-progress", "pipe:1", "-nostats", "-"]);
        let out = self.output(command)?;
        match parse_progress(&String::from_utf8_lossy(&out.stdout)) {
            Some(ms) if ms > 0 => Ok(ms),
            _ => Err(AdapterError::Video(format!(
                "could not read {}: it states no length, and reading it to the end found none",
                video.display()
            ))),
        }
    }

    /// Small greyscale pictures of `video`, one every `every_ms` from the
    /// start, each `width` x `height` bytes, decoded in one pass. At most
    /// `limit`, fewer when the video ends first. `on_thumb(n)` hears about
    /// each one as it arrives, since this is the slow part of a long video.
    pub fn thumbnails(
        &self,
        video: &Path,
        every_ms: u64,
        (width, height): (u32, u32),
        limit: usize,
        on_thumb: &mut dyn FnMut(usize),
    ) -> Result<Vec<Vec<u8>>, AdapterError> {
        let mut command = self.command();
        command
            .args(["-v", "error", "-i"])
            .arg(input(video))
            .args(["-map", "0:v:0", "-vf"])
            .arg(format!(
                "fps=1000/{},{}",
                every_ms.max(1),
                picture_filter((width, height), None, Format::Gray)
            ))
            .arg("-frames:v")
            .arg(limit.to_string())
            .args(Format::Gray.output_args())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| self.start_error(e))?;
        // Read alongside stdout, so a file that makes ffmpeg complain on
        // every frame cannot fill the pipe and stall it.
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let complaints = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let mut thumbs = Vec::new();
        loop {
            let mut thumb = vec![0u8; width as usize * height as usize];
            match stdout.read_exact(&mut thumb) {
                Ok(()) => {
                    on_thumb(thumbs.len());
                    thumbs.push(thumb);
                }
                // The end of the video, or a part picture ffmpeg gave up on.
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => {
                    let _ = child.kill();
                    return Err(AdapterError::Video(format!(
                        "reading pictures from ffmpeg: {e}"
                    )));
                }
            }
        }
        let _ = child.wait();
        let complaints = complaints.join().unwrap_or_default();
        if thumbs.is_empty() {
            return Err(AdapterError::Video(format!(
                "could not decode {}: {}",
                video.display(),
                last_line(&complaints).unwrap_or("ffmpeg gave no pictures")
            )));
        }
        Ok(thumbs)
    }

    /// One picture of `video` at `at_ms`, scaled to `size` and then cut to
    /// `crop` (in the scaled picture's pixels) when one is given.
    pub fn picture_at(
        &self,
        video: &Path,
        at_ms: u64,
        size: (u32, u32),
        crop: Option<Crop>,
        format: Format,
    ) -> Result<Vec<u8>, AdapterError> {
        let mut command = self.command();
        command
            .args(["-v", "error", "-ss"])
            .arg(seconds(at_ms))
            .arg("-i")
            .arg(input(video))
            .args(["-map", "0:v:0", "-frames:v", "1", "-vf"])
            .arg(picture_filter(size, crop, format))
            .args(format.output_args());
        let out = self.output(command)?;
        if out.stdout.is_empty() {
            let why = String::from_utf8_lossy(&out.stderr);
            return Err(AdapterError::Video(format!(
                "could not take the picture at {} in {}: {}",
                seconds(at_ms),
                video.display(),
                last_line(&why).unwrap_or("ffmpeg gave no picture")
            )));
        }
        Ok(out.stdout)
    }

    /// ffmpeg with the options every call shares: no banner, never reading
    /// the terminal, and no console window flashing up on Windows.
    fn command(&self) -> Command {
        let mut command = Command::new(&self.path);
        command
            .args(["-hide_banner", "-nostdin"])
            .stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        command
    }

    fn output(&self, mut command: Command) -> Result<Output, AdapterError> {
        command.output().map_err(|e| self.start_error(e))
    }

    fn start_error(&self, e: std::io::Error) -> AdapterError {
        AdapterError::Video(format!(
            "could not start ffmpeg at {}: {e}",
            self.path.display()
        ))
    }
}

/// What a picture is delivered as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// One byte per pixel, rows top to bottom: what the change test reads.
    Gray,
    /// A JPEG, for the model.
    Jpeg,
}

impl Format {
    fn output_args(self) -> [&'static str; 7] {
        match self {
            Format::Gray => ["-f", "rawvideo", "-pix_fmt", "gray", "-an", "-sn", "pipe:1"],
            // q 3 of mjpeg's 2 (best) to 31: text in a screenshot stays
            // readable at a size a model call can carry 60 of.
            Format::Jpeg => ["-c:v", "mjpeg", "-q:v", "3", "-f", "image2pipe", "pipe:1"],
        }
    }
}

/// A rectangle cut from a picture, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Scale first, so `crop` is in the pixels the caller asked for. Greyscale
/// is taken before scaling: one plane scales to any size, odd ones included.
fn picture_filter((width, height): (u32, u32), crop: Option<Crop>, format: Format) -> String {
    let mut filter = String::new();
    if format == Format::Gray {
        filter.push_str("format=gray,");
    }
    filter.push_str(&format!("scale={width}:{height}:flags=area"));
    if let Some(c) = crop {
        filter.push_str(&format!(",crop={}:{}:{}:{}", c.width, c.height, c.x, c.y));
    }
    filter
}

/// Milliseconds as the seconds ffmpeg's `-ss` takes, exactly.
fn seconds(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

fn last_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).rfind(|l| !l.is_empty())
}

/// What [`Ffmpeg::probe`] read from a video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    /// ffmpeg's name for the picture's codec (`h264`, `vp9`, `hevc`, …).
    pub codec: String,
}

/// The input as ffmpeg should open it. `file:` keeps a name with a colon in
/// it from being read as a protocol.
fn input(video: &Path) -> OsString {
    let mut input = OsString::from("file:");
    input.push(video);
    input
}

/// The input dump ffmpeg prints for `-i` alone: the first video stream's
/// codec and size, and the length when the file states one. The error is
/// ffmpeg's own reason, without the file name it is prefixed with.
fn parse_probe(text: &str) -> Result<Probe, String> {
    if !text.lines().any(|l| l.trim_start().starts_with("Input #0")) {
        let last = text.lines().rev().find(|l| !l.trim().is_empty());
        let reason = last.map(|l| l.rsplit(": ").next().unwrap_or(l).trim());
        return Err(reason.unwrap_or("ffmpeg could not open it").to_string());
    }
    let duration_ms = text
        .lines()
        .find_map(|l| l.split_once("Duration: "))
        .and_then(|(_, rest)| parse_clock(rest.split(',').next().unwrap_or_default()));
    let picture = text
        .lines()
        .filter(|l| l.trim_start().starts_with("Stream #"))
        .find_map(|l| l.split_once(": Video: ").map(|(_, rest)| rest));
    let Some(picture) = picture else {
        return Err("it has no video in it".into());
    };
    let codec = picture
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches(',')
        .to_string();
    let (width, height) = picture
        .split(',')
        .find_map(|part| parse_size(part.split_whitespace().next()?))
        .ok_or("ffmpeg did not say how big its picture is")?;
    Ok(Probe {
        duration_ms: duration_ms.unwrap_or(0),
        width,
        height,
        codec,
    })
}

/// `1914x1074` as (1914, 1074).
fn parse_size(word: &str) -> Option<(u32, u32)> {
    let (w, h) = word.split_once('x')?;
    let (w, h) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// `00:01:24.37` in milliseconds; `N/A` is `None`.
fn parse_clock(clock: &str) -> Option<u64> {
    let mut parts = clock.trim().splitn(3, ':');
    let hours: u64 = parts.next()?.parse().ok()?;
    let minutes: u64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    Some((hours * 3600 + minutes * 60) * 1000 + (seconds * 1000.0).round() as u64)
}

/// The furthest point `-progress` reported, in milliseconds.
fn parse_progress(text: &str) -> Option<u64> {
    text.lines()
        .filter_map(|l| l.strip_prefix("out_time_us="))
        .filter_map(|us| us.trim().parse::<u64>().ok())
        .max()
        .map(|us| us / 1000)
}

/// [`Ffmpeg::find`] with the environment and the filesystem passed in, so
/// every branch is testable on any host.
fn pick(
    named: Option<OsString>,
    beside: Option<PathBuf>,
    search_path: Option<OsString>,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf, AdapterError> {
    if let Some(named) = named.filter(|n| !n.is_empty()) {
        let path = PathBuf::from(named);
        if exists(&path) {
            return Ok(path);
        }
        return Err(AdapterError::NoFfmpeg(format!(
            "FLOWPROOF_FFMPEG is set to {}, and there is no file there. Point it at an \
             ffmpeg binary, or unset it to use the one that ships with flowproof.",
            path.display()
        )));
    }
    if let Some(beside) = beside.filter(|b| exists(b)) {
        return Ok(beside);
    }
    search_path
        .iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(BINARY))
        .find(|path| exists(path))
        .ok_or_else(|| AdapterError::NoFfmpeg(NO_FFMPEG_HELP.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(present: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |path| present.iter().any(|p| Path::new(p) == path)
    }

    fn search_path(dirs: &[&str]) -> Option<OsString> {
        std::env::join_paths(dirs).ok()
    }

    #[test]
    fn a_named_ffmpeg_wins_over_the_bundled_one() {
        let found = pick(
            Some("/opt/ff/ffmpeg".into()),
            Some("/app/ffmpeg".into()),
            None,
            only(&["/opt/ff/ffmpeg", "/app/ffmpeg"]),
        );
        assert_eq!(found.expect("found"), PathBuf::from("/opt/ff/ffmpeg"));
    }

    #[test]
    fn a_named_ffmpeg_that_is_not_there_is_an_error_not_a_fallback() {
        let err = pick(
            Some("/nope/ffmpeg".into()),
            Some("/app/ffmpeg".into()),
            None,
            only(&["/app/ffmpeg"]),
        )
        .expect_err("fails");
        assert!(err.to_string().contains("FLOWPROOF_FFMPEG"), "{err}");
    }

    #[test]
    fn the_bundled_ffmpeg_wins_over_one_on_path() {
        let found = pick(
            None,
            Some(PathBuf::from("/app").join(BINARY)),
            search_path(&["/usr/bin"]),
            |p| p == Path::new("/app").join(BINARY) || p == Path::new("/usr/bin").join(BINARY),
        );
        assert_eq!(found.expect("found"), PathBuf::from("/app").join(BINARY));
    }

    #[test]
    fn path_is_searched_in_order_when_nothing_is_bundled() {
        let found = pick(
            None,
            Some(PathBuf::from("/app").join(BINARY)),
            search_path(&["/a", "/b", "/c"]),
            |p| p == Path::new("/b").join(BINARY) || p == Path::new("/c").join(BINARY),
        );
        assert_eq!(found.expect("found"), PathBuf::from("/b").join(BINARY));
    }

    /// A Windows Game Bar style recording, as ffmpeg 9 describes it.
    const H264_MP4: &str = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'file:/Users/me/SWFL Aufzeichnung.mp4':
  Metadata:
    major_brand     : isom
  Duration: 00:01:24.37, start: 0.000000, bitrate: 1478 kb/s
  Stream #0:0[0x1](und): Video: h264 (avc1 / 0x31637661), yuv420p(progressive), 1914x1074 [SAR 1:1 DAR 319:179], 1333 kb/s, 30 fps, 30 tbr, 15360 tbn (default)
At least one output file must be specified
";

    #[test]
    fn a_recording_reads_as_its_length_size_and_codec() {
        assert_eq!(
            parse_probe(H264_MP4).expect("parses"),
            Probe {
                duration_ms: 84_370,
                width: 1914,
                height: 1074,
                codec: "h264".into(),
            }
        );
    }

    /// Sound first, picture second, and a colour description whose commas
    /// sit inside brackets: the picture is still found and sized.
    #[test]
    fn the_picture_is_found_behind_a_sound_track() {
        let text = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'file:/v/iphone.mov':
  Duration: 00:00:12.50, start: 0.000000, bitrate: 8000 kb/s
  Stream #0:0[0x1](und): Audio: aac (LC) (mp4a / 0x6134706D), 44100 Hz, stereo, fltp, 128 kb/s (default)
  Stream #0:1[0x2](und): Video: hevc (Main) (hvc1 / 0x31637668), yuv420p(tv, bt709), 1920x1080, 7800 kb/s, 60 fps (default)
";
        let read = parse_probe(text).expect("parses");
        assert_eq!(
            (read.codec.as_str(), read.width, read.height),
            ("hevc", 1920, 1080)
        );
        assert_eq!(read.duration_ms, 12_500);
    }

    /// MediaRecorder's webm states no length; the probe leaves it for
    /// [`Ffmpeg::measure`] rather than calling the video empty.
    #[test]
    fn a_browser_recording_with_no_stated_length_reads_as_zero() {
        let text = "Input #0, matroska,webm, from 'file:/tmp/rec.webm':
  Duration: N/A, start: 0.000000, bitrate: N/A
  Stream #0:0(eng): Video: vp8, yuv420p(progressive), 320x180, SAR 1:1 DAR 16:9, 15 fps, 15 tbr, 1k tbn (default)
";
        let read = parse_probe(text).expect("parses");
        assert_eq!(read.duration_ms, 0);
        assert_eq!(
            (read.codec.as_str(), read.width, read.height),
            ("vp8", 320, 180)
        );
    }

    #[test]
    fn a_file_that_is_not_a_video_says_why_without_the_path() {
        let err =
            parse_probe("file:/Users/me/notes.txt: Invalid data found when processing input\n")
                .expect_err("fails");
        assert_eq!(err, "Invalid data found when processing input");
    }

    #[test]
    fn a_sound_only_file_has_no_video_in_it() {
        let text = "Input #0, mp3, from 'file:/v/voice.mp3':
  Duration: 00:00:30.00, start: 0.000000, bitrate: 128 kb/s
  Stream #0:0: Audio: mp3 (mp3float), 44100 Hz, stereo, fltp, 128 kb/s
";
        assert_eq!(
            parse_probe(text).expect_err("fails"),
            "it has no video in it"
        );
    }

    #[test]
    fn progress_reads_as_the_furthest_point_reached() {
        let text = "frame=0\nout_time_us=N/A\nprogress=continue\nout_time_us=2000000\nout_time_us=4066000\nprogress=end\n";
        assert_eq!(parse_progress(text), Some(4066));
        assert_eq!(parse_progress("progress=end\n"), None);
    }

    #[test]
    fn a_name_with_a_colon_is_opened_as_a_file() {
        assert_eq!(
            input(Path::new("clip 10:30.mp4")),
            OsString::from("file:clip 10:30.mp4")
        );
    }

    #[test]
    fn a_frame_for_the_model_is_scaled_then_cut_to_what_was_kept() {
        let crop = Crop {
            x: 0,
            y: 12,
            width: 1280,
            height: 640,
        };
        assert_eq!(
            picture_filter((1280, 718), Some(crop), Format::Jpeg),
            "scale=1280:718:flags=area,crop=1280:640:0:12"
        );
    }

    #[test]
    fn a_thumbnail_turns_grey_before_it_is_scaled() {
        assert_eq!(
            picture_filter((128, 71), None, Format::Gray),
            "format=gray,scale=128:71:flags=area"
        );
    }

    #[test]
    fn a_seek_is_to_the_exact_millisecond() {
        assert_eq!(seconds(84_270), "84.270");
        assert_eq!(seconds(5), "0.005");
    }

    #[test]
    fn no_ffmpeg_anywhere_says_how_to_get_one() {
        let err = pick(None, None, search_path(&["/a"]), |_| false).expect_err("fails");
        assert!(matches!(err, AdapterError::NoFfmpeg(_)));
        assert!(err.to_string().contains("Reinstall flowproof"), "{err}");
    }
}
