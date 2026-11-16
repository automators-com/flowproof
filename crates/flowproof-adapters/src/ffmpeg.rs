//! The ffmpeg flowproof reads screen recordings with.
//!
//! flowproof ships a decode-only ffmpeg next to its own binary: it reads the
//! formats screen recorders produce and nothing else, so drafting from a
//! video needs no browser and no ffmpeg of the user's own. `FLOWPROOF_FFMPEG`
//! names another one (a packager's, a developer's); an ffmpeg on `PATH` is
//! the last resort, for builds from source.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

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
        assert_eq!(found.unwrap(), PathBuf::from("/opt/ff/ffmpeg"));
    }

    #[test]
    fn a_named_ffmpeg_that_is_not_there_is_an_error_not_a_fallback() {
        let err = pick(
            Some("/nope/ffmpeg".into()),
            Some("/app/ffmpeg".into()),
            None,
            only(&["/app/ffmpeg"]),
        )
        .unwrap_err();
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
        assert_eq!(found.unwrap(), PathBuf::from("/app").join(BINARY));
    }

    #[test]
    fn path_is_searched_in_order_when_nothing_is_bundled() {
        let found = pick(
            None,
            Some(PathBuf::from("/app").join(BINARY)),
            search_path(&["/a", "/b", "/c"]),
            |p| p == Path::new("/b").join(BINARY) || p == Path::new("/c").join(BINARY),
        );
        assert_eq!(found.unwrap(), PathBuf::from("/b").join(BINARY));
    }

    #[test]
    fn no_ffmpeg_anywhere_says_how_to_get_one() {
        let err = pick(None, None, search_path(&["/a"]), |_| false).unwrap_err();
        assert!(matches!(err, AdapterError::NoFfmpeg(_)));
        assert!(err.to_string().contains("Reinstall flowproof"), "{err}");
    }
}
