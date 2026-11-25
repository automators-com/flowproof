//! `flowproof mcp install`: register `flowproof mcp` in the project's own
//! client config files (plans/017-flowproof-mcp-server.md, decision 7). The
//! same shape as `flowproof config skill`: write unless already current,
//! refuse to replace a different entry without `--force`, and `--print` for
//! a client this does not know.
//!
//! Paths and keys as each client's docs gave them on 2026-11-25:
//! - Claude Code: `.mcp.json`, `mcpServers` (code.claude.com/docs/en/mcp).
//!   GitHub Copilot in VS Code reads the same file; the `.vscode/mcp.json`
//!   it used before (key `servers`) is deprecated for new configs.
//! - Cursor: `.cursor/mcp.json`, `mcpServers` (cursor.com/docs/context/mcp).
//! - Gemini CLI: `.gemini/settings.json`, `mcpServers`, beside its other
//!   settings (gemini-cli docs/tools/mcp-server.md). Its entries carry no
//!   `type`.
//!
//! Codex reads TOML, from `.codex/config.toml` in trusted projects only, so
//! it is left to `--print` and the docs rather than edited here.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::EXIT_PASS;

/// Which clients `--claude`, `--cursor` and `--gemini` asked for. None
/// asked means `.mcp.json` plus each client whose folder exists.
pub(crate) struct Clients {
    pub claude: bool,
    pub cursor: bool,
    pub gemini: bool,
}

struct Target {
    path: &'static str,
    /// The folder that shows the project uses this client; `None` is
    /// written by default.
    folder: Option<&'static str>,
    /// Whether the entry says `"type": "stdio"`.
    typed: bool,
}

const CLAUDE: Target = Target {
    path: ".mcp.json",
    folder: None,
    typed: true,
};
const CURSOR: Target = Target {
    path: ".cursor/mcp.json",
    folder: Some(".cursor"),
    typed: true,
};
const GEMINI: Target = Target {
    path: ".gemini/settings.json",
    folder: Some(".gemini"),
    typed: false,
};

pub(crate) fn cmd_install(clients: &Clients, print: bool, force: bool) -> Result<u8, String> {
    if print {
        println!(
            "{}",
            render(&json!({ "mcpServers": { "flowproof": entry(true) } }))
        );
        return Ok(EXIT_PASS);
    }
    install_in(Path::new("."), clients, force)?;
    Ok(EXIT_PASS)
}

fn install_in(root: &Path, clients: &Clients, force: bool) -> Result<(), String> {
    for target in targets(root, clients) {
        if install(&root.join(target.path), &entry(target.typed), force)? {
            println!("wrote {}", target.path);
        } else {
            println!("up to date: {}", target.path);
        }
    }
    Ok(())
}

fn targets(root: &Path, clients: &Clients) -> Vec<&'static Target> {
    let named = clients.claude || clients.cursor || clients.gemini;
    [
        (&CLAUDE, clients.claude),
        (&CURSOR, clients.cursor),
        (&GEMINI, clients.gemini),
    ]
    .into_iter()
    .filter(|(target, asked)| match (named, target.folder) {
        (true, _) => *asked,
        (false, None) => true,
        (false, Some(folder)) => root.join(folder).is_dir(),
    })
    .map(|(target, _)| target)
    .collect()
}

/// The bare `flowproof` command, not this binary's path: these files are
/// meant to be committed, and a teammate's `flowproof` is somewhere else.
fn entry(typed: bool) -> Value {
    let mut entry = json!({ "command": "flowproof", "args": ["mcp"] });
    if typed {
        entry["type"] = json!("stdio");
    }
    entry
}

fn render(config: &Value) -> String {
    serde_json::to_string_pretty(config).unwrap_or_default()
}

/// Add the `flowproof` entry under `mcpServers`, keeping every other server
/// and setting (their keys come back in alphabetical order). Returns false
/// when the entry is already there as written.
fn install(path: &Path, entry: &Value, force: bool) -> Result<bool, String> {
    let shown = path.display();
    let mut config = match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Map::new(),
        Ok(text) => match serde_json::from_str(&text) {
            Ok(Value::Object(config)) => config,
            // Comments or trailing commas: rewriting would drop them.
            _ => {
                return Err(format!(
                    "{shown} is not a plain JSON object, so it is left alone; add the \
                     entry from `flowproof mcp install --print` by hand"
                ))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(e) => return Err(format!("could not read {shown}: {e}")),
    };
    let Value::Object(servers) = config.entry("mcpServers").or_insert_with(|| json!({})) else {
        return Err(format!("{shown}: `mcpServers` is not an object"));
    };
    match servers.get("flowproof") {
        Some(existing) if existing == entry => return Ok(false),
        Some(_) if !force => {
            return Err(format!(
                "{shown} already has a different `flowproof` entry; pass --force to replace it"
            ))
        }
        _ => {}
    }
    servers.insert("flowproof".into(), entry.clone());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let text = render(&Value::Object(config)) + "\n";
    std::fs::write(path, text).map_err(|e| format!("could not write {shown}: {e}"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const NONE: Clients = Clients {
        claude: false,
        cursor: false,
        gemini: false,
    };

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("flowproof-mcp-install-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn read(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
    }

    fn paths(root: &Path, clients: &Clients) -> Vec<&'static str> {
        targets(root, clients).iter().map(|t| t.path).collect()
    }

    #[test]
    fn writes_mcp_json_and_the_clients_whose_folders_exist() {
        let dir = temp_dir("targets");
        assert_eq!(paths(&dir, &NONE), [".mcp.json"]);
        std::fs::create_dir_all(dir.join(".cursor")).expect("cursor");
        assert_eq!(paths(&dir, &NONE), [".mcp.json", ".cursor/mcp.json"]);
        let gemini = Clients {
            gemini: true,
            ..NONE
        };
        assert_eq!(paths(&dir, &gemini), [".gemini/settings.json"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_second_install_is_a_no_op() {
        let dir = temp_dir("again");
        install_in(&dir, &NONE, false).expect("first");
        let written = std::fs::read_to_string(dir.join(".mcp.json")).expect("read");
        let expected = json!({ "mcpServers": { "flowproof": {
            "type": "stdio", "command": "flowproof", "args": ["mcp"],
        } } });
        assert_eq!(
            serde_json::from_str::<Value>(&written).expect("json"),
            expected
        );
        assert!(!install(&dir.join(".mcp.json"), &entry(true), false).expect("again"));
        let again = std::fs::read_to_string(dir.join(".mcp.json")).expect("read");
        assert_eq!(again, written);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn keeps_other_servers_and_settings() {
        let dir = temp_dir("keeps");
        let path = dir.join(".gemini/settings.json");
        std::fs::create_dir_all(dir.join(".gemini")).expect("gemini");
        let before = json!({ "theme": "dark", "mcpServers": { "other": { "command": "x" } } });
        std::fs::write(&path, before.to_string()).expect("write");
        assert!(install(&path, &entry(false), false).expect("install"));
        let after = read(&path);
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["mcpServers"]["other"]["command"], "x");
        assert_eq!(after["mcpServers"]["flowproof"], entry(false));
        assert!(after["mcpServers"]["flowproof"].get("type").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_different_entry_needs_force() {
        let dir = temp_dir("force");
        let path = dir.join(".mcp.json");
        let custom = json!({ "mcpServers": { "flowproof": { "command": "/opt/flowproof" } } });
        std::fs::write(&path, custom.to_string()).expect("write");
        let refused = install(&path, &entry(true), false).expect_err("refused");
        assert!(refused.contains("--force"), "{refused}");
        assert_eq!(read(&path), custom);
        assert!(install(&path, &entry(true), true).expect("forced"));
        assert_eq!(read(&path)["mcpServers"]["flowproof"], entry(true));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_with_comments_is_left_alone() {
        let dir = temp_dir("comments");
        let path = dir.join(".mcp.json");
        let text = "{\n  // ours\n  \"mcpServers\": {}\n}\n";
        std::fs::write(&path, text).expect("write");
        let refused = install(&path, &entry(true), true).expect_err("refused");
        assert!(refused.contains("--print"), "{refused}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), text);
        std::fs::remove_dir_all(&dir).ok();
    }
}
