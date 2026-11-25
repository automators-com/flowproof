//! The Claude Code plugin in `plugin/` (plans/017-flowproof-mcp-server.md,
//! step 11) ships copies of the skills the binary embeds and the entry
//! `flowproof mcp install` writes. Claude Code reads the plugin straight from
//! the repository, so a copy that drifts is what users get.

use serde_json::{json, Value};

#[test]
fn the_plugin_skills_are_the_embedded_skills() {
    for (copy, embedded) in [
        (
            include_str!("../../../plugin/skills/flowproof-flows/SKILL.md"),
            include_str!("../skills/flowproof-flows/SKILL.md"),
        ),
        (
            include_str!("../../../plugin/skills/flowproof-config/SKILL.md"),
            include_str!("../skills/flowproof-config/SKILL.md"),
        ),
    ] {
        assert_eq!(
            copy, embedded,
            "plugin/skills/ drifted from crates/flowproof-cli/skills/: copy the file over"
        );
    }
}

#[test]
fn the_plugin_starts_the_server_as_mcp_install_does() {
    let config: Value =
        serde_json::from_str(include_str!("../../../plugin/.mcp.json")).expect("plugin .mcp.json");
    let expected = json!({ "type": "stdio", "command": "flowproof", "args": ["mcp"] });
    assert_eq!(config["mcpServers"]["flowproof"], expected);

    let marketplace: Value =
        serde_json::from_str(include_str!("../../../.claude-plugin/marketplace.json"))
            .expect("marketplace.json");
    assert_eq!(marketplace["plugins"][0]["source"], "./plugin");
}
