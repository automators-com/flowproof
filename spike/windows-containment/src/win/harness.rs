//! The supervisor. Sets the boundary up, runs the canary inside it, and records
//! what was observed.
//!
//! The staging is the plan from the spike brief, cheapest kill first:
//!
//!   * `core`      — days 1–3: identity, job, filters, allowed/denied/grandchild/UDP
//!   * `audit`     — day 4: net-event drops carry address, port and identity
//!   * `negative`  — day 5: enforcement deliberately broken; the run must
//!     report NOT CONTAINED and refuse to certify
//!   * `teardown`  — day 6: filters are gone after an abruptly killed supervisor
//!   * `gui`       — days 7–9: the identity boundary, which is the real obstacle
//!
//! Every stage prints; none of them decides. Reading the log is how the verdict
//! gets made.

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE, HANDLE};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, TerminateProcess, WaitForSingleObject, INFINITE,
};

use super::identity::{is_elevated, RunIdentity};
use super::launch;
use super::netevents;
use super::wfp::{self, Declared, Engine, UserCondition, IPPROTO_TCP_U8};
use super::{wide, WinErr};
use crate::report::Report;

/// An external destination used only for a client-side observation. There is no
/// destination-side oracle for it, and the log says so rather than letting an
/// external probe quietly carry the same weight as an oracled one.
const EXTERNAL_UNDECLARED: &str = "1.1.1.1:443";

pub struct StageOutcome {
    pub child_stdout: String,
    pub child_exit: Option<u32>,
}

/// Work directory the run identity can actually read.
///
/// The cargo target directory is under the runner's own profile and a freshly
/// created local user has no access to it, so the child would die at image load
/// with a message that looks nothing like a permissions problem. Copying the
/// exe somewhere readable removes an entire class of misleading CI failure.
pub struct WorkDir {
    pub path: PathBuf,
    pub exe: PathBuf,
}

impl WorkDir {
    pub fn prepare(user: &str, report: &Report) -> std::io::Result<Self> {
        let path = PathBuf::from(format!("C:\\fp-spike-{}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        let src = std::env::current_exe()?;
        let exe = path.join("canary.exe");
        std::fs::copy(&src, &exe)?;

        // `icacls` rather than hand-rolled ACL code: it is one call, its output
        // is diagnosable from a CI log, and getting inheritance flags wrong in
        // raw `SetEntriesInAclW` is a silent no-op rather than an error.
        //
        // `M` (modify), not `RX`: the canary writes its own side-channel log
        // here, which is what keeps the evidence alive when the spawn falls
        // back to `CreateProcessWithLogonW` and handle inheritance is lost.
        let out = std::process::Command::new("icacls")
            .arg(&path)
            .arg("/grant")
            .arg(format!("{user}:(OI)(CI)M"))
            .output();
        match out {
            Ok(o) => {
                report.note(
                    "workdir.icacls",
                    format!(
                        "status={:?} stdout={} stderr={}",
                        o.status.code(),
                        String::from_utf8_lossy(&o.stdout).trim(),
                        String::from_utf8_lossy(&o.stderr).trim()
                    ),
                );
            }
            Err(e) => report.note("workdir.icacls", format!("spawn failed: {e}")),
        }
        report.note("workdir.path", path.display());
        Ok(Self { path, exe })
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Create an inheritable file handle for the child's stdout/stderr.
///
/// `CREATE_NEW_CONSOLE` would lose the canary's output entirely, and the
/// canary's output *is* the evidence.
fn inheritable_log(path: &Path) -> Result<HANDLE, WinErr> {
    let w = wide(&path.display().to_string());
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    unsafe {
        CreateFileW(
            PCWSTR(w.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(&sa),
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|e| {
        WinErr::new(
            "CreateFileW(child log)",
            e.code().0 as u32,
            path.display().to_string(),
        )
    })
}

pub struct Boundary {
    pub identity: RunIdentity,
    pub engine: Engine,
    pub user_cond: UserCondition,
    pub sublayer: windows::core::GUID,
}

/// Everything up to and including "the filters are in place".
///
/// `enforce = false` is day 5's negative control: identical in every respect
/// except that the block filter is never added. That is the only way to know
/// the canary's failure path works, and a containment feature whose failure
/// path is untested is not a containment feature.
pub fn establish(
    tag: &str,
    declared: &[Declared],
    enforce: bool,
    report: &mut Report,
) -> Result<Boundary, WinErr> {
    let identity = RunIdentity::create(tag)?;
    report.note("identity.name", &identity.name);
    report.note("identity.sid", &identity.sid_string);

    if let Err(e) = launch::grant_desktop_access(identity.psid()) {
        // Not fatal for the console stages, and fatal-looking failures here
        // would be misread as a containment result. Recorded, and the GUI stage
        // checks it again where it actually matters.
        report.note("desktop.grant.error", e.to_string());
    } else {
        report.note("desktop.grant", "ok");
    }

    let mut engine = Engine::open_dynamic()?;
    let sublayer = engine.add_sublayer()?;
    report.note("wfp.sublayer", format!("{sublayer:?}"));

    if let Err(e) = engine.enable_net_events() {
        report.note("wfp.net_events.enable.error", e.to_string());
    } else {
        report.note("wfp.net_events.enable", "ok");
    }

    let user_cond = UserCondition::for_sid(&identity.sid_string)?;

    for d in declared {
        let id = engine.add_permit(&user_cond, *d, false)?;
        report.note(
            "wfp.permit",
            format!(
                "{}:{} proto={} filter_id={}",
                d.addr, d.port, d.protocol, id
            ),
        );
    }

    if enforce {
        let v4 = engine.add_block_all(&user_cond, false)?;
        let v6 = engine.add_block_all(&user_cond, true)?;
        report.note("wfp.block.v4", v4);
        report.note("wfp.block.v6", v6);
        match engine.add_promiscuous_block(&user_cond) {
            Ok(id) => report.note("wfp.block.promiscuous", id),
            Err(e) => report.note("wfp.block.promiscuous.error", e.to_string()),
        }
    } else {
        report.note(
            "wfp.block",
            "DELIBERATELY OMITTED - day 5 negative control, enforcement is broken on purpose",
        );
    }

    Ok(Boundary {
        identity,
        engine,
        user_cond,
        sublayer,
    })
}

/// Run the canary inside the boundary and hand back what it printed.
#[allow(clippy::too_many_arguments)]
pub fn run_canary(
    boundary: &Boundary,
    workdir: &WorkDir,
    args: &[String],
    report: &mut Report,
) -> Result<StageOutcome, WinErr> {
    let (token, logon_kind) = launch::logon(&boundary.identity.name, &boundary.identity.password)?;
    report.note("canary.logon_type", logon_kind);
    let log_path = workdir.path.join("canary.out");
    let side_path = workdir.path.join("canary.side");
    let log = inheritable_log(&log_path)?;

    // `--out` makes the child write its own copy. On the
    // `CreateProcessWithLogonW` fallback path handle inheritance is lost and
    // this file is the only evidence that survives.
    let mut args: Vec<String> = args.to_vec();
    args.push("--out".into());
    args.push(side_path.display().to_string());
    let args = &args;

    let quoted: Vec<String> = args
        .iter()
        .map(|a| {
            if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect();
    let cmdline = format!("\"{}\" {}", workdir.exe.display(), quoted.join(" "));
    report.note("canary.cmdline", &cmdline);

    let spawned = launch::spawn_contained_with_output(
        token,
        &boundary.identity.name,
        &boundary.identity.password,
        &cmdline,
        log,
    );
    unsafe {
        let _ = CloseHandle(log);
    }

    let (mut child, spawn_path) = match spawned {
        Ok(c) => c,
        Err(e) => {
            unsafe {
                let _ = CloseHandle(token);
            }
            return Err(e);
        }
    };
    report.note("canary.spawn_path", spawn_path);
    report.note("canary.pid", child.pid);

    // A generous but finite wait. A hang here is itself a result worth seeing,
    // and INFINITE would turn it into a CI timeout with no log at all.
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut exit: Option<u32> = None;
    while Instant::now() < deadline {
        let r = unsafe { WaitForSingleObject(child.process, 1000) };
        if r.0 == 0 {
            let mut code = 0u32;
            let _ = unsafe { GetExitCodeProcess(child.process, &mut code) };
            exit = Some(code);
            break;
        }
    }
    if exit.is_none() {
        report.note(
            "canary.timeout",
            "child did not exit within 120s; terminating",
        );
        unsafe {
            let _ = TerminateProcess(child.process, 1);
        }
    }

    // Give the kernel's net-event lane a moment; the subscription callback is
    // asynchronous and a drop can land after the connect() has already
    // returned to the caller.
    std::thread::sleep(Duration::from_millis(1500));

    // Prefer the side channel, fall back to the inherited handle, and say
    // which one carried the evidence. Silently picking one would hide a
    // fallback spawn path that produced nothing.
    let mut out = read_or_empty(&side_path);
    let mut source = "side-channel";
    if out.trim().is_empty() {
        out = read_or_empty(&log_path);
        source = "inherited-stdout";
    }
    report.note(
        "canary.output_source",
        format!("{source} ({} bytes)", out.len()),
    );
    child.close();

    for line in out.lines() {
        println!("SPIKE|CHILD| {line}");
    }
    Ok(StageOutcome {
        child_stdout: out,
        child_exit: exit,
    })
}

fn read_or_empty(p: &Path) -> String {
    let mut s = String::new();
    if let Ok(mut f) = std::fs::File::open(p) {
        let _ = f.read_to_string(&mut s);
    }
    s
}

/// Parse one `CANARY|<label>|<target>|<CONNECTED|REFUSED>|os_error=<n>` line.
pub fn probe_result(stdout: &str, label: &str) -> Option<(bool, i32)> {
    for line in stdout.lines() {
        let p: Vec<&str> = line.trim().split('|').collect();
        if p.len() >= 5 && p[0] == "CANARY" && p[1] == label {
            let connected = p[3] == "CONNECTED";
            let code = p[4]
                .strip_prefix("os_error=")
                .and_then(|v| v.parse().ok())?;
            return Some((connected, code));
        }
    }
    None
}

/// Days 1–3 plus day 4, in one pass over one boundary.
pub fn stage_core(report: &mut Report, enforce: bool, tag: &str) {
    let nic = crate::oracle::primary_ipv4();
    report.note("oracle.primary_ipv4", nic);
    report.note(
        "oracle.primary_is_loopback",
        (nic == Ipv4Addr::LOCALHOST).to_string(),
    );

    let dst_declared = match crate::oracle::tcp_destination("declared", IpAddr::V4(nic)) {
        Ok(l) => l,
        Err(e) => {
            report.not_run(
                "core",
                "destination oracle bound",
                format!("bind failed: {e}"),
            );
            return;
        }
    };
    let dst_undeclared = match crate::oracle::tcp_destination("undeclared", IpAddr::V4(nic)) {
        Ok(l) => l,
        Err(e) => {
            report.not_run(
                "core",
                "destination oracle bound",
                format!("bind failed: {e}"),
            );
            return;
        }
    };
    let dst_loopback = match crate::oracle::tcp_destination(
        "loopback-undeclared",
        IpAddr::V4(Ipv4Addr::LOCALHOST),
    ) {
        Ok(l) => l,
        Err(e) => {
            report.not_run("core", "loopback oracle bound", format!("bind failed: {e}"));
            return;
        }
    };
    let dst_udp = match crate::oracle::udp_destination("udp-undeclared", IpAddr::V4(nic)) {
        Ok(l) => l,
        Err(e) => {
            report.not_run("core", "udp oracle bound", format!("bind failed: {e}"));
            return;
        }
    };
    report.note("oracle.declared", dst_declared.addr);
    report.note("oracle.undeclared", dst_undeclared.addr);
    report.note("oracle.loopback_undeclared", dst_loopback.addr);
    report.note("oracle.udp_undeclared", dst_udp.addr);

    let declared = [Declared {
        addr: nic,
        port: dst_declared.addr.port(),
        protocol: IPPROTO_TCP_U8,
    }];

    let mut boundary = match establish(tag, &declared, enforce, report) {
        Ok(b) => b,
        Err(e) => {
            report.not_run("core", "boundary established", e.to_string());
            return;
        }
    };

    let sub =
        match netevents::Subscription::start(boundary.engine.handle, windows::core::GUID::zeroed())
        {
            Ok(s) => {
                report.note("netevent.subscribe", "ok");
                Some(s)
            }
            Err(e) => {
                report.note("netevent.subscribe.error", e.to_string());
                None
            }
        };

    let workdir = match WorkDir::prepare(&boundary.identity.name, report) {
        Ok(w) => w,
        Err(e) => {
            report.not_run("core", "workdir prepared", e.to_string());
            return;
        }
    };

    let args: Vec<String> = vec![
        "canary".into(),
        "--prefix".into(),
        "child".into(),
        "--declared".into(),
        dst_declared.addr.to_string(),
        "--undeclared".into(),
        dst_undeclared.addr.to_string(),
        "--udp-undeclared".into(),
        dst_udp.addr.to_string(),
        "--external".into(),
        EXTERNAL_UNDECLARED.into(),
        "--spawn-grandchild".into(),
    ];

    let outcome = match run_canary(&boundary, &workdir, &args, report) {
        Ok(o) => o,
        Err(e) => {
            report.not_run("core", "canary ran inside the boundary", e.to_string());
            return;
        }
    };
    report.note("canary.exit", format!("{:?}", outcome.child_exit));

    // The canary must be shown to have actually run, before any of its results
    // mean anything. Honesty rule 1: a usage error would also produce no
    // successful connection, and would satisfy a bare "it was blocked" check.
    let ran = outcome.child_stdout.contains("CANARY|DONE|prefix=child");
    report.assert_obs(
        "canary.actually-ran",
        "child prints CANARY|DONE",
        if ran { "present" } else { "ABSENT" },
        ran,
    );
    if !ran {
        report.not_run(
            "core.all-probes",
            "probe results",
            "canary never completed; no probe result can be trusted",
        );
        return;
    }
    let whoami_is_run_identity = outcome
        .child_stdout
        .lines()
        .any(|l| l.starts_with("CANARY|WHOAMI|child|") && l.contains(&boundary.identity.name));
    report.assert_obs(
        "canary.runs-as-run-identity",
        format!("USERNAME == {}", boundary.identity.name),
        outcome
            .child_stdout
            .lines()
            .find(|l| l.starts_with("CANARY|WHOAMI|child|"))
            .unwrap_or("<no WHOAMI line>"),
        whoami_is_run_identity,
    );

    let prefix = if enforce { "core" } else { "negative" };

    // --- the declared destination must still work ---
    if let Some((connected, code)) = probe_result(&outcome.child_stdout, "child.tcp.declared") {
        report.assert_obs(
            format!("{prefix}.declared.client"),
            "CONNECTED",
            format!("connected={connected} os_error={code}"),
            connected,
        );
        let seen = dst_declared.sightings.count();
        report.assert_obs(
            format!("{prefix}.declared.oracle"),
            "destination saw >=1 connection",
            format!("sightings={seen} {:?}", dst_declared.sightings.all()),
            seen >= 1,
        );
    } else {
        report.not_run(
            format!("{prefix}.declared"),
            "declared probe result",
            "no CANARY line for child.tcp.declared",
        );
    }

    // --- the undeclared destination must not ---
    if let Some((connected, code)) = probe_result(&outcome.child_stdout, "child.tcp.undeclared") {
        report.assert_obs(
            format!("{prefix}.undeclared.client"),
            if enforce {
                "REFUSED"
            } else {
                "CONNECTED (enforcement deliberately off)"
            },
            format!("connected={connected} os_error={code}"),
            if enforce { !connected } else { connected },
        );
        report.note("blocked-connect.os_error", code);
        let seen = dst_undeclared.sightings.count();
        report.assert_obs(
            format!("{prefix}.undeclared.oracle"),
            if enforce {
                "destination saw 0 connections"
            } else {
                "destination saw >=1"
            },
            format!("sightings={seen} {:?}", dst_undeclared.sightings.all()),
            if enforce { seen == 0 } else { seen >= 1 },
        );
    } else {
        report.not_run(
            format!("{prefix}.undeclared"),
            "undeclared probe result",
            "no CANARY line for child.tcp.undeclared",
        );
    }

    // --- a grandchild is equally contained ---
    if let Some((connected, code)) =
        probe_result(&outcome.child_stdout, "grandchild.tcp.undeclared")
    {
        report.assert_obs(
            format!("{prefix}.grandchild.client"),
            if enforce { "REFUSED" } else { "CONNECTED" },
            format!("connected={connected} os_error={code}"),
            if enforce { !connected } else { connected },
        );
    } else {
        report.not_run(
            format!("{prefix}.grandchild"),
            "grandchild probe result",
            "no CANARY line for grandchild.tcp.undeclared - grandchild may not have spawned",
        );
    }

    // --- UDP to an undeclared host ---
    if let Some((connected, code)) = probe_result(&outcome.child_stdout, "child.udp.undeclared") {
        report.assert_obs(
            format!("{prefix}.udp.client"),
            if enforce {
                "send_to fails"
            } else {
                "send_to succeeds"
            },
            format!("sent={connected} os_error={code}"),
            if enforce { !connected } else { connected },
        );
        let seen = dst_udp.sightings.count();
        report.assert_obs(
            format!("{prefix}.udp.oracle"),
            if enforce {
                "destination received 0 datagrams"
            } else {
                ">=1"
            },
            format!("sightings={seen} {:?}", dst_udp.sightings.all()),
            if enforce { seen == 0 } else { seen >= 1 },
        );
    }

    // --- external destination, client-side observation only ---
    if let Some((connected, code)) = probe_result(&outcome.child_stdout, "child.tcp.external") {
        report.assert_obs(
            format!("{prefix}.external.client-side-only"),
            if enforce { "REFUSED" } else { "any" },
            format!("connected={connected} os_error={code} (NO destination-side oracle)"),
            if enforce { !connected } else { true },
        );
    }

    // --- day 4: the audit lane ---
    let live = netevents::snapshot();
    report.note("netevent.live.count", live.len());
    for r in live.iter().take(40) {
        println!("SPIKE|NETEVENT-LIVE| {}", r.line());
    }
    match netevents::enumerate() {
        Ok(all) => {
            report.note("netevent.enum.count", all.len());
            for r in all.iter().rev().take(40) {
                println!("SPIKE|NETEVENT-ENUM| {}", r.line());
            }
            let mine: Vec<_> = all
                .iter()
                .chain(live.iter())
                .filter(|r| r.user_sid.as_deref() == Some(boundary.identity.sid_string.as_str()))
                .collect();
            report.note("netevent.attributed-to-run-identity", mine.len());
            let drop_with_addr = mine.iter().find(|r| {
                r.kind == "classify-drop" && r.remote_addr.is_some() && r.remote_port.is_some()
            });
            report.assert_obs(
                format!("{prefix}.audit.drop-carries-address-and-port"),
                "a classify-drop attributed to the run SID, with remote address and port",
                match drop_with_addr {
                    Some(r) => r.line(),
                    None => format!(
                        "no such record ({} attributed records, {} total)",
                        mine.len(),
                        all.len() + live.len()
                    ),
                },
                drop_with_addr.is_some() == enforce,
            );
        }
        Err(e) => report.note("netevent.enum.error", e.to_string()),
    }

    dst_declared.stop();
    dst_undeclared.stop();
    dst_loopback.stop();
    dst_udp.stop();

    // --- teardown through the normal path ---
    let sublayer = boundary.sublayer;
    drop(sub);
    boundary.engine.close();
    match Engine::count_filters_in_sublayer(sublayer) {
        Ok(n) => report.assert_obs(
            format!("{prefix}.teardown.clean-close"),
            "0 filters remain in the sublayer after the engine handle closes",
            format!("{n} remain"),
            n == 0,
        ),
        Err(e) => report.not_run(
            format!("{prefix}.teardown.clean-close"),
            "filter count after close",
            e.to_string(),
        ),
    }

    match boundary.identity.delete() {
        Ok(()) => report.note("identity.deleted", "ok"),
        Err(e) => report.note("identity.delete.error", e.to_string()),
    }
}

/// Day 6 — cleanup after an abruptly killed supervisor.
///
/// The dynamic session is supposed to make this automatic. "Supposed to" is
/// exactly the kind of claim this spike exists to check, so the supervisor is
/// run as a real subprocess and killed outright rather than asked politely.
pub fn stage_abrupt_kill(report: &mut Report, exe: &Path) {
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("hold");
    cmd.stdout(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            report.not_run("teardown.abrupt", "held supervisor spawned", e.to_string());
            return;
        }
    };

    // Read until the holder says the filters are in. Reading is what proves
    // the filters existed *before* the kill; killing on a timer would leave
    // "0 filters remain" ambiguous between clean teardown and never-added.
    let mut sublayer = String::new();
    let mut filters = 0usize;
    let mut held_identity = String::new();
    if let Some(out) = child.stdout.as_mut() {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            match out.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    buf.push(byte[0]);
                    if byte[0] == b'\n' {
                        let line = String::from_utf8_lossy(&buf).trim().to_string();
                        println!("SPIKE|HOLDER| {line}");
                        if let Some(rest) = line.strip_prefix("HOLD|READY|") {
                            let mut it = rest.split('|');
                            sublayer = it.next().unwrap_or_default().to_string();
                            filters = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                            held_identity = it.next().unwrap_or_default().to_string();
                            break;
                        }
                        buf.clear();
                    }
                }
                Err(_) => break,
            }
        }
    }

    if sublayer.is_empty() {
        let _ = child.kill();
        report.not_run(
            "teardown.abrupt",
            "held supervisor reached HOLD|READY",
            "no READY line; see SPIKE|HOLDER lines above",
        );
        return;
    }
    report.note("teardown.abrupt.sublayer", &sublayer);
    report.assert_obs(
        "teardown.abrupt.filters-existed-first",
        ">0 filters present before the kill",
        format!("{filters} filters"),
        filters > 0,
    );

    // TerminateProcess, not Ctrl-C: no destructor runs, no cleanup code gets a
    // chance. Only the kernel's own dynamic-session teardown can save this.
    let _ = child.kill();
    let _ = child.wait();
    std::thread::sleep(Duration::from_millis(1500));

    let guid = sublayer
        .parse::<u128>()
        .ok()
        .map(windows::core::GUID::from_u128);
    match guid {
        Some(g) => match Engine::count_filters_in_sublayer(g) {
            Ok(n) => report.assert_obs(
                "teardown.abrupt.filters-gone",
                "0 filters remain after the supervisor is killed outright",
                format!("{n} remain"),
                n == 0,
            ),
            Err(e) => report.not_run(
                "teardown.abrupt.filters-gone",
                "filter count after kill",
                e.to_string(),
            ),
        },
        None => report.not_run(
            "teardown.abrupt.filters-gone",
            "filter count after kill",
            format!("could not parse sublayer key {sublayer:?}"),
        ),
    }

    // The holder leaked its identity on purpose (see `hold`), so it is deleted
    // from out here. A leaked local user on a throwaway runner is harmless, but
    // leaving one behind would still be sloppy.
    if !held_identity.is_empty() {
        let out = std::process::Command::new("net")
            .args(["user", &held_identity, "/delete"])
            .output();
        report.note(
            "teardown.abrupt.identity-cleanup",
            match out {
                Ok(o) => format!(
                    "status={:?} {}",
                    o.status.code(),
                    String::from_utf8_lossy(&o.stdout).trim()
                ),
                Err(e) => format!("spawn failed: {e}"),
            },
        );
    }
}

/// The `hold` mode used by [`stage_abrupt_kill`]: set the boundary up, announce
/// it, then block forever waiting to be killed.
///
/// The sublayer key travels as a `u128` rather than as formatted hex — the
/// round trip is exact, and a GUID parser written for one log line is a place
/// for a bug that would look like a containment result.
pub fn hold() -> i32 {
    let mut report = Report::new();
    let nic = crate::oracle::primary_ipv4();
    let declared = [Declared {
        addr: nic,
        port: 443,
        protocol: IPPROTO_TCP_U8,
    }];
    match establish("hold", &declared, true, &mut report) {
        Ok(b) => {
            let n = Engine::count_filters_in_sublayer(b.sublayer).unwrap_or(0);
            println!(
                "HOLD|READY|{}|{}|{}",
                b.sublayer.to_u128(),
                n,
                b.identity.name
            );
            use std::io::Write;
            let _ = std::io::stdout().flush();
            // Deliberately leaked: a destructor running here would tear the
            // filters down through the normal path and answer a different
            // question than the one being asked. The identity is leaked with
            // it, and the caller deletes it by name afterwards.
            std::mem::forget(b);
            std::thread::sleep(Duration::from_secs(3600));
            0
        }
        Err(e) => {
            println!("HOLD|FAILED|{e}");
            1
        }
    }
}

/// Days 7–9 — the identity boundary.
///
/// Days 1–6 succeeding proves little; the prior research pass already put the
/// WFP mechanism at ~90% and the fused claim at ~40%. This stage is the spike.
pub fn stage_gui(report: &mut Report) {
    // A GUI app owned by the *original* user, for question 2. Opened on a file
    // with a unique name so its window title cannot collide with the one the
    // contained identity launches for question 1.
    let marker = format!("fpforeign{}", std::process::id());
    let foreign_path = std::env::temp_dir().join(format!("{marker}.txt"));
    if let Err(e) = std::fs::write(&foreign_path, b"foreign-owned document\r\n") {
        report.note("gui.foreign.write_error", e.to_string());
    }
    let foreign = std::process::Command::new("notepad.exe")
        .arg(&foreign_path)
        .spawn();
    match &foreign {
        Ok(c) => report.note("gui.foreign.pid", c.id()),
        Err(e) => report.note("gui.foreign.spawn_error", e.to_string()),
    }
    // Give the foreign window time to exist before anything looks for it;
    // "not found" has to mean refused, not "not yet started".
    std::thread::sleep(Duration::from_secs(5));
    report.note(
        "gui.foreign.token_sid",
        foreign
            .as_ref()
            .ok()
            .and_then(|c| super::gui::token_sid_of(c.id()))
            .unwrap_or_else(|| "<unknown>".into()),
    );

    let nic = crate::oracle::primary_ipv4();
    let declared = [Declared {
        addr: nic,
        port: 443,
        protocol: IPPROTO_TCP_U8,
    }];
    let mut boundary = match establish("gui", &declared, true, report) {
        Ok(b) => b,
        Err(e) => {
            report.not_run("gui", "boundary established", e.to_string());
            return;
        }
    };

    let workdir = match WorkDir::prepare(&boundary.identity.name, report) {
        Ok(w) => w,
        Err(e) => {
            report.not_run("gui", "workdir prepared", e.to_string());
            return;
        }
    };

    let args: Vec<String> = vec![
        "gui-inside".into(),
        "--launch-and-drive".into(),
        "--foreign-title".into(),
        marker.clone(),
    ];
    let outcome = match run_canary(&boundary, &workdir, &args, report) {
        Ok(o) => o,
        Err(e) => {
            report.not_run("gui", "gui probe ran inside the boundary", e.to_string());
            return;
        }
    };

    let out = &outcome.child_stdout;
    let ran = out.contains("GUI|DONE|gui-inside");
    report.assert_obs(
        "gui.probe-actually-ran",
        "child prints GUI|DONE",
        if ran { "present" } else { "ABSENT" },
        ran,
    );

    let uia_ok = out.contains("GUI|UIA-INIT|ok");
    report.assert_obs(
        "gui.uia-client-initialises-under-run-identity",
        "UIAutomation::new() succeeds",
        line_for(out, "GUI|UIA-INIT").unwrap_or_else(|| "<no line>".into()),
        uia_ok,
    );

    // --- question 1: drive a GUI app it launched itself. The kill criterion. ---
    let q1_window = out.contains("GUI|Q1-WINDOW-FOUND|");
    let q1_readback = out
        .lines()
        .find(|l| l.starts_with("GUI|Q1-READBACK|"))
        .map(|l| l.to_string());
    let q1_drove = q1_readback
        .as_deref()
        .map(|l| l.contains("contains_marker=true"))
        .unwrap_or(false);
    report.assert_obs(
        "gui.q1.window-found",
        "the identity sees the window it launched",
        line_for(out, "GUI|Q1-WINDOW-FOUND")
            .or_else(|| line_for(out, "GUI|Q1-WINDOW-NOT-FOUND"))
            .unwrap_or_else(|| "<no line>".into()),
        q1_window,
    );
    report.assert_obs(
        "gui.q1.KILL-CRITERION.drives-own-gui-app",
        "text typed via UIA reads back from the app it launched",
        q1_readback.unwrap_or_else(|| "<no readback line>".into()),
        q1_drove,
    );
    report.note(
        "gui.q1.verdict",
        if q1_drove {
            "question 1 PASSES - the fused claim survives this stage"
        } else {
            "question 1 FAILS - kill criterion; the fused claim is dead here"
        },
    );

    // --- question 2: reach across the boundary. Expected NO. ---
    let q2_found = out.contains("GUI|Q2-WINDOW-FOUND|");
    let q2_drove = line_for(out, "GUI|Q2-SEND-TEXT")
        .map(|l| l.contains("contains_marker=true"))
        .unwrap_or(false);
    report.assert_obs(
        "gui.q2.cross-identity-refused",
        "the foreign-owned window cannot be driven (expected NO)",
        format!(
            "found={q2_found} drove={q2_drove} | {} | {}",
            line_for(out, "GUI|Q2-WINDOW-FOUND")
                .or_else(|| line_for(out, "GUI|Q2-WINDOW-NOT-FOUND"))
                .unwrap_or_else(|| "<no window line>".into()),
            line_for(out, "GUI|Q2-SEND-TEXT")
                .or_else(|| line_for(out, "GUI|Q2-SEND-TEXT-FAILED"))
                .or_else(|| line_for(out, "GUI|Q2-EDIT-NOT-FOUND"))
                .unwrap_or_else(|| "<no send line>".into())
        ),
        !q2_drove,
    );

    // --- question 3: the GUI app is inside the containment scope ---
    let app_sid = line_for(out, "GUI|Q3-GUI-APP-TOKEN-SID");
    let same_sid = app_sid
        .as_deref()
        .map(|l| l.contains(&boundary.identity.sid_string))
        .unwrap_or(false);
    report.assert_obs(
        "gui.q3.launched-app-carries-run-identity-sid",
        format!(
            "the GUI app's token user SID == {} (WFP scopes by exactly this)",
            boundary.identity.sid_string
        ),
        app_sid.unwrap_or_else(|| "<no line>".into()),
        same_sid,
    );

    if let Ok(mut c) = foreign {
        let _ = c.kill();
        let _ = c.wait();
    }
    let _ = std::fs::remove_file(&foreign_path);
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/IM", "notepad.exe"])
        .output();

    boundary.engine.close();
    match boundary.identity.delete() {
        Ok(()) => report.note("gui.identity.deleted", "ok"),
        Err(e) => report.note("gui.identity.delete.error", e.to_string()),
    }
}

fn line_for(haystack: &str, prefix: &str) -> Option<String> {
    haystack
        .lines()
        .find(|l| l.starts_with(prefix))
        .map(|l| l.trim().to_string())
}

/// Suppress warnings for items kept deliberately for later stages.
#[allow(dead_code)]
fn _keep() {
    let _: Option<SocketAddr> = None;
    let _ = wfp::IPPROTO_UDP_U8;
    let _ = wfp::IPPROTO_TCP_U8;
    let _ = is_elevated;
    let _: Option<PWSTR> = None;
    let _ = INFINITE;
}
