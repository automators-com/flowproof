//! Fiori/SAP WebGUI framed input commit behavior, end to end in Chromium.
//!
//! Gated on FLOWPROOF_E2E=1 like the other browser tests. The fixture models
//! the live Fiori failure found while verifying plan 4: a prefilled WebGUI
//! field can show typed text briefly, then restore its old value unless the
//! field is committed through the same keyboard path a person uses.

use flowproof_agent::FlowSpec;

fn skip() -> bool {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping Fiori field commit E2E: set FLOWPROOF_E2E=1 to run it");
        return true;
    }
    false
}

fn write_fixture(name: &str, inner: &str, outer_body: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("flowproof-fiori-field-commit-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("inner.html"), inner).expect("inner fixture written");
    std::fs::write(dir.join("outer.html"), outer_body).expect("outer fixture written");
    let url = format!("file://{}", dir.join("outer.html").display());
    (dir, url)
}

fn record_spec(yaml: &str, url: &str, trace: &std::path::Path) -> Result<usize, String> {
    let spec = FlowSpec::parse(&yaml.replace("__URL__", url)).map_err(|e| e.to_string())?;
    let mut driver = flowproof_cli::driver_for("web").map_err(|e| e.to_string())?;
    let result = flowproof_agent::record(&spec, &mut driver, trace)
        .map(|summary| summary.steps)
        .map_err(|e| e.to_string());
    drop(driver);
    result
}

#[test]
fn sap_webgui_prefilled_frame_input_is_committed_and_read_back() {
    if skip() {
        return;
    }
    let sap_frame = r#"<!doctype html><html><body>
      <div id="webguiPage0">
        <label for="supplier">Supplier</label>
        <input id="supplier" value="10300016">
        <input id="next" aria-label="Next field">
        <div id="out">pending</div>
      </div>
      <script>
        const supplier = document.getElementById('supplier');
        const out = document.getElementById('out');
        let sawSelectAll = false;
        let sawBackspace = false;
        supplier.addEventListener('keydown', (event) => {
          if (event.ctrlKey && event.key.toLowerCase() === 'a') sawSelectAll = true;
          if (event.key === 'Backspace' && sawSelectAll) sawBackspace = true;
          if (event.key === 'Tab') {
            if (sawSelectAll && sawBackspace && supplier.value === '10300001') {
              out.textContent = 'accepted:' + supplier.value;
            } else {
              supplier.value = '10300016';
              out.textContent = 'restored:' + supplier.value;
            }
          }
        });
      </script>
    </body></html>"#;
    let outer = r#"<!doctype html><html><body>
      <iframe title="Application" src="inner.html"></iframe>
    </body></html>"#;
    let (dir, url) = write_fixture("accept", sap_frame, outer);
    let trace = dir.join("accept.trace.jsonl");
    let yaml = r#"
name: SAP framed commit
app: web
url: __URL__
steps:
  - Type 10300001 into the "Supplier" in the iframe "Application"
  - assert: the "css:#supplier" field in the iframe "Application" contains 10300001
  - assert: the "css:#out" in the iframe "Application" shows accepted:10300001
"#;

    let steps = record_spec(yaml, &url, &trace).expect("SAP-like field must be committed");
    assert_eq!(steps, 3);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(
        report.passed,
        "committed SAP-like field must replay: {report:#?}"
    );
    drop(driver);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn sap_webgui_revert_after_commit_fails_at_the_type_step() {
    if skip() {
        return;
    }
    let reverting_frame = r#"<!doctype html><html><body>
      <div id="webguiPage0">
        <label for="supplier">Supplier</label>
        <input id="supplier" value="10300016">
        <input id="next" aria-label="Next field">
        <div id="out">pending</div>
      </div>
      <script>
        const supplier = document.getElementById('supplier');
        const out = document.getElementById('out');
        supplier.addEventListener('keydown', (event) => {
          if (event.key === 'Tab') {
            setTimeout(() => {
              supplier.value = '10300016';
              out.textContent = 'restored:' + supplier.value;
            }, 0);
          }
        });
      </script>
    </body></html>"#;
    let outer = r#"<!doctype html><html><body>
      <iframe title="Application" src="inner.html"></iframe>
    </body></html>"#;
    let (dir, url) = write_fixture("revert", reverting_frame, outer);
    let trace = dir.join("revert.trace.jsonl");
    let yaml = r#"
name: SAP framed revert
app: web
url: __URL__
steps:
  - Type 10300001 into the "Supplier" in the iframe "Application"
  - assert: the "css:#out" in the iframe "Application" shows accepted
"#;

    let err = record_spec(yaml, &url, &trace).expect_err("restored value must fail the type step");
    assert!(
        err.contains("accepted a different value after commit"),
        "failure must name the commit/readback problem: {err}"
    );
    assert!(
        !err.contains("10300001") && !err.contains("10300016"),
        "field commit failures must not leak values that might be secret: {err}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn generic_framed_inputs_do_not_auto_tab() {
    if skip() {
        return;
    }
    let plain_frame = r#"<!doctype html><html><body>
      <label for="field">Field</label>
      <input id="field" value="old">
      <input id="next" aria-label="Next field">
      <div id="out">no-tab</div>
      <script>
        document.getElementById('field').addEventListener('keydown', (event) => {
          if (event.key === 'Tab') document.getElementById('out').textContent = 'tabbed';
        });
      </script>
    </body></html>"#;
    let outer = r#"<!doctype html><html><body>
      <iframe title="plain" src="inner.html"></iframe>
    </body></html>"#;
    let (dir, url) = write_fixture("plain-frame", plain_frame, outer);
    let trace = dir.join("plain.trace.jsonl");
    let yaml = r#"
name: Plain framed typing
app: web
url: __URL__
steps:
  - Type new into the "Field" in the iframe "plain"
  - assert: the "css:#field" field in the iframe "plain" contains new
  - assert: the "css:#out" in the iframe "plain" shows no-tab
"#;

    let steps = record_spec(yaml, &url, &trace).expect("plain framed input should not auto-tab");
    assert_eq!(steps, 3);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn generic_top_level_inputs_do_not_auto_tab() {
    if skip() {
        return;
    }
    let dir = std::env::temp_dir().join("flowproof-fiori-field-commit-top-level");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("page.html");
    std::fs::write(
        &page,
        r#"<!doctype html><html><body>
          <label for="field">Field</label>
          <input id="field" value="old">
          <input id="next" aria-label="Next field">
          <div id="out">no-tab</div>
          <script>
            document.getElementById('field').addEventListener('keydown', (event) => {
              if (event.key === 'Tab') document.getElementById('out').textContent = 'tabbed';
            });
          </script>
        </body></html>"#,
    )
    .expect("page written");
    let trace = dir.join("top.trace.jsonl");
    let yaml = r#"
name: Plain top-level typing
app: web
url: __URL__
steps:
  - Type new into the "Field" field
  - assert: the "Field" field contains new
  - assert: page shows no-tab
"#;

    let url = format!("file://{}", page.display());
    let steps = record_spec(yaml, &url, &trace).expect("top-level input should not auto-tab");
    assert_eq!(steps, 3);

    std::fs::remove_dir_all(&dir).ok();
}
