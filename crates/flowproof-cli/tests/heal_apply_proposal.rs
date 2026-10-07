//! `heal --apply-proposal` puts the reviewed proposal in place without
//! recording again, so what gets applied is exactly what was reviewed.

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "flowproof-apply-proposal-{name}-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn apply_proposal_replaces_the_trace_with_the_proposal() {
    let dir = scratch("applies");
    let spec = dir.join("order.flow.yaml");
    std::fs::write(&spec, "name: order\napp: web\nsteps:\n  - Press Save\n").expect("spec");
    std::fs::write(dir.join("order.trace.jsonl"), "recorded\n").expect("trace");
    std::fs::write(dir.join("order.proposed.jsonl"), "reviewed\n").expect("proposal");

    let args = ["heal", spec.to_str().expect("path"), "--apply-proposal"];
    assert_eq!(flowproof_cli::run_cli(args), 0);
    assert_eq!(
        std::fs::read_to_string(dir.join("order.trace.jsonl")).expect("trace"),
        "reviewed\n"
    );
    assert!(!dir.join("order.proposed.jsonl").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn apply_proposal_without_a_proposal_fails_and_keeps_the_trace() {
    let dir = scratch("missing");
    let spec = dir.join("order.flow.yaml");
    std::fs::write(&spec, "name: order\napp: web\nsteps:\n  - Press Save\n").expect("spec");
    std::fs::write(dir.join("order.trace.jsonl"), "recorded\n").expect("trace");

    let args = ["heal", spec.to_str().expect("path"), "--apply-proposal"];
    assert_ne!(flowproof_cli::run_cli(args), 0);
    assert_eq!(
        std::fs::read_to_string(dir.join("order.trace.jsonl")).expect("trace"),
        "recorded\n"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn apply_proposal_cannot_be_combined_with_apply_or_from_run() {
    for extra in [&["--apply"][..], &["--from-run", "run-dir"][..]] {
        let mut args = vec!["heal", "order.flow.yaml", "--apply-proposal"];
        args.extend_from_slice(extra);
        assert_eq!(flowproof_cli::run_cli(args), 2, "{extra:?}");
    }
}
