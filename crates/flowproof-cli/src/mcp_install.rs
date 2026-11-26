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
//!
//! The Claude desktop app has no project files: it reads one per-user
//! `claude_desktop_config.json` (`mcpServers`, no `type`) from its folder
//! under the platform config directory, and starts every local server in
//! `/` with the user's login-shell PATH (seen in its MCP log on macOS,
//! 2026-11-25). `--claude-desktop` writes that file instead.
//!
//! `--command` writes a given engine path in place of `flowproof`, for an
//! engine that is not on PATH: Flowproof Desktop passes the one it bundles
//! (its plan 007). `--remove` takes the entry out again.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::EXIT_PASS;

/// The `flowproof-flows` skill: when to call which tool, and where the
/// person decides (plan 17, step 10). The server and its guide go in
/// together, into the same two conventions `flowproof config skill` uses.
const FLOWS_SKILL: &str = include_str!("../skills/flowproof-flows/SKILL.md");
const SKILL_PATHS: [&str; 2] = [
    ".claude/skills/flowproof-flows/SKILL.md",
    ".agents/skills/flowproof-flows/SKILL.md",
];

/// Which clients `--claude`, `--cursor`, `--gemini` and `--claude-desktop`
/// asked for. None of the project ones asked means `.mcp.json` plus each
/// client whose folder exists, unless only `--claude-desktop` was given.
pub(crate) struct Clients {
    pub claude: bool,
    pub cursor: bool,
    pub gemini: bool,
    pub claude_desktop: bool,
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

/// What `mcp install` was asked to do, beyond which clients.
pub(crate) struct Options<'a> {
    pub print: bool,
    pub force: bool,
    pub remove: bool,
    /// `--command`: the engine to start instead of `flowproof`.
    pub command: Option<&'a str>,
}

/// How the entry starts the server.
#[derive(Clone, Copy)]
enum Launch<'a> {
    /// `flowproof`, found on the client's PATH.
    Bare,
    /// The project's own install, through `npx --no`.
    Npx,
    /// The engine at this path (`--command`).
    Path(&'a str),
}

pub(crate) fn cmd_install(clients: &Clients, options: &Options) -> Result<u8, String> {
    if let Some(command) = options.command {
        check_command(command)?;
    }
    let root = Path::new(".");
    if options.print {
        let launch = launch(root, options.command);
        println!(
            "{}",
            render(&json!({ "mcpServers": { "flowproof": entry(true, launch) } }))
        );
        return Ok(EXIT_PASS);
    }
    let named = clients.claude || clients.cursor || clients.gemini;
    if named || !clients.claude_desktop {
        if options.remove {
            remove_in(root, clients, options.command, options.force)?;
        } else {
            install_in(root, clients, options.command, options.force)?;
        }
    }
    if clients.claude_desktop {
        let dir = dirs::config_dir().ok_or("could not find this machine's config folder")?;
        let path = desktop_config(&dir);
        let launch = options.command.map_or(Launch::Bare, Launch::Path);
        if options.remove {
            let verb = if uninstall(&path, &entry(false, launch), options.force)? {
                "removed flowproof from"
            } else {
                "no flowproof entry in"
            };
            println!("{verb} {}", path.display());
        } else {
            let verb = if install(&path, &entry(false, launch), options.force)? {
                "wrote"
            } else {
                "up to date:"
            };
            println!("{verb} {}", path.display());
            warn_if_missing(launch);
        }
        println!("Quit and reopen the Claude desktop app to load the change.");
    }
    Ok(EXIT_PASS)
}

/// A client starts the server in a folder of its own (the Claude desktop
/// app in `/`), so a relative path would point at nothing. A bare name is
/// looked up on PATH and is fine.
fn check_command(command: &str) -> Result<(), String> {
    let path = Path::new(command);
    if command.trim().is_empty() {
        return Err("--command is empty".into());
    }
    if path.components().count() > 1 && !path.is_absolute() {
        return Err(format!(
            "--command `{command}` is a relative path; give an absolute one, since the client \
             starts the server in a folder of its own"
        ));
    }
    Ok(())
}

fn warn_if_missing(launch: Launch) {
    match launch {
        Launch::Bare if !on_path("flowproof") => eprintln!(
            "note: `flowproof` is not on your PATH, so the Claude desktop app cannot start \
             it. Install it with `npm install -g flowproof`."
        ),
        Launch::Path(command)
            if Path::new(command).is_absolute() && !Path::new(command).is_file() =>
        {
            eprintln!(
                "note: `{command}` does not exist, so the Claude desktop app cannot start it."
            )
        }
        _ => {}
    }
}

fn launch<'a>(root: &Path, command: Option<&'a str>) -> Launch<'a> {
    match command {
        Some(command) => Launch::Path(command),
        None if npx(root) => Launch::Npx,
        None => Launch::Bare,
    }
}

fn desktop_config(config_dir: &Path) -> std::path::PathBuf {
    config_dir.join("Claude").join("claude_desktop_config.json")
}

fn on_path(name: &str) -> bool {
    let names = [name.to_string(), format!("{name}.exe")];
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| names.iter().any(|n| dir.join(n).is_file()))
    })
}

fn install_in(
    root: &Path,
    clients: &Clients,
    command: Option<&str>,
    force: bool,
) -> Result<(), String> {
    for target in targets(root, clients) {
        let entry = entry(target.typed, launch(root, command));
        if install(&root.join(target.path), &entry, force)? {
            println!("wrote {}", target.path);
        } else {
            println!("up to date: {}", target.path);
        }
    }
    for path in SKILL_PATHS {
        if crate::config::write_skill_file(&root.join(path), FLOWS_SKILL, force)? {
            println!("wrote {path}");
        } else {
            println!("up to date: {path}");
        }
    }
    Ok(())
}

/// Take the `flowproof` entry out of each client's file. The skill files
/// stay: they are plain instructions, and an edited copy is the user's.
fn remove_in(
    root: &Path,
    clients: &Clients,
    command: Option<&str>,
    force: bool,
) -> Result<(), String> {
    for target in targets(root, clients) {
        let entry = entry(target.typed, launch(root, command));
        if uninstall(&root.join(target.path), &entry, force)? {
            println!("removed flowproof from {}", target.path);
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

/// Whether the project installs flowproof itself (`npm install --save-dev`,
/// as the README's quick start does): its `node_modules/.bin` is on no
/// client's PATH, so the entry goes through `npx`.
fn npx(root: &Path) -> bool {
    root.join("node_modules/flowproof/package.json").is_file()
}

/// A command, not this binary's path: these files are meant to be
/// committed, and a teammate's `flowproof` is somewhere else. The bare
/// `flowproof` for a global install; for a project's own, `npx --no`, which
/// runs the project's copy and fails rather than download one when
/// `npm install` has not run yet. A path only when `--command` gives one.
/// No `type` for the Claude desktop app, whose docs never use it.
fn entry(typed: bool, launch: Launch) -> Value {
    let mut entry = match launch {
        Launch::Npx => json!({ "command": "npx", "args": ["--no", "flowproof", "mcp"] }),
        Launch::Bare => json!({ "command": "flowproof", "args": ["mcp"] }),
        Launch::Path(command) => json!({ "command": command, "args": ["mcp"] }),
    };
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
    let mut config = read_config(path)?.unwrap_or_default();
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
    write_config(path, config)?;
    Ok(true)
}

/// Take the `flowproof` entry out, keeping everything else. Only an entry
/// that matches what install would write goes without `--force`: another
/// one was put there by someone else. Returns false when there is none.
fn uninstall(path: &Path, entry: &Value, force: bool) -> Result<bool, String> {
    let shown = path.display();
    let Some(mut config) = read_config(path)? else {
        return Ok(false);
    };
    let Some(Value::Object(servers)) = config.get_mut("mcpServers") else {
        return Ok(false);
    };
    match servers.get("flowproof") {
        None => return Ok(false),
        Some(existing) if existing != entry && !force => {
            return Err(format!(
                "{shown} has a different `flowproof` entry, so it is left alone; pass --force \
                 to remove it anyway"
            ))
        }
        _ => {}
    }
    servers.remove("flowproof");
    write_config(path, config)?;
    Ok(true)
}

/// The file's settings, or `None` when there is no file.
fn read_config(path: &Path) -> Result<Option<Map<String, Value>>, String> {
    let shown = path.display();
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Some(Map::new())),
        Ok(text) => match serde_json::from_str(&text) {
            Ok(Value::Object(config)) => Ok(Some(config)),
            // Comments or trailing commas: rewriting would drop them.
            _ => Err(format!(
                "{shown} is not a plain JSON object, so it is left alone; edit the \
                 `flowproof` entry by hand (`flowproof mcp install --print` shows it)"
            )),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("could not read {shown}: {e}")),
    }
}

fn write_config(path: &Path, config: Map<String, Value>) -> Result<(), String> {
    let text = render(&Value::Object(config)) + "\n";
    std::fs::write(path, text).map_err(|e| format!("could not write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const NONE: Clients = Clients {
        claude: false,
        cursor: false,
        gemini: false,
        claude_desktop: false,
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
    fn writes_the_flows_skill_for_both_conventions() {
        let dir = temp_dir("skill");
        install_in(&dir, &NONE, None, false).expect("install");
        for path in SKILL_PATHS {
            let written = std::fs::read_to_string(dir.join(path)).expect("skill");
            assert_eq!(written, FLOWS_SKILL, "{path}");
        }
        // The open spec: `name` matches the skill's folder.
        assert!(FLOWS_SKILL.starts_with("---\nname: flowproof-flows\n"));
        std::fs::write(dir.join(SKILL_PATHS[0]), "edited").expect("edit");
        let refused = install_in(&dir, &NONE, None, false).expect_err("refused");
        assert!(refused.contains("--force"), "{refused}");
        install_in(&dir, &NONE, None, true).expect("forced");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_second_install_is_a_no_op() {
        let dir = temp_dir("again");
        install_in(&dir, &NONE, None, false).expect("first");
        let written = std::fs::read_to_string(dir.join(".mcp.json")).expect("read");
        let expected = json!({ "mcpServers": { "flowproof": {
            "type": "stdio", "command": "flowproof", "args": ["mcp"],
        } } });
        assert_eq!(
            serde_json::from_str::<Value>(&written).expect("json"),
            expected
        );
        assert!(
            !install(&dir.join(".mcp.json"), &entry(true, Launch::Bare), false).expect("again")
        );
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
        assert!(install(&path, &entry(false, Launch::Bare), false).expect("install"));
        let after = read(&path);
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["mcpServers"]["other"]["command"], "x");
        assert_eq!(after["mcpServers"]["flowproof"], entry(false, Launch::Bare));
        assert!(after["mcpServers"]["flowproof"].get("type").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_different_entry_needs_force() {
        let dir = temp_dir("force");
        let path = dir.join(".mcp.json");
        let custom = json!({ "mcpServers": { "flowproof": { "command": "/opt/flowproof" } } });
        std::fs::write(&path, custom.to_string()).expect("write");
        let refused = install(&path, &entry(true, Launch::Bare), false).expect_err("refused");
        assert!(refused.contains("--force"), "{refused}");
        assert_eq!(read(&path), custom);
        assert!(install(&path, &entry(true, Launch::Bare), true).expect("forced"));
        assert_eq!(
            read(&path)["mcpServers"]["flowproof"],
            entry(true, Launch::Bare)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_project_with_its_own_flowproof_runs_it_through_npx() {
        let dir = temp_dir("npx");
        std::fs::create_dir_all(dir.join("node_modules/flowproof")).expect("package");
        std::fs::write(dir.join("node_modules/flowproof/package.json"), "{}").expect("write");
        install_in(&dir, &NONE, None, false).expect("install");
        let written = read(&dir.join(".mcp.json"))["mcpServers"]["flowproof"].clone();
        assert_eq!(written["command"], "npx");
        assert_eq!(written["args"], json!(["--no", "flowproof", "mcp"]));
        // A bare entry from before is a different entry: replaced only with --force.
        let path = dir.join(".mcp.json");
        install(&path, &entry(true, Launch::Bare), true).expect("bare");
        assert!(install_in(&dir, &NONE, None, false).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_claude_desktop_entry_joins_the_apps_own_settings() {
        let dir = temp_dir("desktop");
        let path = desktop_config(&dir);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        std::fs::write(&path, r#"{"preferences":{"sidebarMode":"chat"}}"#).expect("write");
        let bare = entry(false, Launch::Bare);
        assert!(install(&path, &bare, false).expect("install"));
        let config = read(&path);
        assert_eq!(config["preferences"]["sidebarMode"], "chat");
        assert_eq!(
            config["mcpServers"]["flowproof"],
            json!({ "command": "flowproof", "args": ["mcp"] })
        );
        assert!(!install(&path, &bare, false).expect("again"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn command_writes_the_engine_path_as_given() {
        let dir = temp_dir("command");
        let path = desktop_config(&dir);
        let windows = r"C:\Program Files\flowproof\flowproof-engine.exe";
        let engine = entry(false, Launch::Path(windows));
        assert!(install(&path, &engine, false).expect("install"));
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(written.contains(r#""C:\\Program Files\\flowproof\\flowproof-engine.exe""#));
        assert_eq!(
            read(&path)["mcpServers"]["flowproof"],
            json!({ "command": windows, "args": ["mcp"] })
        );
        // The bare entry is a different one: switching needs --force.
        assert!(install(&path, &entry(false, Launch::Bare), false).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn command_must_not_be_a_relative_path() {
        assert!(check_command("bin/flowproof").is_err());
        assert!(check_command("  ").is_err());
        assert!(check_command("flowproof-dev").is_ok());
        let absolute = std::env::temp_dir().join("flowproof-engine");
        assert!(check_command(&absolute.to_string_lossy()).is_ok());
    }

    #[test]
    fn remove_takes_out_only_a_matching_entry() {
        let dir = temp_dir("remove");
        let path = desktop_config(&dir);
        let ours = entry(
            false,
            Launch::Path("/Applications/flowproof.app/flowproof-engine"),
        );
        assert!(!uninstall(&path, &ours, false).expect("no file"));
        assert!(!path.exists(), "remove creates nothing");
        let before = json!({
            "preferences": { "sidebarMode": "chat" },
            "mcpServers": { "other": { "command": "x" }, "flowproof": ours },
        });
        std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        std::fs::write(&path, before.to_string()).expect("write");
        // Someone else's flowproof stays unless forced.
        let bare = entry(false, Launch::Bare);
        let refused = uninstall(&path, &bare, false).expect_err("refused");
        assert!(refused.contains("--force"), "{refused}");
        assert_eq!(read(&path), before);
        assert!(uninstall(&path, &ours, false).expect("remove"));
        let after = read(&path);
        assert_eq!(after["preferences"]["sidebarMode"], "chat");
        assert_eq!(after["mcpServers"]["other"]["command"], "x");
        assert!(after["mcpServers"].get("flowproof").is_none());
        assert!(!uninstall(&path, &ours, false).expect("again"));
        std::fs::write(&path, before.to_string()).expect("write");
        assert!(uninstall(&path, &bare, true).expect("forced"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_leaves_the_project_skills() {
        let dir = temp_dir("remove-project");
        install_in(&dir, &NONE, None, false).expect("install");
        remove_in(&dir, &NONE, None, false).expect("remove");
        assert!(read(&dir.join(".mcp.json"))["mcpServers"]
            .get("flowproof")
            .is_none());
        assert!(dir.join(SKILL_PATHS[0]).is_file());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_with_comments_is_left_alone() {
        let dir = temp_dir("comments");
        let path = dir.join(".mcp.json");
        let text = "{\n  // ours\n  \"mcpServers\": {}\n}\n";
        std::fs::write(&path, text).expect("write");
        let refused = install(&path, &entry(true, Launch::Bare), true).expect_err("refused");
        assert!(refused.contains("--print"), "{refused}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), text);
        std::fs::remove_dir_all(&dir).ok();
    }
}
