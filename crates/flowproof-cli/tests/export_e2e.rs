//! `flowproof export` end to end through `run_cli`: a flow spec resolves to
//! its committed trace, and the migrator JSON lands in `--out`.

use std::path::PathBuf;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

#[test]
fn export_writes_tosca_json_for_a_spec() {
    let out = std::env::temp_dir().join("flowproof-export-e2e.json");
    std::fs::remove_file(&out).ok();
    let spec = repo("examples/sap/create-order.flow.yaml");
    let code = flowproof_cli::run_cli([
        "export".into(),
        spec.into_os_string(),
        "--format".into(),
        "tosca-json".into(),
        "--out".into(),
        out.clone().into_os_string(),
    ]);
    assert_eq!(code, flowproof_cli::EXIT_PASS);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("written")).expect("json");
    assert_eq!(json["name"], "Create standard order");
    assert_eq!(
        json["steps"][0]["actions"][0]["keyword"],
        "StartTransaction"
    );
}

#[test]
fn export_of_a_missing_trace_is_an_error() {
    let code = flowproof_cli::run_cli(["export", "no/such.trace.jsonl"]);
    assert_eq!(code, flowproof_cli::EXIT_ERROR);
}
