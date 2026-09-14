//! Every user-facing flag has to be written down somewhere a user reads.
//!
//! A flag lands with its code and its `--help` line, and both of those are
//! easy to ship without touching prose. The gap does not announce itself:
//! the feature works, CI is green, and the only symptom is an adopter who
//! cannot find the thing that exists. `--trace`, `--out` and `doctor
//! --prompt` all reached a release that way.
//!
//! So the docs are checked the way the schema is: mechanically, against the
//! definition. The corpus is every `docs/**/*.md` plus `README.md` — what
//! the website renders (topic pages now live in per-topic folders, e.g.
//! `docs/getting-started/*.md`) and what the repository greets you with.
//! Mentioning a flag is a low bar deliberately; this catches *absent*, not
//! *badly explained*.

use clap::CommandFactory;
use flowproof_cli::Cli;

/// Collects every `.md` file under `dir`, recursing into subfolders.
fn collect_md_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable docs dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_md_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
}

/// `docs/**/*.md` + `README.md`, concatenated. These are the pages a user can
/// actually reach: automators.ai renders `docs/` at build time.
fn documentation() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");

    let mut corpus = std::fs::read_to_string(root.join("README.md")).expect("README.md");

    let mut pages = Vec::new();
    collect_md_files(&root.join("docs"), &mut pages);
    // Read in a stable order so a failure names the same corpus every run.
    pages.sort();
    assert!(
        !pages.is_empty(),
        "no docs/**/*.md found under {}",
        root.display()
    );

    for page in pages {
        corpus.push_str(&std::fs::read_to_string(&page).expect("doc page"));
    }
    corpus
}

/// Flags clap generates for itself. They are not ours to document.
const BUILT_IN: [&str; 2] = ["help", "version"];

#[test]
fn every_visible_cli_flag_is_mentioned_in_the_docs() {
    let docs = documentation();
    let cli = Cli::command();
    let mut undocumented = Vec::new();

    for subcommand in cli.get_subcommands() {
        // `mcp-stdio` is `hide = true`: flowproof spawns it, nobody types it.
        // Documenting it would invite exactly the hand-running its help text
        // warns against.
        if subcommand.is_hide_set() {
            continue;
        }
        for arg in subcommand.get_arguments() {
            let Some(long) = arg.get_long() else {
                continue;
            };
            if arg.is_hide_set() || BUILT_IN.contains(&long) {
                continue;
            }
            if !docs.contains(&format!("--{long}")) {
                undocumented.push(format!("{} --{long}", subcommand.get_name()));
            }
        }
    }

    assert!(
        undocumented.is_empty(),
        "these flags exist but no page mentions them: {}\n\
         Add them to docs/getting-started.md (or the page that owns the \
         command) — a flag a user cannot find is a flag that did not ship.",
        undocumented.join(", ")
    );
}

/// The subcommand names themselves, held to the same bar. A whole command can
/// go unwritten the same way a flag can: `doctor` was reachable only from a
/// design document for several releases.
#[test]
fn every_visible_subcommand_is_mentioned_in_the_docs() {
    let docs = documentation();
    let cli = Cli::command();
    let mut undocumented = Vec::new();

    for subcommand in cli.get_subcommands() {
        if subcommand.is_hide_set() {
            continue;
        }
        let name = subcommand.get_name();
        if BUILT_IN.contains(&name) {
            continue;
        }
        if !docs.contains(&format!("flowproof {name}")) {
            undocumented.push(name.to_string());
        }
    }

    assert!(
        undocumented.is_empty(),
        "these subcommands are never shown as `flowproof <name>` in the \
         docs: {}",
        undocumented.join(", ")
    );
}
