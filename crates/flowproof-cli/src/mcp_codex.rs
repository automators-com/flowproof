//! `flowproof mcp install --codex`: the `[mcp_servers.flowproof]` table in
//! Codex's per-user `config.toml` (Flowproof Desktop's plan 008). Codex's
//! CLI, IDE extension and desktop app all read that file.
//!
//! Keys as OpenAI's Codex MCP docs gave them on 2026-11-26
//! (developers.openai.com/codex/mcp): one `[mcp_servers.<name>]` table per
//! server, `command` plus optional `args` for stdio. The file lives in
//! `$CODEX_HOME`, which defaults to `~/.codex`.
//!
//! People edit this file by hand, so the edit goes through `toml_edit`:
//! comments, key order and every other table survive. Only the `flowproof`
//! table is written or removed. Keys the user added to it (`env`, timeouts)
//! don't stop it counting as ours, and stay when it is already current.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use toml_edit::{value, Array, DocumentMut, Item, Table};

/// `$CODEX_HOME/config.toml`, or `~/.codex/config.toml`.
pub(crate) fn config_path() -> Result<PathBuf, String> {
    path_from(std::env::var_os("CODEX_HOME"), dirs::home_dir())
}

fn path_from(codex_home: Option<OsString>, home: Option<PathBuf>) -> Result<PathBuf, String> {
    let dir = match codex_home.filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home
            .ok_or("could not find your home folder")?
            .join(".codex"),
    };
    Ok(dir.join("config.toml"))
}

/// The table `--print --codex` shows.
pub(crate) fn render_entry(command: &str) -> String {
    let mut doc = DocumentMut::new();
    let mut servers = Table::new();
    servers.set_implicit(true);
    servers.insert("flowproof", Item::Table(table(command)));
    doc.insert("mcp_servers", Item::Table(servers));
    doc.to_string()
}

fn table(command: &str) -> Table {
    let mut table = Table::new();
    table.insert("command", value(command));
    table.insert("args", value(Array::from_iter(["mcp"])));
    table
}

/// Whether an existing `flowproof` table starts `command mcp`.
fn matches(existing: &Item, command: &str) -> bool {
    let args = existing.get("args").and_then(Item::as_array).map(|args| {
        args.iter()
            .map(|arg| arg.as_str().unwrap_or_default())
            .collect::<Vec<_>>()
    });
    existing.get("command").and_then(Item::as_str) == Some(command)
        && args.as_deref() == Some(&["mcp"][..])
}

/// Add the `flowproof` table. Returns false when one starting the same
/// engine is already there.
pub(crate) fn install(path: &Path, command: &str, force: bool) -> Result<bool, String> {
    let shown = path.display();
    let mut doc = read(path)?.unwrap_or_default();
    let servers = doc.entry("mcp_servers").or_insert_with(|| {
        let mut servers = Table::new();
        servers.set_implicit(true);
        Item::Table(servers)
    });
    let Some(servers) = servers.as_table_mut() else {
        return Err(format!(
            "{shown}: `mcp_servers` is not a table, so the file is left alone; add the \
             `flowproof` entry by hand (`flowproof mcp install --codex --print` shows it)"
        ));
    };
    match servers.get("flowproof") {
        Some(existing) if matches(existing, command) => return Ok(false),
        Some(_) if !force => {
            return Err(format!(
                "{shown} already has a different `flowproof` entry; pass --force to replace it"
            ))
        }
        _ => {}
    }
    servers.insert("flowproof", Item::Table(table(command)));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    write(path, &doc)?;
    Ok(true)
}

/// Take the `flowproof` table out. One that starts another engine stays
/// unless `force`. Returns false when there is none.
pub(crate) fn uninstall(path: &Path, command: &str, force: bool) -> Result<bool, String> {
    let shown = path.display();
    let Some(mut doc) = read(path)? else {
        return Ok(false);
    };
    let Some(servers) = doc.get_mut("mcp_servers").and_then(Item::as_table_mut) else {
        return Ok(false);
    };
    match servers.get("flowproof") {
        None => return Ok(false),
        Some(existing) if !matches(existing, command) && !force => {
            return Err(format!(
                "{shown} has a different `flowproof` entry, so it is left alone; pass --force \
                 to remove it anyway"
            ))
        }
        _ => {}
    }
    servers.remove("flowproof");
    write(path, &doc)?;
    Ok(true)
}

/// The parsed file, or `None` when there is none.
fn read(path: &Path) -> Result<Option<DocumentMut>, String> {
    let shown = path.display();
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<DocumentMut>()
            .map(Some)
            .map_err(|e| format!("{shown} is not valid TOML, so it is left alone: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("could not read {shown}: {e}")),
    }
}

fn write(path: &Path, doc: &DocumentMut) -> Result<(), String> {
    std::fs::write(path, doc.to_string())
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENGINE: &str = "/Applications/flowproof.app/Contents/MacOS/flowproof-engine";

    fn temp_config(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("flowproof-mcp-codex-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        dir.join(".codex").join("config.toml")
    }

    fn parsed(path: &Path) -> DocumentMut {
        std::fs::read_to_string(path)
            .expect("read")
            .parse()
            .expect("toml")
    }

    fn command_in(path: &Path) -> Option<String> {
        parsed(path)
            .get("mcp_servers")
            .and_then(|servers| servers.get("flowproof"))
            .and_then(|entry| entry.get("command"))
            .and_then(Item::as_str)
            .map(str::to_owned)
    }

    fn cleanup(path: &Path) {
        if let Some(dir) = path.parent().and_then(Path::parent) {
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn codex_home_wins_over_the_home_folder() {
        let home = Some(PathBuf::from("/home/me"));
        assert_eq!(
            path_from(None, home.clone()).expect("default"),
            Path::new("/home/me/.codex/config.toml")
        );
        assert_eq!(
            path_from(Some("/srv/codex".into()), home.clone()).expect("set"),
            Path::new("/srv/codex/config.toml")
        );
        assert_eq!(
            path_from(Some("".into()), home).expect("empty"),
            Path::new("/home/me/.codex/config.toml")
        );
        assert!(path_from(None, None).is_err());
    }

    #[test]
    fn a_new_file_gets_just_the_flowproof_table() {
        let path = temp_config("new");
        assert!(install(&path, ENGINE, false).expect("install"));
        let text = std::fs::read_to_string(&path).expect("read");
        assert_eq!(text, render_entry(ENGINE));
        assert!(text.starts_with("[mcp_servers.flowproof]\n"), "{text}");
        assert!(!text.contains("[mcp_servers]\n"), "{text}");
        assert!(!install(&path, ENGINE, false).expect("again"));
        assert_eq!(std::fs::read_to_string(&path).expect("read"), text);
        cleanup(&path);
    }

    #[test]
    fn comments_and_other_settings_survive_install_and_remove() {
        let path = temp_config("keeps");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        let before = "# my settings\nmodel = \"gpt-5\" # pinned\n\n\
                      [mcp_servers.other]\ncommand = \"x\"\nargs = [\"serve\"]\n";
        std::fs::write(&path, before).expect("write");
        assert!(install(&path, ENGINE, false).expect("install"));
        let after = std::fs::read_to_string(&path).expect("read");
        assert!(after.starts_with(before), "{after}");
        assert_eq!(command_in(&path).as_deref(), Some(ENGINE));
        assert!(uninstall(&path, ENGINE, false).expect("remove"));
        assert_eq!(std::fs::read_to_string(&path).expect("read"), before);
        cleanup(&path);
    }

    #[test]
    fn keys_the_user_added_keep_it_current() {
        let path = temp_config("extra");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        let text = format!(
            "[mcp_servers.flowproof]\ncommand = \"{ENGINE}\"\nargs = [\"mcp\"]\n\
             startup_timeout_sec = 30\n"
        );
        std::fs::write(&path, &text).expect("write");
        assert!(!install(&path, ENGINE, false).expect("current"));
        assert_eq!(std::fs::read_to_string(&path).expect("read"), text);
        assert!(uninstall(&path, ENGINE, false).expect("ours"));
        cleanup(&path);
    }

    #[test]
    fn a_windows_path_is_written_verbatim() {
        let path = temp_config("windows");
        let windows = r"C:\Program Files\flowproof\flowproof-engine.exe";
        assert!(install(&path, windows, false).expect("install"));
        assert_eq!(command_in(&path).as_deref(), Some(windows));
        assert!(!install(&path, windows, false).expect("again"));
        cleanup(&path);
    }

    #[test]
    fn a_different_entry_needs_force() {
        let path = temp_config("force");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        let theirs = "[mcp_servers.flowproof]\ncommand = \"flowproof\"\nargs = [\"mcp\"]\n";
        std::fs::write(&path, theirs).expect("write");
        let refused = install(&path, ENGINE, false).expect_err("refused");
        assert!(refused.contains("--force"), "{refused}");
        let refused = uninstall(&path, ENGINE, false).expect_err("refused");
        assert!(refused.contains("--force"), "{refused}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), theirs);
        assert!(install(&path, ENGINE, true).expect("forced"));
        assert_eq!(command_in(&path).as_deref(), Some(ENGINE));
        std::fs::write(&path, theirs).expect("write");
        assert!(uninstall(&path, ENGINE, true).expect("forced"));
        assert_eq!(command_in(&path), None);
        cleanup(&path);
    }

    #[test]
    fn remove_without_a_file_creates_nothing() {
        let path = temp_config("missing");
        assert!(!uninstall(&path, ENGINE, false).expect("no file"));
        assert!(!path.exists());
        cleanup(&path);
    }

    #[test]
    fn invalid_toml_is_left_alone() {
        let path = temp_config("invalid");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        std::fs::write(&path, "model = \n").expect("write");
        let refused = install(&path, ENGINE, false).expect_err("refused");
        assert!(refused.contains("not valid TOML"), "{refused}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "model = \n");
        cleanup(&path);
    }

    #[test]
    fn an_inline_mcp_servers_is_left_alone() {
        let path = temp_config("inline");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        std::fs::write(&path, "mcp_servers = {}\n").expect("write");
        let refused = install(&path, ENGINE, false).expect_err("refused");
        assert!(refused.contains("--print"), "{refused}");
        cleanup(&path);
    }
}
