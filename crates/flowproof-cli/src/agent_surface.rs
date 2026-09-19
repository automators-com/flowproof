//! Adapter boundary for an agent block. Reuses the standalone agent runner,
//! including cassette replay and its containment/assertion checks.
use flowproof_agent::FlowSpec;
use flowproof_driver::{AppDriver, DriverError, NoOpDriver, UiaSelector};
use serde_json::Value;
use std::{collections::HashMap, time::Duration};

pub(crate) struct AgentSurface {
    spec: FlowSpec,
    name: String,
    empty: NoOpDriver,
}
impl AgentSurface {
    pub(crate) fn new(spec: FlowSpec, name: String) -> Self {
        Self {
            spec,
            name,
            empty: NoOpDriver::new(),
        }
    }
}
fn substitute(value: &mut Value, captures: &HashMap<String, String>) -> Result<(), DriverError> {
    match value {
        Value::String(s) => {
            *s = flowproof_trace::captures::substitute(s, captures).map_err(DriverError::Uia)?
        }
        Value::Array(a) => {
            for v in a {
                substitute(v, captures)?;
            }
        }
        Value::Object(m) => {
            for v in m.values_mut() {
                substitute(v, captures)?;
            }
        }
        _ => {}
    }
    Ok(())
}
impl AppDriver for AgentSurface {
    fn launch(&mut self, _: &str, _: &str, _: Duration) -> Result<(), DriverError> {
        Ok(())
    }
    fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
        self.empty.screen_size()
    }
    fn element_exists(&mut self, s: &UiaSelector) -> Result<bool, DriverError> {
        self.empty.element_exists(s)
    }
    fn invoke(&mut self, s: &UiaSelector) -> Result<(), DriverError> {
        self.empty.invoke(s)
    }
    fn read_text(&mut self, s: &UiaSelector) -> Result<String, DriverError> {
        self.empty.read_text(s)
    }
    fn type_text(&mut self, s: &UiaSelector, t: &str) -> Result<(), DriverError> {
        self.empty.type_text(s, t)
    }
    fn agent_segment(
        &mut self,
        steps: &Value,
        cassette: Option<&Value>,
        captures: &HashMap<String, String>,
    ) -> Result<Value, DriverError> {
        let error = |e: String| DriverError::Uia(format!("agent surface {}: {e}", self.name));
        let steps = serde_json::from_value(steps.clone()).map_err(|e| error(e.to_string()))?;
        let spec = self
            .spec
            .surface_flow(&self.name, steps)
            .map_err(|e| error(e.to_string()))?;
        let mut spec = serde_json::to_value(spec).map_err(|e| error(e.to_string()))?;
        substitute(&mut spec, captures)?;
        let spec =
            FlowSpec::parse(&serde_json::to_string(&spec).map_err(|e| error(e.to_string()))?)
                .map_err(|e| error(e.to_string()))?;
        let dir =
            std::env::temp_dir().join(format!("flowproof-agent-segment-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&dir).map_err(|e| error(e.to_string()))?;
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let path = dir.join("cassette.json");
        let outcome = if let Some(cassette) = cassette {
            std::fs::write(
                &path,
                serde_json::to_vec(cassette).map_err(|e| error(e.to_string()))?,
            )
            .map_err(|e| error(e.to_string()))?;
            super::agent_flow::replay(&spec, &path).1
        } else {
            super::agent_flow::record(&spec, &path).1
        };
        outcome.map_err(error)?;
        serde_json::from_slice(&std::fs::read(path).map_err(|e| error(e.to_string()))?)
            .map_err(|e| error(e.to_string()))
    }
}
