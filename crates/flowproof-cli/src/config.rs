//! `flowproof config`: a global, per-machine credential file for SAP GUI and
//! Fiori. Design and reasoning live in `plans/001-credential-config.md`; the
//! short version — `sap` and `fiori` are two independent profiles (not one
//! shared identity), each mapping to its own set of env vars, seeded into the
//! process environment as a fallback so `flowproof_trace::secret::resolve_refs`
//! and both adapters need no changes at all. Config-time correctness is
//! deliberately not this module's job: SAP already rejects a bad credential
//! at record/run time with a specific message (`sap_com.rs`), so this module
//! only ever writes what it's given.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::EXIT_PASS;

/// SAP GUI's fields, mapped to the env vars `sap_com.rs`'s `login_for` and a
/// flow's own `connection:`/`login:` already read
/// (plans/001-credential-config.md:167-173). All optional: a fresh profile
/// starts empty, and re-running `flowproof config sap` merges into whatever
/// is already there rather than blanking fields it wasn't asked about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SapProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
}

impl SapProfile {
    fn env_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if let Some(v) = &self.user {
            pairs.push(("SAP_USER", v.clone()));
        }
        if let Some(v) = &self.password {
            pairs.push(("SAP_PASSWORD", v.clone()));
        }
        if let Some(v) = &self.client {
            pairs.push(("SAP_CLIENT", v.clone()));
        }
        if let Some(v) = &self.language {
            pairs.push(("SAP_LANGUAGE", v.clone()));
        }
        if let Some(v) = &self.connection {
            pairs.push(("SAP_CONNECTION", v.clone()));
        }
        pairs
    }
}

/// Fiori's fields — a deliberately separate shape from [`SapProfile`], not a
/// shared identity: same field names, but `FIORI_*` env vars rather than
/// `SAP_*`, because two profiles cannot both feed one process's copy of
/// `SAP_USER` at once (plans/001-credential-config.md, "Two profiles, not
/// one identity"). `base_url` replaces SAP GUI's `connection` as the one
/// surface-specific field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FioriProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl FioriProfile {
    fn env_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if let Some(v) = &self.user {
            pairs.push(("FIORI_USER", v.clone()));
        }
        if let Some(v) = &self.password {
            pairs.push(("FIORI_PASSWORD", v.clone()));
        }
        if let Some(v) = &self.client {
            pairs.push(("FIORI_CLIENT", v.clone()));
        }
        if let Some(v) = &self.language {
            pairs.push(("FIORI_LANGUAGE", v.clone()));
        }
        if let Some(v) = &self.base_url {
            pairs.push(("FIORI_BASE_URL", v.clone()));
        }
        pairs
    }
}

/// The whole file: two independent, optional profiles. Both absent is the
/// state before anyone has run `flowproof config` at all — not an error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sap: Option<SapProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fiori: Option<FioriProfile>,
}

impl Config {
    /// Every `(env var, value)` this config has an answer for, across both
    /// profiles combined.
    fn env_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if let Some(sap) = &self.sap {
            pairs.extend(sap.env_pairs());
        }
        if let Some(fiori) = &self.fiori {
            pairs.extend(fiori.env_pairs());
        }
        pairs
    }

    /// A copy with every password field replaced by a fixed mask — for
    /// `flowproof config show`, which must never echo a real secret back to
    /// a terminal (plans/001-credential-config.md, "The secret sitting on
    /// disk": the same discipline `CHARTER.md` invariant 9 applies to
    /// traces, applied here to a terminal instead).
    pub fn masked(&self) -> Config {
        let mut copy = self.clone();
        if let Some(sap) = &mut copy.sap {
            if sap.password.is_some() {
                sap.password = Some("********".to_string());
            }
        }
        if let Some(fiori) = &mut copy.fiori {
            if fiori.password.is_some() {
                fiori.password = Some("********".to_string());
            }
        }
        copy
    }
}

/// `%APPDATA%\flowproof\config.yaml` (Windows) / `~/Library/Application
/// Support/flowproof/config.yaml` (macOS) / `$XDG_CONFIG_HOME/flowproof/config.yaml`,
/// falling back to `~/.config/flowproof/config.yaml` (Linux) —
/// `dirs::config_dir()` resolves all three per
/// plans/001-credential-config.md:89-103.
pub fn config_path() -> Result<PathBuf, String> {
    let base = dirs::config_dir()
        .ok_or_else(|| "could not resolve a config directory on this platform".to_string())?;
    Ok(base.join("flowproof").join("config.yaml"))
}

/// Load-or-default: a missing file is an empty [`Config`], not an error —
/// the state of every machine before `flowproof config sap`/`fiori` has ever
/// run on it.
pub fn load() -> Result<Config, String> {
    load_from(&config_path()?)
}

fn load_from(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_yaml::from_str(&text)
            .map_err(|e| format!("{} is not valid YAML: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
    }
}

/// Write the config, creating its parent directory if needed. `0600` (owner
/// read/write only) on Unix, applied at write time since the file may hold a
/// real password (plans/001-credential-config.md, "The secret sitting on
/// disk"). No Windows-side equivalent yet — a stated gap, not a silent one;
/// see the plan's "Decisions" section.
pub fn save(config: &Config) -> Result<(), String> {
    save_to(&config_path()?, config)
}

fn save_to(path: &Path, config: &Config) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let yaml =
        serde_yaml::to_string(config).map_err(|e| format!("could not serialize config: {e}"))?;
    std::fs::write(path, yaml).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("could not set permissions on {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Seed every mapped env var this config has a value for, but ONLY into a
/// variable the process doesn't already have — an explicit shell export, CI
/// secret, or a suite's `env:`/`env_from` always wins
/// (plans/001-credential-config.md, "How it reaches the flow", mirroring
/// `apply_suite_env`'s opposite, unconditional precedent at the other end of
/// the stack). A missing file seeds nothing, silently — the common case
/// before anyone has configured anything. An unreadable/malformed file warns
/// on stderr and is treated as empty, the same "warn, don't abort" posture
/// `apply_suite_env` already takes for a single bad key, because this runs
/// on the way into every `record`/`run`, including ones that have nothing to
/// do with SAP or Fiori.
pub fn seed_env() {
    let config = match load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("warning: flowproof config unreadable, ignoring it ({e})");
            return;
        }
    };
    for (name, value) in config.env_pairs() {
        if std::env::var(name).is_err() {
            std::env::set_var(name, value);
        }
    }
}

/// A plain-text prompt: shows the current value (if any) as a bracketed
/// default, Enter alone keeps it, anything else replaces it. Not used for
/// the password field — see [`prompt_password`].
fn prompt_field(label: &str, current: Option<&str>) -> Result<Option<String>, String> {
    match current {
        Some(v) => print!("{label} [{v}]: "),
        None => print!("{label}: "),
    }
    std::io::stdout()
        .flush()
        .map_err(|e| format!("writing prompt: {e}"))?;
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("reading input: {e}"))?;
    let entered = line.trim();
    if entered.is_empty() {
        Ok(current.map(str::to_string))
    } else {
        Ok(Some(entered.to_string()))
    }
}

/// The password prompt: masked input via `rpassword`, never echoes a stored
/// value back. An empty answer keeps whatever is already stored — the only
/// way to leave the password alone without retyping it blind, since it is
/// never shown (plans/001-credential-config.md, "The secret sitting on
/// disk").
fn prompt_password(label: &str, currently_set: bool) -> Result<Option<String>, String> {
    let prompt = if currently_set {
        format!("{label} [leave blank to keep current]: ")
    } else {
        format!("{label}: ")
    };
    let entered =
        rpassword::prompt_password(prompt).map_err(|e| format!("reading password: {e}"))?;
    if entered.is_empty() {
        Ok(None)
    } else {
        Ok(Some(entered))
    }
}

/// Interactive/flag-driven arguments common to both `sap` and `fiori`; the
/// one field that differs (`connection` vs `base_url`) stays outside this
/// struct.
pub struct SharedArgs {
    pub user: Option<String>,
    pub password: Option<String>,
    pub client: Option<String>,
    pub language: Option<String>,
}

impl SharedArgs {
    fn any_set(&self) -> bool {
        self.user.is_some()
            || self.password.is_some()
            || self.client.is_some()
            || self.language.is_some()
    }

    /// Apply whichever fields were actually given, leaving the rest of the
    /// profile untouched — the non-interactive mirror of the prompts'
    /// merge-on-rerun behavior. Takes the four destination fields directly
    /// rather than a whole profile, since `SapProfile` and `FioriProfile`
    /// share this shape but aren't the same type.
    fn apply_to(
        self,
        user: &mut Option<String>,
        password: &mut Option<String>,
        client: &mut Option<String>,
        language: &mut Option<String>,
    ) {
        if let Some(v) = self.user {
            *user = Some(v);
        }
        if let Some(v) = self.password {
            *password = Some(v);
        }
        if let Some(v) = self.client {
            *client = Some(v);
        }
        if let Some(v) = self.language {
            *language = Some(v);
        }
    }
}

/// `flowproof config sap`: prompt for (or take as flags) user, password,
/// client, language, connection, merge into whatever `sap:` block already
/// exists, and write the file. No live check against SAP — see
/// plans/001-credential-config.md, "The shape the team landed on".
pub fn cmd_sap(shared: SharedArgs, connection: Option<String>) -> Result<u8, String> {
    let any_flag = shared.any_set() || connection.is_some();
    let mut config = load()?;
    let mut profile = config.sap.take().unwrap_or_default();

    if any_flag {
        shared.apply_to(
            &mut profile.user,
            &mut profile.password,
            &mut profile.client,
            &mut profile.language,
        );
        if let Some(v) = connection {
            profile.connection = Some(v);
        }
    } else {
        require_tty(
            "flowproof config sap",
            &[
                "--user",
                "--password",
                "--client",
                "--language",
                "--connection",
            ],
        )?;
        profile.user = prompt_field("SAP user", profile.user.as_deref())?;
        profile.password =
            prompt_password("SAP password", profile.password.is_some())?.or(profile.password);
        profile.client = prompt_field("SAP client (optional)", profile.client.as_deref())?;
        profile.language = prompt_field("SAP language (optional)", profile.language.as_deref())?;
        profile.connection = prompt_field(
            "SAP Logon connection name (optional)",
            profile.connection.as_deref(),
        )?;
    }

    config.sap = Some(profile);
    save(&config)?;
    println!("wrote {}", config_path()?.display());
    Ok(EXIT_PASS)
}

/// `flowproof config fiori`: same shape as [`cmd_sap`], writing the
/// independent `fiori:` block with `base_url` in place of `connection`.
pub fn cmd_fiori(shared: SharedArgs, base_url: Option<String>) -> Result<u8, String> {
    let any_flag = shared.any_set() || base_url.is_some();
    let mut config = load()?;
    let mut profile = config.fiori.take().unwrap_or_default();

    if any_flag {
        shared.apply_to(
            &mut profile.user,
            &mut profile.password,
            &mut profile.client,
            &mut profile.language,
        );
        if let Some(v) = base_url {
            profile.base_url = Some(v);
        }
    } else {
        require_tty(
            "flowproof config fiori",
            &[
                "--user",
                "--password",
                "--client",
                "--language",
                "--base-url",
            ],
        )?;
        profile.user = prompt_field("Fiori user", profile.user.as_deref())?;
        profile.password =
            prompt_password("Fiori password", profile.password.is_some())?.or(profile.password);
        profile.client = prompt_field("Fiori client (optional)", profile.client.as_deref())?;
        profile.language = prompt_field("Fiori language (optional)", profile.language.as_deref())?;
        profile.base_url = prompt_field(
            "Fiori launchpad base URL (optional)",
            profile.base_url.as_deref(),
        )?;
    }

    config.fiori = Some(profile);
    save(&config)?;
    println!("wrote {}", config_path()?.display());
    Ok(EXIT_PASS)
}

/// A real TTY is required to prompt; piped/non-interactive stdin (a script,
/// CI) fails fast with the flag alternative named, rather than hanging on a
/// read that will never come — the same "an agent that fails to start says
/// so" posture `CHARTER.md` §4 already asks for on the agent boundary.
fn require_tty(command: &str, flags: &[&str]) -> Result<(), String> {
    if std::io::stdin().is_terminal() {
        Ok(())
    } else {
        Err(format!(
            "{command} needs an interactive terminal to prompt for values; pass {} instead when \
             scripting or running in CI",
            flags.join("/")
        ))
    }
}

/// `flowproof config show`: the file's path and contents, password masked.
pub fn cmd_show() -> Result<u8, String> {
    let config = load()?;
    println!("{}", config_path()?.display());
    let yaml = serde_yaml::to_string(&config.masked())
        .map_err(|e| format!("could not render config: {e}"))?;
    print!("{yaml}");
    Ok(EXIT_PASS)
}

/// `flowproof config path`: the resolved path alone, for scripting or
/// opening in an editor.
pub fn cmd_path() -> Result<u8, String> {
    println!("{}", config_path()?.display());
    Ok(EXIT_PASS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `HOME` (and `XDG_CONFIG_HOME` on Linux) are process-global, exactly
    /// like `sap_com.rs`'s `SAP_USER` — tests that touch them must not run
    /// side by side.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("flowproof-config-test-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn env_pairs_map_every_field_to_its_documented_var() {
        let config = Config {
            sap: Some(SapProfile {
                user: Some("obeva".into()),
                password: Some("pw".into()),
                client: Some("100".into()),
                language: Some("EN".into()),
                connection: Some("TS3".into()),
            }),
            fiori: Some(FioriProfile {
                user: Some("obeva".into()),
                password: Some("pw2".into()),
                client: Some("200".into()),
                language: Some("DE".into()),
                base_url: Some("https://launchpad.test/".into()),
            }),
        };
        let pairs = config.env_pairs();
        // The table in plans/001-credential-config.md:167-178, verbatim.
        assert!(pairs.contains(&("SAP_USER", "obeva".to_string())));
        assert!(pairs.contains(&("SAP_PASSWORD", "pw".to_string())));
        assert!(pairs.contains(&("SAP_CLIENT", "100".to_string())));
        assert!(pairs.contains(&("SAP_LANGUAGE", "EN".to_string())));
        assert!(pairs.contains(&("SAP_CONNECTION", "TS3".to_string())));
        assert!(pairs.contains(&("FIORI_USER", "obeva".to_string())));
        assert!(pairs.contains(&("FIORI_PASSWORD", "pw2".to_string())));
        assert!(pairs.contains(&("FIORI_CLIENT", "200".to_string())));
        assert!(pairs.contains(&("FIORI_LANGUAGE", "DE".to_string())));
        assert!(pairs.contains(&("FIORI_BASE_URL", "https://launchpad.test/".to_string())));
        assert_eq!(pairs.len(), 10, "no extra, no missing: {pairs:?}");
    }

    #[test]
    fn an_empty_config_seeds_nothing() {
        assert!(Config::default().env_pairs().is_empty());
    }

    #[test]
    fn masked_replaces_only_the_password_and_only_when_set() {
        let config = Config {
            sap: Some(SapProfile {
                user: Some("obeva".into()),
                password: Some("secret".into()),
                ..Default::default()
            }),
            fiori: Some(FioriProfile::default()),
        };
        let masked = config.masked();
        let sap = masked.sap.as_ref().expect("sap profile present");
        assert_eq!(sap.user.as_deref(), Some("obeva"));
        assert_eq!(sap.password.as_deref(), Some("********"));
        // No password set on the fiori profile: nothing to mask, stays None.
        let fiori = masked.fiori.as_ref().expect("fiori profile present");
        assert_eq!(fiori.password, None);
    }

    #[test]
    fn save_then_load_round_trips_and_is_owner_only_on_unix() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("config.yaml");
        let config = Config {
            sap: Some(SapProfile {
                user: Some("obeva".into()),
                connection: Some("TS3".into()),
                ..Default::default()
            }),
            fiori: None,
        };
        save_to(&path, &config).expect("save");
        let loaded = load_from(&path).expect("load");
        assert_eq!(loaded, config);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "owner read/write only, got {mode:o}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_file_loads_as_an_empty_default_not_an_error() {
        let dir = temp_dir("missing");
        let path = dir.join("does-not-exist.yaml");
        assert_eq!(
            load_from(&path).expect("missing is not an error"),
            Config::default()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_malformed_file_is_a_named_error() {
        let dir = temp_dir("malformed");
        let path = dir.join("config.yaml");
        std::fs::write(&path, "sap: [this, is, not, a, mapping]").expect("write");
        let err = load_from(&path).expect_err("must not silently become empty");
        assert!(err.contains("not valid YAML"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The path resolution itself, against a fake `HOME` rather than the
    /// real one (plans/001-credential-config.md's Next checklist: "tests
    /// against a temp HOME/XDG_CONFIG_HOME, not the real one"). Exercises
    /// whichever of the two `dirs::config_dir()` actually uses per platform:
    /// macOS reads `HOME` directly, Linux falls back to `HOME` when
    /// `XDG_CONFIG_HOME` is unset — so overriding just `HOME` and clearing
    /// `XDG_CONFIG_HOME` pins both to one deterministic answer.
    #[test]
    #[cfg(unix)]
    fn config_path_resolves_under_a_fake_home() {
        let _guard = ENV.lock().expect("env lock");
        let home = temp_dir("fake-home");
        let previous_home = std::env::var_os("HOME");
        let previous_xdg = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("HOME", &home);
        std::env::remove_var("XDG_CONFIG_HOME");

        let resolved = config_path().expect("resolves under a fake HOME");
        assert!(
            resolved.starts_with(&home),
            "{resolved:?} must live under the fake HOME {home:?}"
        );
        assert_eq!(
            resolved.file_name().and_then(|n| n.to_str()),
            Some("config.yaml")
        );
        assert_eq!(
            resolved
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str()),
            Some("flowproof")
        );

        match previous_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match previous_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        std::fs::remove_dir_all(&home).ok();
    }

    /// Unlike the fake-`HOME` test above, this one is NOT `#[cfg(unix)]` —
    /// it runs against whatever the real environment actually is, on every
    /// platform this test suite executes on, Windows included. It cannot
    /// assert an exact path (Windows resolves `%APPDATA%` through the Known
    /// Folder API, not an env var a test can override the way `HOME` is on
    /// Unix — see plans/001-credential-config.md's "Next": Windows path
    /// resolution was never verified end to end for exactly this reason).
    /// What it CAN prove, cheaply, on every CI runner including Windows: the
    /// call succeeds at all and the shape is right — `flowproof/config.yaml`
    /// under an absolute directory, not a panic or a silent empty path.
    #[test]
    fn config_path_has_the_right_shape_on_whatever_platform_is_running() {
        let resolved = config_path().expect("dirs::config_dir() must resolve on a real machine");
        assert!(resolved.is_absolute(), "{resolved:?} must be absolute");
        assert_eq!(
            resolved.file_name().and_then(|n| n.to_str()),
            Some("config.yaml")
        );
        assert_eq!(
            resolved
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str()),
            Some("flowproof")
        );
    }

    #[test]
    fn seed_env_fills_gaps_only() {
        let _guard = ENV.lock().expect("env lock");
        std::env::remove_var("SAP_CLIENT");
        std::env::set_var("SAP_LANGUAGE", "ALREADY_SET");

        let config = Config {
            sap: Some(SapProfile {
                client: Some("100".into()),
                language: Some("FROM_CONFIG".into()),
                ..Default::default()
            }),
            fiori: None,
        };
        for (name, value) in config.env_pairs() {
            if std::env::var(name).is_err() {
                std::env::set_var(name, value);
            }
        }
        assert_eq!(
            std::env::var("SAP_CLIENT").as_deref(),
            Ok("100"),
            "unset var is filled from config"
        );
        assert_eq!(
            std::env::var("SAP_LANGUAGE").as_deref(),
            Ok("ALREADY_SET"),
            "already-set var is left alone, config never overrides"
        );

        std::env::remove_var("SAP_CLIENT");
        std::env::remove_var("SAP_LANGUAGE");
    }
}
