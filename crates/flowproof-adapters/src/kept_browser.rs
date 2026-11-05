//! A browser left open after a run (`run --leave-open <file>`), so a later
//! run can carry on in it (`run --attach <file>`), and closed on request
//! (`flowproof session close <file>`).
//!
//! A browser flowproof launches normally dies with the run: the launcher
//! kills it and deletes its profile when its handle drops. A kept browser is
//! started differently - detached, in its own process group, with a profile
//! directory of its own - and the run CONNECTS to it, so the run ends and the
//! browser stays. The session file is how the next run, and whoever closes
//! it, finds it again: the DevTools address, the process, the profile.
//!
//! The DevTools address is full control of a logged-in browser, so it is
//! bound to 127.0.0.1 and the session file is created private (0600 on unix)
//! and refused when it is not.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Set by the CLI for `run --leave-open <file>`: launch a kept browser and
/// record it in `<file>`.
pub const LEAVE_OPEN_ENV: &str = "FLOWPROOF_LEAVE_BROWSER_OPEN";
/// Set by the CLI for `run --attach <file>`: carry on in the kept browser
/// `<file>` records instead of launching one.
pub const ATTACH_ENV: &str = "FLOWPROOF_ATTACH_BROWSER";

const SCHEMA: u32 = 1;
/// Every kept profile lives directly under the temp directory with this
/// prefix; `close` deletes nothing else, whatever a session file says.
const PROFILE_PREFIX: &str = "flowproof-kept-";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

/// What a session file records about one kept browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeptSession {
    pub schema: u32,
    pub pid: u32,
    /// `ws://127.0.0.1:<port>/devtools/browser/<id>`.
    pub ws_url: String,
    pub profile_dir: PathBuf,
    /// Unix seconds.
    pub created: u64,
}

impl KeptSession {
    fn port(&self) -> Option<u16> {
        self.ws_url
            .strip_prefix("ws://127.0.0.1:")?
            .split('/')
            .next()?
            .parse()
            .ok()
    }

    /// Whether the browser is still there: its DevTools port still answers.
    pub fn is_alive(&self) -> bool {
        self.port().is_some_and(|port| {
            std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                Duration::from_millis(500),
            )
            .is_ok()
        })
    }
}

/// Start a visible Chromium that outlives this process and wait until its
/// DevTools endpoint is up.
pub fn spawn(chrome: &Path) -> Result<KeptSession, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let nanos = now.as_nanos();
    let profile_dir =
        std::env::temp_dir().join(format!("{PROFILE_PREFIX}{}-{nanos}", std::process::id()));
    private_dir(&profile_dir)?;

    let mut command = std::process::Command::new(chrome);
    command
        .args(headless_chrome::browser::DEFAULT_ARGS)
        .arg("--no-sandbox")
        .arg("--no-default-browser-check")
        .arg("--remote-debugging-address=127.0.0.1")
        .arg("--remote-debugging-port=0")
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg("about:blank")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Its own process group, so stopping the run that started it (a signal
    // to the run's group) leaves it alone, and `close` can stop it whole.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(all(windows, feature = "sap-com"))]
    let spawned = crate::sap_com::spawn_detached(&mut command);
    #[cfg(not(all(windows, feature = "sap-com")))]
    let spawned = command.spawn();
    let mut child = spawned.map_err(|e| {
        let _ = std::fs::remove_dir_all(&profile_dir);
        format!("starting {}: {e}", chrome.display())
    })?;

    // Chromium writes the port it picked (and the browser target's path)
    // into the profile once DevTools is listening.
    let active_port = profile_dir.join("DevToolsActivePort");
    let started = Instant::now();
    let ws_url = loop {
        if let Ok(text) = std::fs::read_to_string(&active_port) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                if let Ok(port) = port.trim().parse::<u16>() {
                    break format!("ws://127.0.0.1:{port}{}", path.trim());
                }
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            let _ = std::fs::remove_dir_all(&profile_dir);
            return Err(format!(
                "the browser exited during startup ({status}); a host with no desktop \
                 session cannot show one"
            ));
        }
        if started.elapsed() > STARTUP_TIMEOUT {
            stop(child.id());
            let _ = std::fs::remove_dir_all(&profile_dir);
            return Err("the browser did not open its DevTools port within 20s".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    Ok(KeptSession {
        schema: SCHEMA,
        pid: child.id(),
        ws_url,
        profile_dir,
        created: now.as_secs(),
    })
}

/// Write `session` to `path`, private and whole: a temp file in the same
/// directory, then a rename.
pub fn write(path: &Path, session: &KeptSession) -> Result<(), String> {
    let text = serde_json::to_string_pretty(session).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&tmp).and_then(|mut file| {
        use std::io::Write;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    written
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("writing {}: {e}", path.display())
        })
}

/// Read and check a session file: a regular, private file naming a
/// loopback DevTools address and a profile flowproof made.
pub fn read(path: &Path) -> Result<KeptSession, String> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("no kept browser at {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "{} must have private permissions (chmod 600)",
                path.display()
            ));
        }
    }
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let session: KeptSession = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not a kept-browser session: {e}", path.display()))?;
    if session.schema != SCHEMA {
        return Err(format!(
            "{} was written by a different flowproof (schema {})",
            path.display(),
            session.schema
        ));
    }
    if session.port().is_none() {
        return Err(format!(
            "{} names a DevTools address that is not on 127.0.0.1",
            path.display()
        ));
    }
    if !is_kept_profile(&session.profile_dir) {
        return Err(format!(
            "{} names a profile directory flowproof did not make",
            path.display()
        ));
    }
    Ok(session)
}

/// Close the kept browser `path` records and remove its profile and the
/// file. Nothing there, or a browser already gone, is not an error.
pub fn close(path: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    let session = read(path)?;
    discard(&session);
    std::fs::remove_file(path).map_err(|e| format!("removing {}: {e}", path.display()))
}

/// Stop a kept browser and remove its profile.
pub fn discard(session: &KeptSession) {
    // Only a browser still answering on its port is signalled: a pid alone
    // could by now belong to anything.
    if session.is_alive() {
        stop(session.pid);
        let started = Instant::now();
        while session.is_alive() && started.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(100));
        }
        if session.is_alive() {
            kill(session.pid);
        }
    }
    if is_kept_profile(&session.profile_dir) {
        let _ = std::fs::remove_dir_all(&session.profile_dir);
    }
}

fn is_kept_profile(dir: &Path) -> bool {
    let temp = std::env::temp_dir();
    dir.parent() == Some(temp.as_path())
        && dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(PROFILE_PREFIX) && !n.contains(".."))
}

fn private_dir(dir: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| format!("creating {}: {e}", dir.display()))
}

/// Ask the browser's whole process group to quit.
fn stop(pid: u32) {
    #[cfg(unix)]
    signal_group(pid, "TERM");
    #[cfg(windows)]
    kill(pid);
}

fn kill(pid: u32) {
    #[cfg(unix)]
    signal_group(pid, "KILL");
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(unix)]
fn signal_group(pid: u32, signal: &str) {
    let _ = std::process::Command::new("kill")
        .args(["-s", signal, "--", &format!("-{pid}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> KeptSession {
        KeptSession {
            schema: SCHEMA,
            pid: 4242,
            ws_url: "ws://127.0.0.1:9333/devtools/browser/abc".into(),
            profile_dir: std::env::temp_dir().join(format!("{PROFILE_PREFIX}test")),
            created: 1_793_836_800,
        }
    }

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("flowproof-kept-tests-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join("session.json")
    }

    #[test]
    fn a_session_round_trips_through_its_file() {
        let path = temp_file("round-trip");
        write(&path, &session()).expect("writes");
        assert_eq!(read(&path).expect("reads"), session());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Writing again replaces it.
        write(&path, &session()).expect("rewrites");
    }

    #[cfg(unix)]
    #[test]
    fn a_session_file_others_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_file("perms");
        write(&path, &session()).expect("writes");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(read(&path).expect_err("refused").contains("chmod 600"));
    }

    #[test]
    fn a_session_pointing_anywhere_but_loopback_or_its_own_profile_is_refused() {
        let path = temp_file("tamper");
        let mut remote = session();
        remote.ws_url = "ws://10.0.0.5:9333/devtools/browser/abc".into();
        write(&path, &remote).expect("writes");
        assert!(read(&path).expect_err("refused").contains("127.0.0.1"));

        let mut elsewhere = session();
        elsewhere.profile_dir = PathBuf::from("/home/someone");
        write(&path, &elsewhere).expect("writes");
        assert!(read(&path).expect_err("refused").contains("profile"));

        std::fs::write(&path, "{\"schema\":1,\"extra\":true}").expect("writes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        }
        assert!(read(&path).is_err());
    }

    #[test]
    fn closing_a_session_whose_browser_is_gone_just_cleans_up() {
        let path = temp_file("gone");
        let mut gone = session();
        // Port 1 does not answer, so nothing is signalled.
        gone.ws_url = "ws://127.0.0.1:1/devtools/browser/abc".into();
        write(&path, &gone).expect("writes");
        close(&path).expect("closes");
        assert!(!path.exists());
        // Idempotent.
        close(&path).expect("nothing to close");
    }
}
