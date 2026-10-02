//! Draft a `.flow.yaml` spec from an outcome and the app's own source code.
//!
//! Exploring the live app toward an outcome means guessing how it is
//! navigated; the code says so outright (routes, labels, where a status is
//! rendered). One model call picks the files worth reading, a second drafts
//! steps from them, each citing the `path:line` it rests on so a reviewer can
//! check it. What the code cannot settle (sign-in, existing records) is flagged.

use std::path::Path;

use crate::draft_assembly::DraftLine;
use crate::llm::ModelClient;
use crate::AgentError;

/// How many files are listed for the model to choose from, and picked.
const MAX_LISTED: usize = 3000;
const MAX_PICKED: usize = 12;

/// Where UI text and navigation live: markup, components, routes, views and
/// message bundles. Anything else is not listed.
const SOURCE_EXTENSIONS: &str = "ts tsx js jsx mjs vue svelte astro html htm py rb erb go java \
     kt cs cshtml razor php twig hbs j2 jinja xml properties";
const SKIPPED_DIRS: &str = "node_modules dist build out target vendor coverage __pycache__";

#[derive(Debug, thiserror::Error)]
pub enum CodeAuthorError {
    #[error("nothing to read under '{0}': no source files, or the model picked none")]
    NothingToRead(String),
    #[error("no steps were drafted from the code - nothing to draft")]
    NoStepsInferred,
    #[error("model produced a draft that does not parse as a flow spec: {0}")]
    DraftInvalid(#[from] crate::spec::SpecError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("io error at '{path}': {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

fn io_err(path: &Path, source: std::io::Error) -> CodeAuthorError {
    CodeAuthorError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// The line of code a drafted step rests on, relative to the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRef {
    pub file: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeDraftLine {
    pub line: DraftLine,
    pub source: Option<SourceRef>,
}

/// Root `.gitignore` entries, kept simple: a name or path prefix, or `*.ext`.
fn ignore_patterns(repo: &Path) -> Vec<String> {
    std::fs::read_to_string(repo.join(".gitignore"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().trim_start_matches('/').trim_end_matches('/'))
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        .map(str::to_string)
        .collect()
}

fn ignored(rel: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| match p.strip_prefix('*') {
        Some(suffix) => !suffix.contains('*') && rel.ends_with(suffix),
        None => rel == p || rel.starts_with(&format!("{p}/")) || rel.split('/').any(|c| c == p),
    })
}

/// The repository's source files, relative and sorted. Dot-files and
/// dot-directories (`.env*`, `.git`) and anything the root `.gitignore`
/// names are never listed, so they are never read or sent.
pub fn source_files(repo: &Path) -> Result<Vec<String>, CodeAuthorError> {
    let patterns = ignore_patterns(repo);
    let mut found = Vec::new();
    let mut dirs = vec![repo.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| io_err(&dir, e))? {
            let path = entry.map_err(|e| io_err(&dir, e))?.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let rel = path
                .strip_prefix(repo)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if name.starts_with('.') || ignored(&rel, &patterns) {
                continue;
            }
            if path.is_dir() {
                if !SKIPPED_DIRS.split(' ').any(|d| d == name) {
                    dirs.push(path);
                }
            } else if path
                .extension()
                .is_some_and(|e| SOURCE_EXTENSIONS.split_whitespace().any(|x| x == e))
            {
                found.push(rel);
            }
        }
    }
    found.sort();
    Ok(found)
}

fn pick_prompt() -> &'static str {
    "You choose which source files of an app to read in order to write a UI \
     test that reaches a stated outcome. Pick the files that show how a user \
     gets there: routes and navigation, the pages and components on the way, \
     the forms they fill in, and where the text the outcome mentions is \
     rendered (including message bundles). Respond with ONLY lines of the form \
     `FILE: <path>` using paths exactly as listed, most useful first, at most 12."
}

/// Asks the model which listed files to read; anything not in the list is dropped.
pub fn pick_files(
    goal: &str,
    url: Option<&str>,
    files: &[String],
    client: &mut dyn ModelClient,
) -> Result<Vec<String>, CodeAuthorError> {
    let listed = &files[..files.len().min(MAX_LISTED)];
    let start = url.map(|u| format!("The flow starts at {u}.\n"));
    let user = format!(
        "Outcome: {goal}\n{}Files:\n{}",
        start.unwrap_or_default(),
        listed.join("\n")
    );
    let reply = client.complete(pick_prompt(), &user)?;
    let mut picked: Vec<String> = Vec::new();
    for path in reply.lines().filter_map(|l| l.trim().strip_prefix("FILE:")) {
        let path = path.trim().to_string();
        if listed.contains(&path) && !picked.contains(&path) && picked.len() < MAX_PICKED {
            picked.push(path);
        }
    }
    Ok(picked)
}

const WEB_STEP_RULES: &str = "\
Write steps using ONLY these forms. A quoted label is the element's VISIBLE \
text (its link text, button text, <label>, placeholder or aria-label) exactly \
as the code renders it; values and options are plain text, never quoted:
- Go to /path
- Click \"<visible text>\" (a link, tab, menu item or row)
- Type <value> into the \"<label>\" field
- Select <option> from the \"<label>\" field
- Press the \"<label>\" button
- Press Enter
Asserts use: page shows <text>, or: the \"<target>\" shows <text>.";

/// The instructions for the drafting call, given the picked files.
pub fn draft_prompt() -> String {
    format!(
        "You write a UI test from an app's source code. The user states the \
outcome the test must prove; you are given the source files that matter, with \
line numbers. Work out how a user reaches that outcome from the starting page \
and write each step on its own line prefixed `STEP: `. {WEB_STEP_RULES}

End with one or more lines prefixed `ASSERT: ` that prove the outcome.

Use only labels and paths the code actually shows. Where reaching the outcome \
needs something the code cannot give (sign-in credentials, a record that must \
already exist, a choice between several plausible paths), do NOT guess: write \
one line prefixed `UNEXPLAINED: ` saying plainly what is needed. Never write a \
password, token or key, even if one appears in the code.

End EVERY line with ` @ <path>:<line>`: the file and line number where that \
label, route or behaviour is defined. Respond with ONLY the \
STEP:/ASSERT:/UNEXPLAINED: lines - no prose, no numbering, no code fences."
    )
}

/// Splits a trailing ` @ path:line` citation off a reply line. The citation
/// is always removed from the step text, and kept only if it names a file
/// that was actually read.
fn split_source(text: &str, read: &[String]) -> (String, Option<SourceRef>) {
    if let Some((body, cite)) = text.rsplit_once(" @ ") {
        if let Some((file, line)) = cite.trim().rsplit_once(':') {
            if let Ok(line) = line.parse::<u32>() {
                let source = read.iter().any(|r| r == file).then(|| SourceRef {
                    file: file.to_string(),
                    line,
                });
                return (body.trim().to_string(), source);
            }
        }
    }
    (text.trim().to_string(), None)
}

/// Turns the drafting reply into lines, keeping each line's citation.
pub fn parse_draft(reply: &str, read: &[String]) -> Vec<CodeDraftLine> {
    let mut lines = Vec::new();
    for raw in reply.lines().map(str::trim) {
        let (make, rest): (fn(String) -> DraftLine, &str) =
            if let Some(rest) = raw.strip_prefix("STEP:") {
                (DraftLine::Action, rest)
            } else if let Some(rest) = raw.strip_prefix("ASSERT:") {
                (DraftLine::Assert, rest)
            } else if let Some(rest) = raw.strip_prefix("UNEXPLAINED:") {
                (DraftLine::Flagged, rest)
            } else {
                continue; // a stray line is ignored, as in doc_author
            };
        let (text, source) = split_source(rest.trim(), read);
        if !text.is_empty() {
            lines.push(CodeDraftLine {
                line: make(text),
                source,
            });
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScriptedModel(Vec<&'static str>);

    impl ModelClient for ScriptedModel {
        fn complete(&mut self, _system: &str, _user: &str) -> Result<String, AgentError> {
            Ok(self.0.remove(0).to_string())
        }
        fn identity(&self) -> (String, String) {
            ("test".into(), "test".into())
        }
    }

    #[test]
    fn source_files_skip_secrets_ignored_paths_and_non_source() {
        let dir = std::env::temp_dir().join(format!("fp-code-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for f in "src/App.tsx src/i18n/en.properties .env.local node_modules/x/i.js generated/api.ts logo.png README.md"
            .split(' ')
        {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().expect("has a parent")).expect("creates dir");
            std::fs::write(p, "x").expect("writes file");
        }
        std::fs::write(dir.join(".gitignore"), "/generated/\n*.log\n").expect("writes .gitignore");
        let files = source_files(&dir).expect("lists");
        assert_eq!(files, ["src/App.tsx", "src/i18n/en.properties"]);
        std::fs::remove_dir_all(&dir).expect("cleans up");
    }

    #[test]
    fn pick_files_keeps_only_listed_paths() {
        let files = vec!["src/App.tsx".to_string(), "src/Nav.tsx".to_string()];
        let mut model = ScriptedModel(vec![
            "FILE: src/Nav.tsx\nFILE: src/Made/Up.tsx\nFILE: src/Nav.tsx",
        ]);
        let picked = pick_files("goal", None, &files, &mut model).expect("picks");
        assert_eq!(picked, ["src/Nav.tsx"]);
    }

    /// Each step keeps the line it was drafted from, so the reviewer can
    /// check it; a citation of a file that was never read is dropped, not trusted.
    #[test]
    fn parse_draft_keeps_citations_and_flags_what_the_code_cannot_settle() {
        let read = vec!["src/Nav.tsx".to_string()];
        let lines = parse_draft(
            "STEP: Click \"Suppliers\" @ src/Nav.tsx:21\n\
             UNEXPLAINED: sign in first; the page requires an account @ src/Auth.tsx:14\n\
             ASSERT: page shows Acme Metals\n\
             a stray line",
            &read,
        );
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0].line,
            DraftLine::Action("Click \"Suppliers\"".into())
        );
        assert_eq!(
            lines[0].source,
            Some(SourceRef {
                file: "src/Nav.tsx".into(),
                line: 21
            })
        );
        assert_eq!(
            lines[1].line,
            DraftLine::Flagged("sign in first; the page requires an account".into())
        );
        assert_eq!(lines[1].source, None);
        assert_eq!(
            lines[2].line,
            DraftLine::Assert("page shows Acme Metals".into())
        );
    }
}
