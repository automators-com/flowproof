//! Export a trace as the Tosca migrator's test-case JSON: the `Generic*`
//! test-case shape the Automators migrator reads with `file_format: JSON`
//! and turns into Tosca modules and test steps.
//!
//! The mapping is deliberately conservative. A step the migrator cannot
//! express faithfully (a page-wide text check, a web target with no DOM id)
//! is left out and
//! named in [`ToscaExport::warnings`], never guessed at: a Tosca test that
//! silently checks less than the recording did is worse than one that
//! visibly has a gap.

use serde_json::{json, Value};

use crate::format::{Action, Adapter, Assertion, Header, Selector, Step};
use crate::SelectorTier;

/// The exported test case plus every step that could not be carried over.
#[derive(Debug)]
pub struct ToscaExport {
    pub testcase: Value,
    pub warnings: Vec<String>,
}

/// One exported action, before steps are assembled. Keyword actions go in
/// a step of their own: the migrator skips module creation for a whole step
/// once any action in it is a keyword.
struct Exported {
    intent: String,
    keyword: bool,
    action: Value,
}

/// Convert a single-surface SAP GUI or web trace. Other adapters are an
/// error, not a partial export.
pub fn export(header: &Header, steps: &[Step]) -> Result<ToscaExport, String> {
    let sap = match header.app.adapter {
        Adapter::SapCom => true,
        Adapter::Web => false,
        other => {
            return Err(format!(
                "Tosca export supports sap and web traces; this trace uses the {other:?} adapter"
            ))
        }
    };
    let mut warnings = Vec::new();
    let mut out = Vec::new();
    let base = header.app.url.clone().unwrap_or_default();
    // SAP modules are named after the transaction the step ran in, web
    // modules after the site.
    let mut application = if sap { "SAP".into() } else { site(header) };
    if !sap && !base.is_empty() {
        let open =
            json!({"name": "Open URL", "keyword": "NavigateBrowser", "value": tosca_value(&base)});
        out.push(Exported {
            intent: format!("Open {base}"),
            keyword: true,
            action: open,
        });
    }
    if steps
        .iter()
        .any(|s| s.repeat.is_some() || !s.guards.is_empty())
    {
        warnings.push("repeat:/when: blocks are exported as the one recorded path".into());
    }
    for step in steps {
        if let Action::Launch(p) = &step.action {
            if let (true, Some(url)) = (sap, p.get("url").and_then(Value::as_str)) {
                application = transaction(url);
            }
        }
        match export_step(step, sap, &application, &base) {
            Ok(e) => out.push(e),
            Err(why) => warnings.push(format!(
                "{} ({}): not exported, {why}",
                step.id, step.intent
            )),
        }
    }
    let steps = Value::Array(assemble(out));
    let testcase = json!({
        "name": header.spec.as_ref().map_or("flowproof trace", |s| s.name.as_str()),
        "description": format!("Exported from flowproof trace {}", header.trace_id),
        "testcasetype": "TestCase",
        // The migrator skips a test case whose stored hash equals the
        // incoming one, so an empty hash would freeze the first import.
        "hash": fnv1a(&steps.to_string()),
        "steps": steps,
    });
    Ok(ToscaExport { testcase, warnings })
}

/// Consecutive actions with the same intent (one authored step) and the
/// same kind (keyword or element) become one test step.
fn assemble(actions: Vec<Exported>) -> Vec<Value> {
    let mut steps: Vec<Exported> = Vec::new();
    for e in actions {
        match steps.last_mut() {
            Some(s) if s.intent == e.intent && s.keyword == e.keyword => {
                s.action.as_array_mut().expect("array").push(e.action)
            }
            _ => steps.push(Exported {
                action: json!([e.action]),
                ..e
            }),
        }
    }
    let step = |(i, s): (usize, Exported)| json!({"name": s.intent, "order": (i + 1).to_string(), "actions": s.action});
    steps.into_iter().enumerate().map(step).collect()
}

/// `base` is the web flow's URL, which relative `Go to`s resolve against.
fn export_step(step: &Step, sap: bool, application: &str, base: &str) -> Result<Exported, String> {
    let exported = |keyword, action| Exported {
        intent: step.intent.clone(),
        keyword,
        action,
    };
    let keyword = |name: &str, kw: &str, value: String| {
        Ok(exported(
            true,
            json!({"name": name, "keyword": kw, "value": value}),
        ))
    };
    let mut extra = json!({});
    let (mode, value, fallback) = match &step.action {
        Action::Launch(p) => {
            let url = p
                .get("url")
                .and_then(Value::as_str)
                .ok_or("a reload has no Tosca keyword")?;
            return match sap {
                true => keyword("Start transaction", "StartTransaction", transaction(url)),
                false => keyword(
                    "Open URL",
                    "NavigateBrowser",
                    tosca_value(&absolute(base, url)),
                ),
            };
        }
        Action::PressKey(p) => {
            let keys = tosca_key(&p.key, p.modifiers.is_empty())
                .ok_or(format!("key {} has no Tosca spelling", p.key))?;
            return keyword("Send keys", "SendKey", keys);
        }
        Action::TypeText(p) => {
            let submit = if p.submit == Some(true) {
                "{ENTER}"
            } else {
                ""
            };
            (
                "Input",
                format!("{}{submit}", tosca_value(&p.text)),
                "TextBox",
            )
        }
        Action::Click(_) => ("Input", "Click".into(), "Button"),
        Action::DoubleClick(_) => ("Input", "DoubleClick".into(), "Button"),
        Action::RightClick(_) => ("Input", "RightClick".into(), "Button"),
        Action::SetChecked(p) => {
            let on = p.get("checked").and_then(Value::as_bool).unwrap_or(true);
            ("Input", on.to_string(), "CheckBox")
        }
        Action::Capture(p) => {
            let name = p.get("name").and_then(Value::as_str).unwrap_or_default();
            extra = json!({"outputvalue": name});
            ("Buffer", name.to_string(), "TextBox")
        }
        Action::Assert(Assertion::ElementState { expect, .. }) => {
            element_check(expect, &mut extra)?
        }
        other => return Err(format!("{} has no Tosca equivalent", kind(other))),
    };
    let element = match sap {
        true => sap_element(&step.selectors, fallback, application)?,
        false => web_element(&step.selectors, fallback, application)?,
    };
    let mut action = json!({"name": element["name"], "actionmode": mode, "element": element});
    if !value.is_empty() {
        action["value"] = value.into();
    }
    action
        .as_object_mut()
        .expect("object")
        .extend(extra.as_object().cloned().unwrap_or_default());
    Ok(exported(false, action))
}

/// An element-scoped `element_state` reading as a Verify or Exist action.
fn element_check(
    expect: &Value,
    extra: &mut Value,
) -> Result<(&'static str, String, &'static str), String> {
    if expect.get("scope").and_then(Value::as_str) == Some("surface") {
        return Err("page-wide checks have no Tosca module to verify against".into());
    }
    let s = |k: &str| expect.get(k).and_then(Value::as_str).map(tosca_value);
    let b = |k: &str| expect.get(k).and_then(Value::as_bool);
    if let Some(v) = s("value_equals") {
        Ok(("Verify", v, "TextBox"))
    } else if let Some(v) = s("value_contains") {
        Ok(("Verify", format!("*{v}*"), "TextBox"))
    } else if let Some(c) = b("checked") {
        Ok((
            "Verify",
            if c { "True" } else { "False" }.into(),
            "CheckBox",
        ))
    } else if let Some(present) = b("present") {
        if !present {
            *extra = json!({"expression": {"operator": "not"}});
        }
        Ok(("Exist", String::new(), "TextBox"))
    } else if b("enabled") == Some(true) {
        Ok(("Enabled", String::new(), "TextBox"))
    } else {
        Err(format!("the check {expect} has no Tosca equivalent"))
    }
}

fn sap_element(selectors: &[Selector], fallback: &str, application: &str) -> Result<Value, String> {
    let label = payload(selectors, SelectorTier::TextAnchor, "text");
    let (name, business_type, props) = match payload(selectors, SelectorTier::NativeId, "id") {
        Some(id) => {
            // A scripting id's last segment is the control's type prefix
            // followed by its technical name: `ctxtVBAK-AUART`.
            let last = id.rsplit('/').next().unwrap_or(id);
            let (prefix, tech) = last.split_at(
                last.find(|c: char| !c.is_ascii_lowercase())
                    .unwrap_or(last.len()),
            );
            let mut props = vec![json!({"name": "RelativeId", "value": id})];
            let tech = (!tech.is_empty() && !tech.starts_with('[')).then_some(tech);
            if let Some(tech) = tech {
                props.push(json!({"name": "Name", "value": tech}));
            }
            (
                label.or(tech).unwrap_or(last),
                sap_type(prefix).unwrap_or(fallback),
                props,
            )
        }
        None => {
            let label = label.ok_or("the SAP target has neither a scripting id nor a label")?;
            (
                label,
                fallback,
                vec![json!({"name": "text", "value": label})],
            )
        }
    };
    Ok(json!({
        "name": name, "engine": "SAP", "application": application, "context": "",
        "steering_strategy": "SAPGUI_CBTA", "business_type": business_type,
        "interface_type": "GUI", "properties": props,
    }))
}

fn web_element(selectors: &[Selector], fallback: &str, application: &str) -> Result<Value, String> {
    let css = payload(selectors, SelectorTier::NativeId, "css");
    let plain = |i: &&str| {
        !i.is_empty()
            && i.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
    };
    let id = css
        .and_then(|c| c.strip_prefix('#'))
        .filter(plain)
        .ok_or_else(|| match css {
            Some(css) => format!("css selector {css} is not a plain DOM id"),
            None => "the web target has no DOM id".to_string(),
        })?;
    let business_type = match payload(selectors, SelectorTier::A11y, "role") {
        Some("textbox" | "searchbox") => "TextBox",
        Some("button") => "Button",
        Some("link") => "Link",
        Some("checkbox") => "CheckBox",
        Some("combobox") => "ComboBox",
        Some("radio") => "RadioButton",
        _ => fallback,
    };
    let name = payload(selectors, SelectorTier::A11y, "name")
        .or_else(|| payload(selectors, SelectorTier::TextAnchor, "text"))
        .unwrap_or(id);
    // The page title is not recorded yet, so the module matches any page.
    Ok(json!({
        "name": name, "engine": "Html", "application": application, "context": "*",
        "steering_strategy": "Html_NWBC", "business_type": business_type,
        "interface_type": "GUI", "properties": [{"name": "html id", "value": id}],
    }))
}

/// The host of a web flow's URL, or its name when the URL is a `${VAR}`.
fn site(header: &Header) -> String {
    let url = header.app.url.as_deref().unwrap_or_default();
    let host = url
        .split("://")
        .nth(1)
        .and_then(|r| r.split(['/', ':']).next());
    match host {
        Some(h) if !h.is_empty() && !h.contains('$') => h.to_string(),
        _ => header
            .spec
            .as_ref()
            .map_or("web".into(), |s| s.name.clone()),
    }
}

/// Resolve a root-relative `Go to /path` against the flow's URL.
fn absolute(base: &str, url: &str) -> String {
    if !url.starts_with('/') {
        return url.to_string();
    }
    let after_scheme = base.find("://").map_or(0, |i| i + 3);
    let origin = base[after_scheme..]
        .find('/')
        .map_or(base.len(), |j| after_scheme + j);
    format!("{}{url}", &base[..origin])
}

fn payload<'a>(selectors: &'a [Selector], tier: SelectorTier, key: &str) -> Option<&'a str> {
    selectors
        .iter()
        .filter(|s| s.tier == tier)
        .find_map(|s| s.payload.get(key)?.as_str())
}

fn sap_type(prefix: &str) -> Option<&'static str> {
    Some(match prefix {
        "txt" | "ctxt" | "pwd" => "TextBox",
        "btn" => "Button",
        "chk" => "CheckBox",
        "rad" => "RadioButton",
        "cmb" => "ComboBox",
        "tbl" => "Table",
        "lbl" => "Label",
        _ => return None,
    })
}

/// `/nVA01` or `/oVA01` → `VA01`.
fn transaction(url: &str) -> String {
    let code = url
        .strip_prefix('/')
        .map_or(url, |t| t.strip_prefix(['n', 'N', 'o', 'O']).unwrap_or(t));
    code.to_ascii_uppercase()
}

/// `${captured.x}` is a buffer in Tosca; any other `${VAR}` is a test
/// configuration parameter.
fn tosca_value(text: &str) -> String {
    let re = regex::Regex::new(r"\$\{([^}]+)\}").expect("valid regex");
    re.replace_all(text, |c: &regex::Captures| {
        match c[1].strip_prefix("captured.") {
            Some(name) => format!("{{B[{name}]}}"),
            None => format!("{{CP[{}]}}", &c[1]),
        }
    })
    .into_owned()
}

/// The keys the migrator's `SendKey` keyword knows. A chord is left out
/// rather than sent as a bare key.
fn tosca_key(key: &str, no_modifiers: bool) -> Option<String> {
    let fkey = key
        .strip_prefix('F')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=12).contains(&n));
    let name = match key {
        "Enter" => "ENTER",
        "Escape" => "ESC",
        "Tab" => "TAB",
        k if fkey => k,
        _ => return None,
    };
    no_modifiers.then(|| format!("{{{name}}}"))
}

fn kind(action: &Action) -> String {
    let v = serde_json::to_value(action).unwrap_or_default();
    let kind = v["params"]["kind"]
        .as_str()
        .map(|k| format!("{k} assertion"));
    kind.or(v["type"].as_str().map(String::from))
        .unwrap_or_default()
}

/// A stable, dependency-free content hash (FNV-1a).
fn fnv1a(s: &str) -> String {
    let h = s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{h:016x}")
}
