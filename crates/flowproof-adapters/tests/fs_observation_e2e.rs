//! Filesystem observation against a real kernel: does the filter install, does
//! it trap what it should, and does CONTINUE actually let the syscall run?
//!
//! The unit tests in `egress_linux` read the filter as DATA, which is what
//! makes them cheap and portable and also what they cannot prove: a BPF
//! program with a wrong jump offset is still well-formed data, and only a
//! kernel will reject it. On rejection `seccomp()` fails inside `pre_exec`, so
//! EVERY contained flow stops spawning - a failure the Linux E2E suites would
//! catch except that they are off the pull-request path. Hence one cheap live
//! witness here, negative half included: an ordinary read and append must NOT
//! appear, or the in-kernel `JSET O_TRUNC` test is not doing its job and every
//! log line of a contained run is paying for a supervisor round-trip.

#![cfg(all(target_os = "linux", feature = "agent"))]

use std::process::Command;
use std::time::Instant;

use flowproof_adapters::egress::AllowSet;
use flowproof_adapters::egress_linux;

#[test]
fn the_filter_installs_and_observes_without_preventing() {
    let dir = std::env::temp_dir().join(format!("flowproof-fs-obs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let (gone, clobbered, moved) = (
        dir.join("gone.txt"),
        dir.join("clobbered.txt"),
        dir.join("moved.txt"),
    );
    std::fs::write(&gone, "delete me").expect("fixture");
    std::fs::write(&clobbered, "old contents").expect("fixture");
    std::fs::write(dir.join("source.txt"), "move me").expect("fixture");

    // Only `/bin/sh` and the two binaries every image with a shell has. The
    // first command is the NEGATIVE control: a read and an append carry no
    // O_TRUNC, so neither may reach the supervisor.
    let script = format!(
        "cd {d}; cat gone.txt >> appended.txt; rm gone.txt; \
         echo new > clobbered.txt; mv source.txt moved.txt",
        d = dir.display()
    );
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(&script);
    let prep = egress_linux::install(&mut cmd, &AllowSet::default()).expect("filter prepared");
    let spawned = Instant::now();
    let mut child = cmd.spawn().expect("the filter must not break spawn");
    let supervisor = prep.into_supervisor(spawned).expect("supervisor started");
    let status = child.wait().expect("child waited on");
    let (_egress, fs) = supervisor.stop_and_collect();

    let report = fs.report_lines().join("\n");
    assert!(
        status.success(),
        "CONTINUE means the shell still works: {status:?}\n{report}"
    );
    assert!(fs.faults.is_empty(), "unadjudicated syscalls:\n{report}");

    // The CONTINUE half: had the supervisor denied these, the shell would have
    // reported errors and the files would still be sitting here.
    assert!(!gone.exists(), "the unlink was observed AND performed");
    assert!(moved.exists(), "the rename was observed AND performed");
    assert_eq!(
        std::fs::read_to_string(&clobbered).expect("readable"),
        "new\n",
        "the redirect truncated the old contents away, which is the O_TRUNC"
    );

    for want in ["unlink", "rename", "open"] {
        assert!(
            fs.destructive.iter().any(|e| e.op.contains(want)),
            "no {want} observed:\n{report}"
        );
    }
    let paths: Vec<&str> = fs
        .destructive
        .iter()
        .filter_map(|e| e.path.as_deref())
        .collect();
    // Absolute, which is `/proc/<tid>/cwd` doing its job: the shell `cd`'d, so
    // every path it passed was relative.
    assert!(
        paths.contains(&gone.display().to_string().as_str()),
        "the deleted file is named absolutely:\n{report}"
    );
    // The negative control: neither the read nor the append carried O_TRUNC,
    // so neither may have woken the supervisor.
    assert!(
        !paths.iter().any(|p| p.contains("appended.txt")),
        "an append and a read must not trap; the JSET O_TRUNC gate is not \
         holding, and a contained run now pays a round-trip per log line:\n{report}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
