//! Provider-neutral, bounded dataset input for row-isolated Flowproof runs.
//! Row payloads remain in memory only long enough to map declared scalars;
//! credentials and unmapped fields never enter commands, logs or evidence.

use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use flowproof_agent::{DataBinding, DataProvider, DataRowsMode, DataShard, SuiteManifest};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CLI_ROW_CAP: usize = 10_000;
pub const DESKTOP_ROW_CAP: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetIdentity {
    pub dataset_id: String,
    pub content_hash: String,
    pub row_count: u64,
    pub chunk_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowRef {
    pub dataset_id: String,
    pub content_hash: String,
    pub ordinal: u64,
}

impl RowRef {
    pub fn stable(&self) -> String {
        format!("{}:{}:{}", self.dataset_id, self.content_hash, self.ordinal)
    }
}

#[derive(Debug, Clone)]
pub struct MappedRow {
    pub row_ref: RowRef,
    pub values: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DatasetMetadata {
    id: String,
    status: String,
    row_count: u64,
    chunk_count: u64,
    content_hash: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChunkInfo {
    chunk_index: u64,
}

pub trait DataSource {
    fn pin(&self, dataset: &str) -> Result<DatasetIdentity, String>;
    fn rows(
        &self,
        identity: &DatasetIdentity,
        selected: &BTreeSet<u64>,
        visit: &mut dyn FnMut(u64, serde_json::Value) -> Result<(), String>,
    ) -> Result<(), String>;
}

pub struct DataMakerSource {
    base_url: String,
    api_key: String,
    project_id: Option<String>,
    agent: ureq::Agent,
}

impl DataMakerSource {
    pub fn from_env() -> Result<Self, String> {
        let base_url = std::env::var("DATAMAKER_API_URL")
            .map_err(|_| "DATAMAKER_API_URL is required for a Data Maker binding")?;
        let api_key = std::env::var("DATAMAKER_API_KEY")
            .map_err(|_| "DATAMAKER_API_KEY is required for a Data Maker binding")?;
        if base_url.trim().is_empty() || api_key.trim().is_empty() {
            return Err("DATAMAKER_API_URL and DATAMAKER_API_KEY must not be empty".into());
        }
        let config = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .proxy(ureq::Proxy::try_from_env())
            .timeout_global(Some(Duration::from_secs(30)))
            .build();
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            project_id: std::env::var("DATAMAKER_PROJECT_ID")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            agent: config.into(),
        })
    }

    fn request(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let mut request = self
            .agent
            .get(format!("{}{path}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .header("Accept", "application/json, application/x-ndjson");
        if let Some(project_id) = &self.project_id {
            request = request.header("X-Project-Id", project_id);
        }
        request
    }

    fn get(&self, path: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
        let mut last = String::new();
        for attempt in 0..3 {
            match self.request(path).call() {
                Ok(response) => return Ok(response),
                Err(error) => {
                    last = error.to_string();
                    if attempt < 2 {
                        std::thread::sleep(Duration::from_millis(100 * (1 << attempt)));
                    }
                }
            }
        }
        Err(format!(
            "Data Maker GET {path} failed after 3 attempts: {last}"
        ))
    }

    fn metadata(&self, dataset: &str) -> Result<DatasetMetadata, String> {
        let mut response = self.get(&format!("/datasets/{dataset}"))?;
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|error| format!("reading Data Maker dataset metadata: {error}"))?;
        serde_json::from_str(&text)
            .map_err(|error| format!("invalid Data Maker dataset metadata: {error}"))
    }

    fn assert_identity(&self, identity: &DatasetIdentity) -> Result<(), String> {
        let current = self.metadata(&identity.dataset_id)?;
        let current_hash = current.content_hash.unwrap_or_default();
        if current.id != identity.dataset_id
            || current_hash != identity.content_hash
            || current.row_count != identity.row_count
            || current.chunk_count != identity.chunk_count
        {
            return Err(format!(
                "dataset {} changed after the run was pinned; stopped before reading another chunk",
                identity.dataset_id
            ));
        }
        Ok(())
    }
}

impl DataSource for DataMakerSource {
    fn pin(&self, dataset: &str) -> Result<DatasetIdentity, String> {
        let metadata = self.metadata(dataset)?;
        if metadata.id != dataset {
            return Err(format!(
                "Data Maker returned dataset {} for requested {dataset}",
                metadata.id
            ));
        }
        if metadata.status != "ready" {
            return Err(format!(
                "dataset {dataset} is {}; it must be ready before replay starts",
                metadata.status
            ));
        }
        let content_hash = metadata
            .content_hash
            .filter(|hash| !hash.is_empty())
            .ok_or_else(|| format!("ready dataset {dataset} has no content hash"))?;
        let mut response = self.get(&format!("/datasets/{dataset}/chunks"))?;
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|error| format!("reading Data Maker chunk list: {error}"))?;
        let chunks: Vec<ChunkInfo> = serde_json::from_str(&text)
            .map_err(|error| format!("invalid Data Maker chunk list: {error}"))?;
        if chunks.len() as u64 != metadata.chunk_count
            || chunks
                .iter()
                .enumerate()
                .any(|(expected, chunk)| chunk.chunk_index != expected as u64)
        {
            return Err(format!(
                "dataset {dataset} chunk list does not match its pinned chunk count"
            ));
        }
        Ok(DatasetIdentity {
            dataset_id: metadata.id,
            content_hash,
            row_count: metadata.row_count,
            chunk_count: metadata.chunk_count,
        })
    }

    fn rows(
        &self,
        identity: &DatasetIdentity,
        selected: &BTreeSet<u64>,
        visit: &mut dyn FnMut(u64, serde_json::Value) -> Result<(), String>,
    ) -> Result<(), String> {
        if selected.is_empty() {
            return Ok(());
        }
        let last = *selected.last().expect("nonempty selection");
        let mut ordinal = 0_u64;
        for chunk in 0..identity.chunk_count {
            self.assert_identity(identity)?;
            let mut response =
                self.get(&format!("/datasets/{}/chunks/{chunk}", identity.dataset_id))?;
            let reader = BufReader::new(response.body_mut().as_reader());
            for (line_index, line) in reader.lines().enumerate() {
                let line = line.map_err(|error| {
                    format!(
                        "reading dataset {} chunk {chunk}: {error}",
                        identity.dataset_id
                    )
                })?;
                if line.trim().is_empty() {
                    continue;
                }
                if selected.contains(&ordinal) {
                    let row = serde_json::from_str(&line).map_err(|error| {
                        format!(
                            "dataset {} row {ordinal} (chunk {chunk}, line {}): malformed JSONL: {error}",
                            identity.dataset_id,
                            line_index + 1
                        )
                    })?;
                    visit(ordinal, row)?;
                }
                ordinal += 1;
                if ordinal > last {
                    return Ok(());
                }
            }
        }
        if ordinal != identity.row_count {
            return Err(format!(
                "dataset {} ended at {ordinal} rows but metadata pinned {}",
                identity.dataset_id, identity.row_count
            ));
        }
        Ok(())
    }
}

pub fn default_data_path(spec: &Path) -> PathBuf {
    let name = spec.file_name().unwrap_or_default().to_string_lossy();
    let base = name
        .strip_suffix(".flow.yaml")
        .or_else(|| name.strip_suffix(".yaml"))
        .or_else(|| name.strip_suffix(".yml"))
        .unwrap_or(&name);
    spec.with_file_name(format!("{base}.data.yaml"))
}

pub fn load_binding(spec: &Path, explicit: Option<&Path>) -> Result<Option<DataBinding>, String> {
    if let Some(path) = explicit {
        return read_binding(path).map(Some);
    }
    if spec.is_file() {
        let sidecar = default_data_path(spec);
        if sidecar.exists() {
            return read_binding(&sidecar).map(Some);
        }
    }
    let suite_dir = if spec.is_dir() {
        spec
    } else {
        spec.parent().unwrap_or(Path::new("."))
    };
    Ok(SuiteManifest::load_from_dir(suite_dir)
        .map_err(|error| error.to_string())?
        .and_then(|manifest| manifest.data))
}

fn read_binding(path: &Path) -> Result<DataBinding, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("reading data binding {}: {error}", path.display()))?;
    serde_yaml::from_str(&text)
        .map_err(|error| format!("parsing data binding {}: {error}", path.display()))
}

pub fn validate(binding: &DataBinding, cap: usize) -> Result<(), String> {
    if binding.rows.limit == 0 {
        return Err("data.rows.limit is required and must be greater than zero".into());
    }
    if binding.rows.limit > cap {
        return Err(format!(
            "data row limit {} exceeds the {}-row {} cap",
            binding.rows.limit,
            cap,
            if cap == DESKTOP_ROW_CAP {
                "Desktop"
            } else {
                "CLI/CI"
            }
        ));
    }
    if binding.map.is_empty() {
        return Err("data.map must contain at least one variable mapping".into());
    }
    for (name, path) in &binding.map {
        if !valid_env_name(name) {
            return Err(format!(
                "data mapping name `{name}` must match [A-Za-z_][A-Za-z0-9_]*"
            ));
        }
        if path.split('.').any(|part| part.is_empty()) {
            return Err(format!(
                "data mapping `{name}` has invalid dotted path `{path}`"
            ));
        }
    }
    match binding.rows.mode {
        DataRowsMode::Range => {
            let start = binding
                .rows
                .start
                .ok_or("data range mode requires rows.start")?;
            let end = binding
                .rows
                .end
                .ok_or("data range mode requires rows.end")?;
            if start >= end {
                return Err("data range requires rows.start < rows.end".into());
            }
        }
        _ if binding.rows.start.is_some() || binding.rows.end.is_some() => {
            return Err("rows.start and rows.end are only valid for range mode".into())
        }
        _ => {}
    }
    if matches!(binding.rows.mode, DataRowsMode::Sample) && binding.rows.seed.is_none() {
        return Err("data sample mode requires rows.seed".into());
    }
    if !matches!(binding.rows.mode, DataRowsMode::Sample) && binding.rows.seed.is_some() {
        return Err("rows.seed is only valid for sample mode".into());
    }
    validate_shard(binding.rows.shard.as_ref())?;
    if binding.execution.concurrency != 1 {
        return Err("data.execution.concurrency must be 1 in this release".into());
    }
    Ok(())
}

fn validate_shard(shard: Option<&DataShard>) -> Result<(), String> {
    if let Some(shard) = shard {
        if shard.total == 0 || shard.index >= shard.total {
            return Err("data shard requires total > 0 and zero-based index < total".into());
        }
    }
    Ok(())
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn selected_ordinals(binding: &DataBinding, row_count: u64) -> BTreeSet<u64> {
    let shard_accepts = |ordinal: u64| {
        binding
            .rows
            .shard
            .as_ref()
            .is_none_or(|shard| ordinal % u64::from(shard.total) == u64::from(shard.index))
    };
    let range = match binding.rows.mode {
        DataRowsMode::Range => {
            binding.rows.start.unwrap_or(0)..binding.rows.end.unwrap_or(row_count).min(row_count)
        }
        _ => 0..row_count,
    };
    if matches!(binding.rows.mode, DataRowsMode::Sample) {
        let seed = binding.rows.seed.unwrap_or_default();
        let mut heap: BinaryHeap<(u64, u64)> = BinaryHeap::new();
        for ordinal in range.filter(|ordinal| shard_accepts(*ordinal)) {
            let score = sample_score(seed, ordinal);
            if heap.len() < binding.rows.limit {
                heap.push((score, ordinal));
            } else if heap.peek().is_some_and(|top| score < top.0) {
                heap.pop();
                heap.push((score, ordinal));
            }
        }
        return heap.into_iter().map(|(_, ordinal)| ordinal).collect();
    }
    range
        .filter(|ordinal| shard_accepts(*ordinal))
        .take(binding.rows.limit)
        .collect()
}

fn sample_score(seed: u64, ordinal: u64) -> u64 {
    let mut value = ordinal ^ seed.wrapping_add(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

fn scalar(row: &serde_json::Value, path: &str) -> Result<String, String> {
    let mut value = row;
    for part in path.split('.') {
        value = value
            .as_object()
            .and_then(|object| object.get(part))
            .ok_or_else(|| format!("field `{path}` is missing"))?;
    }
    match value {
        serde_json::Value::Null => Ok(String::new()),
        serde_json::Value::String(value) => Ok(value.clone()),
        serde_json::Value::Number(value) => Ok(value.to_string()),
        serde_json::Value::Bool(value) => Ok(value.to_string()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => Err(format!(
            "field `{path}` must be a scalar string, number, boolean, or null"
        )),
    }
}

pub fn load_rows(
    binding: &DataBinding,
    cap: usize,
) -> Result<(DatasetIdentity, Vec<MappedRow>), String> {
    validate(binding, cap)?;
    let dataset = flowproof_trace::secret::resolve_refs(&binding.dataset)
        .map_err(|error| format!("resolving data.dataset: {error}"))?;
    let source: Box<dyn DataSource> = match binding.provider {
        DataProvider::Datamaker => Box::new(DataMakerSource::from_env()?),
    };
    let identity = source.pin(&dataset)?;
    let selected = selected_ordinals(binding, identity.row_count);
    let mut rows = Vec::with_capacity(selected.len());
    source.rows(&identity, &selected, &mut |ordinal, row| {
        let mut values = BTreeMap::new();
        for (name, path) in &binding.map {
            let value = scalar(&row, path).map_err(|error| {
                format!(
                    "dataset {} row {ordinal}, mapping `{name}` from `{path}`: {error}",
                    identity.dataset_id
                )
            })?;
            values.insert(name.clone(), value);
        }
        rows.push(MappedRow {
            row_ref: RowRef {
                dataset_id: identity.dataset_id.clone(),
                content_hash: identity.content_hash.clone(),
                ordinal,
            },
            values,
        });
        Ok(())
    })?;
    Ok((identity, rows))
}

pub fn mapping_digest(binding: &DataBinding) -> String {
    let bytes = serde_json::to_vec(&binding.map).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(mode: DataRowsMode) -> DataBinding {
        DataBinding {
            provider: DataProvider::Datamaker,
            dataset: "dataset-1".into(),
            map: BTreeMap::from([("EMAIL".into(), "contact.email".into())]),
            rows: flowproof_agent::DataRows {
                mode,
                limit: 3,
                start: None,
                end: None,
                seed: None,
                shard: None,
            },
            execution: Default::default(),
        }
    }

    #[test]
    fn deterministic_samples_and_shards_are_stable() {
        let mut sample = binding(DataRowsMode::Sample);
        sample.rows.seed = Some(42);
        assert_eq!(
            selected_ordinals(&sample, 100),
            selected_ordinals(&sample, 100)
        );
        let mut left = binding(DataRowsMode::All);
        left.rows.limit = 100;
        left.rows.shard = Some(DataShard { index: 0, total: 2 });
        let mut right = left.clone();
        right.rows.shard = Some(DataShard { index: 1, total: 2 });
        let a = selected_ordinals(&left, 20);
        let b = selected_ordinals(&right, 20);
        assert!(a.is_disjoint(&b));
        assert_eq!(a.len() + b.len(), 20);
    }

    #[test]
    fn dotted_mapping_accepts_only_scalars() {
        let row = serde_json::json!({"contact": {"email": "a@example.test"}, "tags": []});
        assert_eq!(scalar(&row, "contact.email").unwrap(), "a@example.test");
        assert!(scalar(&row, "contact.missing")
            .unwrap_err()
            .contains("missing"));
        assert!(scalar(&row, "tags").unwrap_err().contains("scalar"));
    }

    #[test]
    fn two_hundred_fifty_thousand_row_selection_stays_bounded() {
        let mut sample = binding(DataRowsMode::Sample);
        sample.rows.limit = 1_000;
        sample.rows.seed = Some(7);
        let selected = selected_ordinals(&sample, 250_000);
        assert_eq!(selected.len(), 1_000);
        assert!(selected.iter().all(|ordinal| *ordinal < 250_000));
    }

    #[test]
    fn desktop_and_cli_caps_fail_before_connecting() {
        let mut too_large = binding(DataRowsMode::All);
        too_large.rows.limit = DESKTOP_ROW_CAP + 1;
        assert!(validate(&too_large, DESKTOP_ROW_CAP)
            .unwrap_err()
            .contains("Desktop"));
        too_large.rows.limit = CLI_ROW_CAP + 1;
        assert!(validate(&too_large, CLI_ROW_CAP)
            .unwrap_err()
            .contains("CLI/CI"));
    }

    #[test]
    fn fake_datamaker_streams_multiple_chunks_and_pins_identity() {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("fake API binds");
        let base_url = format!("http://{}", server.server_addr());
        let handle = std::thread::spawn(move || {
            for _ in 0..6 {
                let request = server.recv().expect("request");
                assert_eq!(
                    request
                        .headers()
                        .iter()
                        .find(|h| h.field.equiv("X-API-Key"))
                        .map(|h| h.value.as_str()),
                    Some("read-only")
                );
                let path = request.url();
                let (body, content_type) = match path {
                    "/datasets/ready" => (
                        r#"{"id":"ready","status":"ready","rowCount":3,"chunkCount":2,"contentHash":"hash-1"}"#,
                        "application/json",
                    ),
                    "/datasets/ready/chunks" => {
                        (r#"[{"chunkIndex":0},{"chunkIndex":1}]"#, "application/json")
                    }
                    "/datasets/ready/chunks/0" => {
                        ("{\"id\":1}\n{\"id\":2}\n", "application/x-ndjson")
                    }
                    "/datasets/ready/chunks/1" => ("{\"id\":3}\n", "application/x-ndjson"),
                    other => panic!("unexpected fake API request {other}"),
                };
                request
                    .respond(tiny_http::Response::from_string(body).with_header(
                        tiny_http::Header::from_bytes("Content-Type", content_type).unwrap(),
                    ))
                    .unwrap();
            }
        });
        let source = DataMakerSource {
            base_url,
            api_key: "read-only".into(),
            project_id: Some("project-1".into()),
            agent: ureq::Agent::new_with_defaults(),
        };
        let identity = source.pin("ready").expect("ready dataset pins");
        let mut seen = Vec::new();
        source
            .rows(&identity, &BTreeSet::from([0, 2]), &mut |ordinal, row| {
                seen.push((ordinal, row["id"].as_u64().unwrap()));
                Ok(())
            })
            .expect("chunks stream");
        assert_eq!(seen, vec![(0, 1), (2, 3)]);
        handle.join().unwrap();
    }

    #[test]
    fn fake_datamaker_rejects_unready_before_any_chunk() {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("fake API binds");
        let base_url = format!("http://{}", server.server_addr());
        let handle = std::thread::spawn(move || {
            let request = server.recv().expect("metadata request");
            request.respond(tiny_http::Response::from_string(
                r#"{"id":"building","status":"building","rowCount":0,"chunkCount":0,"contentHash":null}"#,
            ).with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap())).unwrap();
        });
        let source = DataMakerSource {
            base_url,
            api_key: "read-only".into(),
            project_id: None,
            agent: ureq::Agent::new_with_defaults(),
        };
        assert!(source
            .pin("building")
            .unwrap_err()
            .contains("must be ready"));
        handle.join().unwrap();
    }
}
