//! `flowproof config skill` end to end through `run_cli`, per
//! plans/003-agent-config-skill.md's "Getting the skill into the end user's
//! project".
//!
//! The current directory is process-global, so — like `config_cli_e2e.rs`'s
//! `HOME` — everything here runs under one lock rather than side by side.
#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Mutex;

static ENV: Mutex<()> = Mutex::new(());

fn temp_project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-config-skill-cli-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("project dir");
    dir
}

fn with_fake_cwd<T>(project: &std::path::Path, body: impl FnOnce() -> T) -> T {
    let previous = std::env::current_dir().expect("current dir");
    std::env::set_current_dir(project).expect("set cwd");
    let result = body();
    std::env::set_current_dir(previous).expect("restore cwd");
    result
}

#[test]
fn config_skill_with_no_flags_writes_both_defaults() {
    let _guard = ENV.lock().expect("env lock");
    let project = temp_project("both-defaults");

    with_fake_cwd(&project, || {
        let code = flowproof_cli::run_cli(["config", "skill"]);
        assert_eq!(code, 0);
    });

    let claude = project.join(".claude/skills/flowproof-config/SKILL.md");
    let agents = project.join(".agents/skills/flowproof-config/SKILL.md");
    assert!(claude.is_file(), "{claude:?} must exist");
    assert!(agents.is_file(), "{agents:?} must exist");
    let claude_text = std::fs::read_to_string(&claude).expect("read claude skill");
    let agents_text = std::fs::read_to_string(&agents).expect("read agents skill");
    assert_eq!(claude_text, agents_text, "both copies are byte-identical");
    assert!(claude_text.starts_with("---\nname: flowproof-config"));

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn config_skill_rerun_is_an_idempotent_no_op() {
    let _guard = ENV.lock().expect("env lock");
    let project = temp_project("idempotent");

    with_fake_cwd(&project, || {
        assert_eq!(flowproof_cli::run_cli(["config", "skill"]), 0, "first run");
        assert_eq!(
            flowproof_cli::run_cli(["config", "skill"]),
            0,
            "second run is a no-op, not an error"
        );
    });

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn config_skill_refuses_a_differing_existing_file_without_force() {
    let _guard = ENV.lock().expect("env lock");
    let project = temp_project("conflict");
    let claude_dir = project.join(".claude/skills/flowproof-config");
    std::fs::create_dir_all(&claude_dir).expect("mkdir");
    std::fs::write(claude_dir.join("SKILL.md"), "hand-edited content").expect("seed file");

    with_fake_cwd(&project, || {
        let code = flowproof_cli::run_cli(["config", "skill"]);
        assert_ne!(code, 0, "must refuse rather than clobber");

        let code = flowproof_cli::run_cli(["config", "skill", "--force"]);
        assert_eq!(code, 0, "--force overwrites");
    });

    let claude_text =
        std::fs::read_to_string(project.join(".claude/skills/flowproof-config/SKILL.md"))
            .expect("read claude skill");
    assert!(claude_text.starts_with("---\nname: flowproof-config"));

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn config_skill_claude_and_agents_flags_select_one_target_each() {
    let _guard = ENV.lock().expect("env lock");
    let project = temp_project("select-one");

    with_fake_cwd(&project, || {
        assert_eq!(flowproof_cli::run_cli(["config", "skill", "--claude"]), 0);
    });
    assert!(project
        .join(".claude/skills/flowproof-config/SKILL.md")
        .is_file());
    assert!(!project
        .join(".agents/skills/flowproof-config/SKILL.md")
        .exists());

    with_fake_cwd(&project, || {
        assert_eq!(flowproof_cli::run_cli(["config", "skill", "--agents"]), 0);
    });
    assert!(project
        .join(".agents/skills/flowproof-config/SKILL.md")
        .is_file());

    std::fs::remove_dir_all(&project).ok();
}

#[test]
fn config_skill_dir_writes_an_arbitrary_extra_target() {
    let _guard = ENV.lock().expect("env lock");
    let project = temp_project("custom-dir");

    with_fake_cwd(&project, || {
        let code = flowproof_cli::run_cli(["config", "skill", "--dir", ".github/skills"]);
        assert_eq!(code, 0);
    });

    // --dir is additive: the two conventional defaults still land too.
    assert!(project
        .join(".claude/skills/flowproof-config/SKILL.md")
        .is_file());
    assert!(project
        .join(".agents/skills/flowproof-config/SKILL.md")
        .is_file());
    assert!(project
        .join(".github/skills/flowproof-config/SKILL.md")
        .is_file());

    std::fs::remove_dir_all(&project).ok();
}
