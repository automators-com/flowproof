//! The environment a contained agent actually receives.
//!
//! # The gap this closes
//!
//! `CreateProcessAsUserW` with `lpEnvironment: None` hands the child **the
//! calling process's** environment. On Linux that is harmless, because
//! `std::process::Command` layers the injected variables on top of an
//! inherited set. Here it is not: flowproof injects `OPENAI_BASE_URL`,
//! `ANTHROPIC_BASE_URL`, `FLOWPROOF_PROMPT` and the MCP stand-in handles per
//! run, and none of them are set on flowproof's own process.
//!
//! So a contained agent launched with `None` would see no proxy at all. At
//! replay it would reach for the real provider - the one failure a
//! determinism tool must never allow - and at record it would produce the "0
//! model calls" report that #188 exists to make legible. The symptom would
//! name the agent, not the launcher.
//!
//! # Format
//!
//! `NAME=VALUE\0NAME=VALUE\0\0`, UTF-16, sorted. Windows documents the block
//! as sorted; names are also **case-insensitive**, so an override of `path`
//! must replace an inherited `PATH` rather than sit beside it and let the
//! child pick whichever it finds first.

use std::collections::BTreeMap;

/// Build a UNICODE environment block from this process's environment plus
/// `overrides`.
///
/// Returns the UTF-16 buffer; the caller passes a pointer to it and must keep
/// it alive across the `CreateProcess*` call.
pub fn environment_block(overrides: &BTreeMap<String, String>) -> Vec<u16> {
    // Keyed by upper-case name so an override replaces the inherited entry
    // whatever case either used. The value keeps the ORIGINAL name, because
    // the child should see the name as it was written.
    let mut merged: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (name, value) in std::env::vars() {
        // Windows keeps hidden `=C:` drive-current-directory entries. They are
        // not ours to reorder or re-emit, and a malformed one poisons the whole
        // block, so anything with an empty name is dropped.
        if name.is_empty() {
            continue;
        }
        merged.insert(name.to_uppercase(), (name, value));
    }
    for (name, value) in overrides {
        if name.is_empty() {
            continue;
        }
        merged.insert(name.to_uppercase(), (name.clone(), value.clone()));
    }

    let mut out = Vec::new();
    for (_, (name, value)) in merged {
        out.extend(format!("{name}={value}").encode_utf16());
        out.push(0);
    }
    // A block is terminated by an extra NUL. An EMPTY block is still two NULs,
    // never one: a single NUL is read as "the block ends immediately" only if
    // something precedes it, and an empty buffer would be dereferenced as a
    // wild pointer.
    if out.is_empty() {
        out.push(0);
    }
    out.push(0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(block: &[u16]) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = Vec::new();
        for &c in block {
            if c == 0 {
                if cur.is_empty() {
                    break;
                }
                out.push(String::from_utf16_lossy(&cur));
                cur.clear();
            } else {
                cur.push(c);
            }
        }
        out
    }

    /// The injected variables reach the child. This is the whole point: with
    /// `lpEnvironment: None` they do not, and a replay agent would go to the
    /// real provider instead of the proxy.
    #[test]
    fn overrides_are_present_in_the_block() {
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "OPENAI_BASE_URL".to_string(),
            "http://127.0.0.1:9/v1".into(),
        );
        overrides.insert("FLOWPROOF_PROMPT".to_string(), "do the thing".into());

        let entries = parse(&environment_block(&overrides));
        assert!(entries
            .iter()
            .any(|e| e == "OPENAI_BASE_URL=http://127.0.0.1:9/v1"));
        assert!(entries.iter().any(|e| e == "FLOWPROOF_PROMPT=do the thing"));
    }

    /// The parent's environment is inherited too, so a contained agent keeps
    /// PATH and everything else it needs to run at all.
    #[test]
    fn the_parents_environment_is_inherited() {
        // Something guaranteed present in this process.
        std::env::set_var("FLOWPROOF_ENV_BLOCK_PROBE", "inherited");
        let entries = parse(&environment_block(&BTreeMap::new()));
        assert!(entries
            .iter()
            .any(|e| e == "FLOWPROOF_ENV_BLOCK_PROBE=inherited"));
        std::env::remove_var("FLOWPROOF_ENV_BLOCK_PROBE");
    }

    /// Names are case-insensitive on Windows, so an override REPLACES the
    /// inherited entry rather than sitting beside it. Two entries differing
    /// only in case would leave the child picking whichever it found first.
    #[test]
    fn an_override_replaces_an_inherited_name_whatever_its_case() {
        std::env::set_var("FLOWPROOF_CASE_PROBE", "from-parent");
        let mut overrides = BTreeMap::new();
        overrides.insert("flowproof_case_probe".to_string(), "from-override".into());

        let entries = parse(&environment_block(&overrides));
        let matching: Vec<_> = entries
            .iter()
            .filter(|e| e.to_uppercase().starts_with("FLOWPROOF_CASE_PROBE="))
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "exactly one entry survives: {matching:?}"
        );
        assert!(matching[0].ends_with("=from-override"), "{matching:?}");
        std::env::remove_var("FLOWPROOF_CASE_PROBE");
    }

    /// The block is double-NUL terminated, and an empty one is still two NULs
    /// rather than an empty buffer that would be dereferenced as a wild
    /// pointer.
    #[test]
    fn the_block_is_double_nul_terminated() {
        let block = environment_block(&BTreeMap::new());
        assert!(block.len() >= 2);
        assert_eq!(block[block.len() - 1], 0);
        assert_eq!(block[block.len() - 2], 0);
    }

    /// Sorted, as Windows documents the block to be.
    #[test]
    fn entries_are_sorted_case_insensitively() {
        let mut overrides = BTreeMap::new();
        overrides.insert("ZZ_FLOWPROOF_SORT".to_string(), "z".into());
        overrides.insert("AA_FLOWPROOF_SORT".to_string(), "a".into());

        let entries = parse(&environment_block(&overrides));
        let upper: Vec<String> = entries.iter().map(|e| e.to_uppercase()).collect();
        let mut sorted = upper.clone();
        sorted.sort();
        assert_eq!(upper, sorted, "the block must be sorted");
    }
}
