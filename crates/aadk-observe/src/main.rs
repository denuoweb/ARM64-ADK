use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use aadk_proto::aadk::v1::{
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    observe_service_server::{ObserveService, ObserveServiceServer},
    ErrorCode, ErrorDetail, ExportEvidenceBundleRequest, ExportEvidenceBundleResponse,
    ExportSupportBundleRequest, ExportSupportBundleResponse, GetJobRequest, Id, JobCompleted,
    JobEvent, JobFailed, JobLogAppended, JobProgress, JobProgressUpdated, JobState,
    JobStateChanged, KeyValue, ListRunOutputsRequest, ListRunOutputsResponse, ListRunsRequest,
    ListRunsResponse, LogChunk, PageInfo, Pagination, PublishJobEventRequest, ReloadStateRequest,
    ReloadStateResponse, RunFilter, RunId, RunOutput, RunOutputFilter, RunOutputKind,
    RunOutputSummary, RunRecord, StartJobRequest, StreamJobEventsRequest, Timestamp,
    UpsertRunOutputsRequest, UpsertRunOutputsResponse, UpsertRunRequest, UpsertRunResponse,
};
use aadk_util::{
    data_dir, job_addr, now_millis, now_ts, serve_grpc_with_telemetry, write_json_atomic,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{watch, Mutex};
use tonic::{transport::Channel, Request, Response, Status};
use tracing::warn;
use uuid::Uuid;
use zip::{write::FileOptions, CompressionMethod, ZipWriter};

const STATE_FILE_NAME: &str = "observe.json";
const JOB_STATE_FILE_NAME: &str = "jobs.json";
const DEFAULT_PAGE_SIZE: usize = 25;
const MAX_RUNS: usize = 200;
const DEFAULT_BUNDLE_RETENTION_DAYS: u64 = 30;
const DEFAULT_BUNDLE_MAX: usize = 50;
const DEFAULT_TMP_RETENTION_HOURS: u64 = 24;

#[derive(Default)]
struct State {
    runs: Vec<RunRecordEntry>,
    outputs: Vec<RunOutputEntry>,
}

#[derive(Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    runs: Vec<RunRecordEntry>,
    #[serde(default)]
    outputs: Vec<RunOutputEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RunRecordEntry {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    run_id: String,
    #[serde(default)]
    correlation_id: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    target_id: Option<String>,
    #[serde(default)]
    toolchain_set_id: Option<String>,
    started_at: i64,
    #[serde(default)]
    finished_at: Option<i64>,
    result: String,
    #[serde(default)]
    job_ids: Vec<String>,
    #[serde(default)]
    summary: Vec<SummaryEntry>,
    #[serde(default)]
    output_summary: RunOutputSummaryEntry,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SummaryEntry {
    key: String,
    value: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct RunOutputSummaryEntry {
    bundle_count: u32,
    artifact_count: u32,
    #[serde(default)]
    last_updated_at: Option<i64>,
    #[serde(default)]
    last_bundle_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
enum RunOutputKindEntry {
    #[default]
    Unspecified,
    Bundle,
    Artifact,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RunOutputEntry {
    output_id: String,
    run_id: String,
    #[serde(default)]
    kind: RunOutputKindEntry,
    #[serde(default)]
    output_type: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    metadata: Vec<SummaryEntry>,
}

fn merge_summary(entry: &mut RunRecordEntry, updates: Vec<KeyValue>) {
    for item in updates {
        let key = item.key.trim();
        if key.is_empty() {
            continue;
        }
        if let Some(existing) = entry.summary.iter_mut().find(|entry| entry.key == key) {
            existing.value = item.value;
        } else {
            entry.summary.push(SummaryEntry {
                key: key.to_string(),
                value: item.value,
            });
        }
    }
}

fn run_output_kind_entry(kind: i32) -> RunOutputKindEntry {
    match RunOutputKind::try_from(kind).unwrap_or(RunOutputKind::Unspecified) {
        RunOutputKind::Bundle => RunOutputKindEntry::Bundle,
        RunOutputKind::Artifact => RunOutputKindEntry::Artifact,
        RunOutputKind::Unspecified => RunOutputKindEntry::Unspecified,
    }
}

fn run_output_kind_proto(kind: RunOutputKindEntry) -> i32 {
    match kind {
        RunOutputKindEntry::Bundle => RunOutputKind::Bundle as i32,
        RunOutputKindEntry::Artifact => RunOutputKind::Artifact as i32,
        RunOutputKindEntry::Unspecified => RunOutputKind::Unspecified as i32,
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct JobHistoryFile {
    jobs: Vec<JobHistoryEntry>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct JobHistoryEntry {
    job_id: String,
    history: Vec<JobHistoryEvent>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct JobHistoryEvent {
    payload: Option<JobHistoryPayload>,
}

#[derive(Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
enum JobHistoryPayload {
    Log {
        chunk: Option<JobLogChunk>,
    },
    #[serde(other)]
    Other,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct JobLogChunk {
    stream: String,
    data: Vec<u8>,
    truncated: bool,
}

#[derive(Clone)]
struct Svc {
    state: Arc<Mutex<State>>,
}

struct BundlePlan {
    output_path: PathBuf,
    items: Vec<BundleItem>,
}

enum BundleItem {
    File { source: PathBuf, name: String },
    Generated { name: String, contents: Vec<u8> },
}

#[derive(Serialize)]
struct BundleManifest {
    bundle_type: String,
    created_at: i64,
    run_id: Option<String>,
    correlation_id: Option<String>,
    includes: BundleIncludes,
}

#[derive(Serialize)]
struct BundleIncludes {
    include_logs: bool,
    include_config: bool,
    include_toolchain_provenance: bool,
    include_recent_runs: bool,
    recent_runs_limit: u32,
}

fn default_schema_version() -> u32 {
    1
}

fn state_file_path() -> PathBuf {
    aadk_util::state_file_path(STATE_FILE_NAME)
}

fn job_state_file_path() -> PathBuf {
    aadk_util::state_file_path(JOB_STATE_FILE_NAME)
}

fn bundles_dir() -> PathBuf {
    data_dir().join("bundles")
}

fn read_env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn read_env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn cleanup_bundles_best_effort() {
    if let Err(err) = cleanup_bundles() {
        warn!("observe bundle cleanup failed: {}", err);
    }
}

fn cleanup_bundles() -> io::Result<()> {
    let dir = bundles_dir();
    if !dir.exists() {
        return Ok(());
    }
    let retention_days = read_env_u64(
        "AADK_OBSERVE_BUNDLE_RETENTION_DAYS",
        DEFAULT_BUNDLE_RETENTION_DAYS,
    );
    let max_bundles = read_env_usize("AADK_OBSERVE_BUNDLE_MAX", DEFAULT_BUNDLE_MAX);
    let tmp_hours = read_env_u64(
        "AADK_OBSERVE_TMP_RETENTION_HOURS",
        DEFAULT_TMP_RETENTION_HOURS,
    );
    let now = std::time::SystemTime::now();
    let max_age = if retention_days == 0 {
        None
    } else {
        Some(Duration::from_secs(
            retention_days.saturating_mul(24 * 60 * 60),
        ))
    };
    let tmp_age = if tmp_hours == 0 {
        None
    } else {
        Some(Duration::from_secs(tmp_hours.saturating_mul(60 * 60)))
    };

    let mut bundles = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let metadata = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if metadata.is_dir() {
            if let Some(tmp_age) = tmp_age {
                if name.starts_with("tmp") {
                    if let Ok(modified) = metadata.modified() {
                        if now
                            .duration_since(modified)
                            .unwrap_or_else(|_| Duration::from_secs(0))
                            > tmp_age
                        {
                            let _ = fs::remove_dir_all(&path);
                        }
                    }
                }
            }
            continue;
        }
        if metadata.is_file() && path.extension().map(|ext| ext == "zip").unwrap_or(false) {
            let modified = metadata.modified().unwrap_or(now);
            bundles.push((path, modified));
        }
    }

    if let Some(max_age) = max_age {
        for (path, modified) in &bundles {
            if now
                .duration_since(*modified)
                .unwrap_or_else(|_| Duration::from_secs(0))
                > max_age
            {
                let _ = fs::remove_file(path);
            }
        }
    }

    if max_bundles != 0 {
        let mut remaining = Vec::new();
        for (path, modified) in bundles {
            if path.exists() {
                remaining.push((path, modified));
            }
        }
        if remaining.len() > max_bundles {
            remaining.sort_by_key(|(_, modified)| *modified);
            let remove_count = remaining.len() - max_bundles;
            for (path, _) in remaining.into_iter().take(remove_count) {
                let _ = fs::remove_file(path);
            }
        }
    }

    Ok(())
}

fn load_state() -> State {
    let path = state_file_path();
    match fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str::<StateFile>(&data) {
            Ok(file) => {
                let mut state = State {
                    runs: file.runs,
                    outputs: file.outputs,
                };
                for run in state.runs.iter_mut() {
                    run.output_summary = compute_output_summary(&run.run_id, &state.outputs);
                }
                state
            }
            Err(err) => {
                warn!("Failed to parse {}: {}", path.display(), err);
                State::default()
            }
        },
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                warn!("Failed to read {}: {}", path.display(), err);
            }
            State::default()
        }
    }
}

fn save_state(state: &State) -> io::Result<()> {
    let file = StateFile {
        runs: state.runs.clone(),
        outputs: state.outputs.clone(),
    };
    write_json_atomic(&state_file_path(), &file)
}

fn upsert_run(state: &mut State, run: RunRecordEntry) {
    state.runs.retain(|item| item.run_id != run.run_id);
    state.runs.insert(0, run);
    if state.runs.len() > MAX_RUNS {
        state.runs.truncate(MAX_RUNS);
    }
}

fn normalize_id(id: Option<&Id>) -> Option<String> {
    id.map(|value| value.value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn run_matches_filter(run: &RunRecordEntry, filter: &RunFilter) -> bool {
    if let Some(run_id) = filter
        .run_id
        .as_ref()
        .map(|id| id.value.trim())
        .filter(|value| !value.is_empty())
    {
        if run.run_id != run_id {
            return false;
        }
    }
    if !filter.correlation_id.trim().is_empty()
        && run.correlation_id.as_deref().unwrap_or("") != filter.correlation_id
    {
        return false;
    }
    if let Some(project_id) = filter
        .project_id
        .as_ref()
        .map(|id| id.value.trim())
        .filter(|value| !value.is_empty())
    {
        if run.project_id.as_deref().unwrap_or("") != project_id {
            return false;
        }
    }
    if let Some(target_id) = filter
        .target_id
        .as_ref()
        .map(|id| id.value.trim())
        .filter(|value| !value.is_empty())
    {
        if run.target_id.as_deref().unwrap_or("") != target_id {
            return false;
        }
    }
    if let Some(toolchain_set_id) = filter
        .toolchain_set_id
        .as_ref()
        .map(|id| id.value.trim())
        .filter(|value| !value.is_empty())
    {
        if run.toolchain_set_id.as_deref().unwrap_or("") != toolchain_set_id {
            return false;
        }
    }
    if !filter.result.trim().is_empty() && run.result != filter.result {
        return false;
    }
    true
}

fn compute_output_summary(run_id: &str, outputs: &[RunOutputEntry]) -> RunOutputSummaryEntry {
    let mut summary = RunOutputSummaryEntry::default();
    let mut last_bundle: Option<&RunOutputEntry> = None;

    for output in outputs.iter().filter(|item| item.run_id == run_id) {
        match output.kind {
            RunOutputKindEntry::Bundle => summary.bundle_count += 1,
            RunOutputKindEntry::Artifact => summary.artifact_count += 1,
            RunOutputKindEntry::Unspecified => {}
        }
        if summary
            .last_updated_at
            .map(|ts| output.created_at > ts)
            .unwrap_or(true)
        {
            summary.last_updated_at = Some(output.created_at);
        }
        if output.kind == RunOutputKindEntry::Bundle
            && last_bundle
                .map(|entry| output.created_at > entry.created_at)
                .unwrap_or(true)
        {
            last_bundle = Some(output);
        }
    }

    summary.last_bundle_id = last_bundle.map(|entry| entry.output_id.clone());
    summary
}

fn refresh_run_output_summary(state: &mut State, run_id: &str) -> RunOutputSummaryEntry {
    let summary = compute_output_summary(run_id, &state.outputs);
    if let Some(run) = state.runs.iter_mut().find(|item| item.run_id == run_id) {
        run.output_summary = summary.clone();
    } else {
        let mut run = RunRecordEntry::new(run_id.to_string(), "running");
        run.output_summary = summary.clone();
        state.runs.insert(0, run);
        if state.runs.len() > MAX_RUNS {
            state.runs.truncate(MAX_RUNS);
        }
    }
    summary
}

fn upsert_output_entry(state: &mut State, output: RunOutputEntry) -> RunOutputSummaryEntry {
    let run_id = output.run_id.clone();
    state
        .outputs
        .retain(|entry| entry.output_id != output.output_id);
    state.outputs.insert(0, output);
    refresh_run_output_summary(state, &run_id)
}

fn output_matches_filter(entry: &RunOutputEntry, filter: &RunOutputFilter) -> bool {
    let kind_filter = RunOutputKind::try_from(filter.kind).unwrap_or(RunOutputKind::Unspecified);
    if kind_filter != RunOutputKind::Unspecified && entry.kind != run_output_kind_entry(filter.kind)
    {
        return false;
    }
    if !filter.output_type.trim().is_empty() && entry.output_type != filter.output_type.trim() {
        return false;
    }
    if !filter.path_contains.trim().is_empty() && !entry.path.contains(filter.path_contains.trim())
    {
        return false;
    }
    if !filter.label_contains.trim().is_empty()
        && !entry.label.contains(filter.label_contains.trim())
    {
        return false;
    }
    true
}

impl RunRecordEntry {
    fn new(run_id: String, result: &str) -> Self {
        let now = now_millis();
        Self {
            schema_version: default_schema_version(),
            run_id,
            correlation_id: None,
            project_id: None,
            target_id: None,
            toolchain_set_id: None,
            started_at: now,
            finished_at: None,
            result: result.into(),
            job_ids: Vec::new(),
            summary: Vec::new(),
            output_summary: RunOutputSummaryEntry::default(),
        }
    }

    fn to_proto(&self) -> RunRecord {
        RunRecord {
            run_id: Some(RunId {
                value: self.run_id.clone(),
            }),
            project_id: self.project_id.as_ref().map(|value| Id {
                value: value.clone(),
            }),
            target_id: self.target_id.as_ref().map(|value| Id {
                value: value.clone(),
            }),
            toolchain_set_id: self.toolchain_set_id.as_ref().map(|value| Id {
                value: value.clone(),
            }),
            started_at: Some(Timestamp {
                unix_millis: self.started_at,
            }),
            finished_at: self.finished_at.map(|ts| Timestamp { unix_millis: ts }),
            result: self.result.clone(),
            job_ids: self
                .job_ids
                .iter()
                .map(|value| Id {
                    value: value.clone(),
                })
                .collect(),
            summary: self
                .summary
                .iter()
                .map(|item| KeyValue {
                    key: item.key.clone(),
                    value: item.value.clone(),
                })
                .collect(),
            correlation_id: self.correlation_id.clone().unwrap_or_default(),
            output_summary: Some(self.output_summary.to_proto()),
        }
    }
}

impl RunOutputSummaryEntry {
    fn to_proto(&self) -> RunOutputSummary {
        RunOutputSummary {
            bundle_count: self.bundle_count,
            artifact_count: self.artifact_count,
            updated_at: self.last_updated_at.map(|ts| Timestamp { unix_millis: ts }),
            last_bundle_id: self.last_bundle_id.clone().unwrap_or_default(),
        }
    }
}

impl RunOutputEntry {
    fn to_proto(&self) -> RunOutput {
        RunOutput {
            output_id: self.output_id.clone(),
            run_id: Some(RunId {
                value: self.run_id.clone(),
            }),
            kind: run_output_kind_proto(self.kind),
            output_type: self.output_type.clone(),
            path: self.path.clone(),
            label: self.label.clone(),
            job_id: self.job_id.as_ref().map(|value| Id {
                value: value.clone(),
            }),
            created_at: if self.created_at == 0 {
                None
            } else {
                Some(Timestamp {
                    unix_millis: self.created_at,
                })
            },
            metadata: self
                .metadata
                .iter()
                .map(|item| KeyValue {
                    key: item.key.clone(),
                    value: item.value.clone(),
                })
                .collect(),
        }
    }
}

impl Svc {
    fn new() -> Self {
        cleanup_bundles_best_effort();
        Self {
            state: Arc::new(Mutex::new(load_state())),
        }
    }
}

async fn connect_job() -> Result<JobServiceClient<Channel>, Status> {
    let addr = job_addr();
    let endpoint = format!("http://{addr}");
    let channel = Channel::from_shared(endpoint)
        .map_err(|e| Status::internal(format!("invalid job endpoint: {e}")))?
        .connect()
        .await
        .map_err(|e| Status::unavailable(format!("job service unavailable: {e}")))?;
    Ok(JobServiceClient::new(channel))
}

async fn job_is_cancelled(client: &mut JobServiceClient<Channel>, job_id: &str) -> bool {
    let resp = client
        .get_job(GetJobRequest {
            job_id: Some(Id {
                value: job_id.to_string(),
            }),
        })
        .await;
    let job = match resp {
        Ok(resp) => resp.into_inner().job,
        Err(_) => return false,
    };
    matches!(
        job.and_then(|job| JobState::try_from(job.state).ok()),
        Some(JobState::Cancelled)
    )
}

async fn spawn_cancel_watcher(job_id: String) -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);
    let mut client = match connect_job().await {
        Ok(client) => client,
        Err(err) => {
            warn!("cancel watcher: failed to connect job service: {err}");
            return rx;
        }
    };
    let mut stream = match client
        .stream_job_events(StreamJobEventsRequest {
            job_id: Some(Id {
                value: job_id.clone(),
            }),
            include_history: true,
        })
        .await
    {
        Ok(resp) => resp.into_inner(),
        Err(err) => {
            warn!("cancel watcher: stream failed for {job_id}: {err}");
            return rx;
        }
    };

    tokio::spawn(async move {
        loop {
            match stream.message().await {
                Ok(Some(evt)) => {
                    if let Some(JobPayload::StateChanged(state)) = evt.payload {
                        if JobState::try_from(state.new_state).unwrap_or(JobState::Unspecified)
                            == JobState::Cancelled
                        {
                            let _ = tx.send(true);
                            break;
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
    });

    rx
}

fn cancel_requested(cancel_rx: &watch::Receiver<bool>) -> bool {
    *cancel_rx.borrow()
}

#[allow(clippy::too_many_arguments)]
async fn start_job(
    client: &mut JobServiceClient<Channel>,
    job_type: &str,
    params: Vec<KeyValue>,
    correlation_id: &str,
    project_id: Option<String>,
    target_id: Option<String>,
    toolchain_set_id: Option<String>,
    run_id: Option<RunId>,
) -> Result<String, Status> {
    let resp = client
        .start_job(StartJobRequest {
            job_type: job_type.into(),
            params,
            project_id: project_id.map(|value| Id { value }),
            target_id: target_id.map(|value| Id { value }),
            toolchain_set_id: toolchain_set_id.map(|value| Id { value }),
            correlation_id: correlation_id.to_string(),
            run_id,
        })
        .await
        .map_err(|e| Status::unavailable(format!("job start failed: {e}")))?
        .into_inner();

    let job_id = resp
        .job
        .and_then(|r| r.job_id)
        .map(|i| i.value)
        .unwrap_or_default();

    if job_id.is_empty() {
        return Err(Status::internal("job service returned empty job_id"));
    }
    Ok(job_id)
}

async fn publish_job_event(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    payload: JobPayload,
) -> Result<(), Status> {
    client
        .publish_job_event(PublishJobEventRequest {
            event: Some(JobEvent {
                at: Some(now_ts()),
                job_id: Some(Id {
                    value: job_id.to_string(),
                }),
                payload: Some(payload),
            }),
        })
        .await
        .map_err(|e| Status::unavailable(format!("publish job event failed: {e}")))?;
    Ok(())
}

async fn publish_state(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    state: JobState,
) -> Result<(), Status> {
    publish_job_event(
        client,
        job_id,
        JobPayload::StateChanged(JobStateChanged {
            new_state: state as i32,
        }),
    )
    .await
}

async fn publish_progress(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    percent: u32,
    phase: &str,
    metrics: Vec<KeyValue>,
) -> Result<(), Status> {
    publish_job_event(
        client,
        job_id,
        JobPayload::Progress(JobProgressUpdated {
            progress: Some(JobProgress {
                percent,
                phase: phase.into(),
                metrics,
            }),
        }),
    )
    .await
}

fn metric(key: &str, value: impl ToString) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: value.to_string(),
    }
}

async fn publish_log(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    message: &str,
) -> Result<(), Status> {
    publish_job_event(
        client,
        job_id,
        JobPayload::Log(JobLogAppended {
            chunk: Some(LogChunk {
                stream: "observe".into(),
                data: message.as_bytes().to_vec(),
                truncated: false,
            }),
        }),
    )
    .await
}

async fn publish_completed(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    summary: &str,
    outputs: Vec<KeyValue>,
) -> Result<(), Status> {
    publish_state(client, job_id, JobState::Success).await?;
    publish_job_event(
        client,
        job_id,
        JobPayload::Completed(JobCompleted {
            summary: summary.into(),
            outputs,
        }),
    )
    .await
}

async fn publish_failed(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    error: ErrorDetail,
) -> Result<(), Status> {
    publish_state(client, job_id, JobState::Failed).await?;
    publish_job_event(
        client,
        job_id,
        JobPayload::Failed(JobFailed { error: Some(error) }),
    )
    .await
}

fn error_detail(code: ErrorCode, message: &str, detail: &str, job_id: &str) -> ErrorDetail {
    ErrorDetail {
        code: code as i32,
        message: message.into(),
        technical_details: detail.into(),
        remedies: vec![],
        correlation_id: job_id.to_string(),
    }
}

fn error_detail_for_io(context: &str, err: &io::Error, job_id: &str) -> ErrorDetail {
    let code = match err.kind() {
        io::ErrorKind::NotFound => ErrorCode::NotFound,
        io::ErrorKind::AlreadyExists => ErrorCode::AlreadyExists,
        io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        io::ErrorKind::InvalidInput => ErrorCode::InvalidArgument,
        _ => ErrorCode::Internal,
    };
    error_detail(code, &format!("{context} failed"), &err.to_string(), job_id)
}

fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn write_zip_bundle(plan: BundlePlan) -> io::Result<()> {
    ensure_parent_dir(&plan.output_path)?;
    let file = fs::File::create(&plan.output_path)?;
    let mut zip = ZipWriter::new(file);
    let options = FileOptions::default().compression_method(CompressionMethod::Deflated);

    for item in plan.items {
        match item {
            BundleItem::File { source, name } => {
                if !source.is_file() {
                    continue;
                }
                zip.start_file(name, options)?;
                let mut input = fs::File::open(source)?;
                io::copy(&mut input, &mut zip)?;
            }
            BundleItem::Generated { name, contents } => {
                zip.start_file(name, options)?;
                zip.write_all(&contents)?;
            }
        }
    }

    zip.finish()?;
    Ok(())
}

fn gather_env() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (key, value) in std::env::vars() {
        if key.starts_with("AADK_") {
            map.insert(key, value);
        }
    }
    map
}

fn add_state_file(items: &mut Vec<BundleItem>, file_name: &str) {
    items.push(BundleItem::File {
        source: aadk_util::state_file_path(file_name),
        name: format!("state/{file_name}"),
    });
}

fn sanitize_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "_".into()
    } else {
        out
    }
}

fn load_job_history() -> Option<JobHistoryFile> {
    let path = job_state_file_path();
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                warn!("Failed to read {}: {}", path.display(), err);
            }
            return None;
        }
    };
    match serde_json::from_str::<JobHistoryFile>(&data) {
        Ok(file) => Some(file),
        Err(err) => {
            warn!("Failed to parse {}: {}", path.display(), err);
            None
        }
    }
}

fn log_readme(message: String) -> BundleItem {
    BundleItem::Generated {
        name: "logs/README.txt".into(),
        contents: message.into_bytes(),
    }
}

fn collect_log_items(run_id: &str, runs_snapshot: &[RunRecordEntry]) -> Vec<BundleItem> {
    let Some(run) = runs_snapshot.iter().find(|item| item.run_id == run_id) else {
        return vec![log_readme(format!(
            "No run record found for run_id {run_id}\n"
        ))];
    };
    if run.job_ids.is_empty() {
        return vec![log_readme(format!(
            "Run {run_id} has no recorded job ids\n"
        ))];
    }

    let file = match load_job_history() {
        Some(file) => file,
        None => {
            return vec![log_readme(format!(
                "Job history file {} not available\n",
                job_state_file_path().display()
            ))];
        }
    };

    let job_set: HashSet<String> = run.job_ids.iter().cloned().collect();
    let mut logs: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();

    for job in file.jobs {
        if !job_set.contains(&job.job_id) {
            continue;
        }
        for event in job.history {
            let Some(JobHistoryPayload::Log { chunk }) = event.payload else {
                continue;
            };
            let Some(chunk) = chunk else {
                continue;
            };
            let stream = if chunk.stream.trim().is_empty() {
                "log".to_string()
            } else {
                chunk.stream
            };
            logs.entry(job.job_id.clone())
                .or_default()
                .entry(stream)
                .or_default()
                .extend_from_slice(&chunk.data);
        }
    }

    if logs.is_empty() {
        return vec![log_readme(format!(
            "No job logs found in {} for run {run_id}\n",
            job_state_file_path().display()
        ))];
    }

    let mut items = Vec::new();
    for (job_id, streams) in logs {
        let job_dir = sanitize_segment(&job_id);
        for (stream, data) in streams {
            if data.is_empty() {
                continue;
            }
            let stream_name = sanitize_segment(&stream);
            items.push(BundleItem::Generated {
                name: format!("logs/{job_dir}/{stream_name}.log"),
                contents: data,
            });
        }
    }
    if items.is_empty() {
        items.push(log_readme(format!(
            "Job logs were empty for run {run_id}\n"
        )));
    }
    items
}

fn make_manifest(
    bundle_type: &str,
    run_id: Option<String>,
    correlation_id: Option<String>,
    req: &ExportSupportBundleRequest,
) -> BundleManifest {
    BundleManifest {
        bundle_type: bundle_type.into(),
        created_at: now_millis(),
        run_id,
        correlation_id,
        includes: BundleIncludes {
            include_logs: req.include_logs,
            include_config: req.include_config,
            include_toolchain_provenance: req.include_toolchain_provenance,
            include_recent_runs: req.include_recent_runs,
            recent_runs_limit: req.recent_runs_limit,
        },
    }
}

#[allow(clippy::result_large_err)]
fn support_bundle_plan(
    output_path: PathBuf,
    run_id: &str,
    req: &ExportSupportBundleRequest,
    runs_snapshot: Vec<RunRecordEntry>,
) -> Result<BundlePlan, Status> {
    let mut items = Vec::new();
    let correlation_id = req.correlation_id.trim().to_string();
    let correlation_id = if correlation_id.is_empty() {
        None
    } else {
        Some(correlation_id)
    };
    let manifest = make_manifest("support", Some(run_id.to_string()), correlation_id, req);
    let manifest_json = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| Status::internal(format!("manifest serialization failed: {e}")))?;
    items.push(BundleItem::Generated {
        name: "manifest.json".into(),
        contents: manifest_json,
    });

    if req.include_config {
        let env = gather_env();
        let env_json = serde_json::to_vec_pretty(&env)
            .map_err(|e| Status::internal(format!("env serialization failed: {e}")))?;
        items.push(BundleItem::Generated {
            name: "config/env.json".into(),
            contents: env_json,
        });
        add_state_file(&mut items, "projects.json");
        add_state_file(&mut items, "builds.json");
        add_state_file(&mut items, "targets.json");
    }

    if req.include_recent_runs {
        let limit = if req.recent_runs_limit == 0 {
            runs_snapshot.len()
        } else {
            req.recent_runs_limit as usize
        };
        let slice = runs_snapshot
            .iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let runs_json = serde_json::to_vec_pretty(&slice)
            .map_err(|e| Status::internal(format!("runs serialization failed: {e}")))?;
        items.push(BundleItem::Generated {
            name: "runs.json".into(),
            contents: runs_json,
        });
    }

    if req.include_toolchain_provenance {
        let toolchain_path = data_dir().join("state").join("toolchains.json");
        items.push(BundleItem::File {
            source: toolchain_path,
            name: "toolchains.json".into(),
        });
    }

    if req.include_logs {
        items.extend(collect_log_items(run_id, &runs_snapshot));
    }

    Ok(BundlePlan { output_path, items })
}

#[allow(clippy::result_large_err)]
fn evidence_bundle_plan(output_path: PathBuf, run: RunRecordEntry) -> Result<BundlePlan, Status> {
    let includes = BundleIncludes {
        include_logs: false,
        include_config: false,
        include_toolchain_provenance: false,
        include_recent_runs: false,
        recent_runs_limit: 0,
    };
    let manifest = BundleManifest {
        bundle_type: "evidence".into(),
        created_at: now_millis(),
        run_id: Some(run.run_id.clone()),
        correlation_id: run.correlation_id.clone(),
        includes,
    };
    let manifest_json = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| Status::internal(format!("manifest serialization failed: {e}")))?;
    let run_json = serde_json::to_vec_pretty(&run)
        .map_err(|e| Status::internal(format!("run serialization failed: {e}")))?;

    Ok(BundlePlan {
        output_path,
        items: vec![
            BundleItem::Generated {
                name: "manifest.json".into(),
                contents: manifest_json,
            },
            BundleItem::Generated {
                name: "run.json".into(),
                contents: run_json,
            },
        ],
    })
}

async fn update_run_state(
    state: Arc<Mutex<State>>,
    run_id: &str,
    update: impl FnOnce(&mut RunRecordEntry),
) -> io::Result<()> {
    let mut st = state.lock().await;
    if let Some(entry) = st.runs.iter_mut().find(|item| item.run_id == run_id) {
        update(entry);
    }
    save_state(&st)
}

async fn record_output(
    state: Arc<Mutex<State>>,
    output: RunOutputEntry,
) -> io::Result<RunOutputSummaryEntry> {
    let mut st = state.lock().await;
    let summary = upsert_output_entry(&mut st, output);
    save_state(&st)?;
    Ok(summary)
}

fn support_bundle_metrics(
    run_id: &str,
    output_path: &Path,
    req: &ExportSupportBundleRequest,
) -> Vec<KeyValue> {
    vec![
        metric("run_id", run_id),
        metric("output_path", output_path.display()),
        metric("include_logs", req.include_logs),
        metric("include_config", req.include_config),
        metric(
            "include_toolchain_provenance",
            req.include_toolchain_provenance,
        ),
        metric("include_recent_runs", req.include_recent_runs),
        metric("recent_runs_limit", req.recent_runs_limit),
    ]
}

fn evidence_bundle_metrics(run_id: &str, output_path: &Path) -> Vec<KeyValue> {
    vec![
        metric("run_id", run_id),
        metric("output_path", output_path.display()),
    ]
}

async fn run_support_bundle_job(
    mut job_client: JobServiceClient<Channel>,
    state: Arc<Mutex<State>>,
    run_id: String,
    job_id: String,
    output_path: PathBuf,
    req: ExportSupportBundleRequest,
) -> Result<(), Status> {
    let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;
    if job_is_cancelled(&mut job_client, &job_id).await {
        let _ = publish_log(
            &mut job_client,
            &job_id,
            "Support bundle cancelled before start\n",
        )
        .await;
        let _ = update_run_state(state, &run_id, |entry| {
            entry.result = "cancelled".into();
            entry.finished_at = Some(now_millis());
        })
        .await;
        return Ok(());
    }

    publish_state(&mut job_client, &job_id, JobState::Running).await?;
    publish_log(
        &mut job_client,
        &job_id,
        &format!("Creating support bundle for run {run_id}\n"),
    )
    .await?;

    let base_metrics = support_bundle_metrics(&run_id, &output_path, &req);
    publish_progress(
        &mut job_client,
        &job_id,
        10,
        "collecting metadata",
        base_metrics.clone(),
    )
    .await?;

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Support bundle cancelled\n").await;
        let _ = update_run_state(state, &run_id, |entry| {
            entry.result = "cancelled".into();
            entry.finished_at = Some(now_millis());
        })
        .await;
        return Ok(());
    }

    let runs_snapshot = {
        let st = state.lock().await;
        st.runs.clone()
    };

    let plan = match support_bundle_plan(output_path.clone(), &run_id, &req, runs_snapshot) {
        Ok(plan) => plan,
        Err(err) => {
            let detail = error_detail(
                ErrorCode::Internal,
                "bundle planning failed",
                err.message(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            let _ = update_run_state(state, &run_id, |entry| {
                entry.result = "failed".into();
                entry.finished_at = Some(now_millis());
            })
            .await;
            return Err(err);
        }
    };

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Support bundle cancelled\n").await;
        let _ = update_run_state(state, &run_id, |entry| {
            entry.result = "cancelled".into();
            entry.finished_at = Some(now_millis());
        })
        .await;
        return Ok(());
    }

    let mut write_metrics = base_metrics.clone();
    write_metrics.push(metric("item_count", plan.items.len()));
    publish_progress(
        &mut job_client,
        &job_id,
        60,
        "writing bundle",
        write_metrics,
    )
    .await?;

    match write_zip_bundle(plan) {
        Ok(_) => {}
        Err(err) => {
            let detail = error_detail_for_io("write bundle", &err, &job_id);
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            let _ = update_run_state(state, &run_id, |entry| {
                entry.result = "failed".into();
                entry.finished_at = Some(now_millis());
            })
            .await;
            return Err(Status::internal(format!("bundle write failed: {err}")));
        }
    }

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Support bundle cancelled\n").await;
        let _ = update_run_state(state, &run_id, |entry| {
            entry.result = "cancelled".into();
            entry.finished_at = Some(now_millis());
        })
        .await;
        return Ok(());
    }

    let _ = update_run_state(state.clone(), &run_id, |entry| {
        entry.result = "success".into();
        entry.finished_at = Some(now_millis());
    })
    .await;

    let _ = record_output(
        state,
        RunOutputEntry {
            output_id: format!("bundle:{job_id}"),
            run_id: run_id.clone(),
            kind: RunOutputKindEntry::Bundle,
            output_type: "support_bundle".into(),
            path: output_path.to_string_lossy().to_string(),
            label: "Support bundle".into(),
            job_id: Some(job_id.clone()),
            created_at: now_millis(),
            metadata: vec![
                SummaryEntry {
                    key: "include_logs".into(),
                    value: req.include_logs.to_string(),
                },
                SummaryEntry {
                    key: "include_config".into(),
                    value: req.include_config.to_string(),
                },
                SummaryEntry {
                    key: "include_toolchain_provenance".into(),
                    value: req.include_toolchain_provenance.to_string(),
                },
                SummaryEntry {
                    key: "include_recent_runs".into(),
                    value: req.include_recent_runs.to_string(),
                },
                SummaryEntry {
                    key: "recent_runs_limit".into(),
                    value: req.recent_runs_limit.to_string(),
                },
            ],
        },
    )
    .await;

    publish_completed(
        &mut job_client,
        &job_id,
        "Support bundle created",
        vec![
            KeyValue {
                key: "run_id".into(),
                value: run_id.clone(),
            },
            KeyValue {
                key: "output_path".into(),
                value: output_path.to_string_lossy().to_string(),
            },
        ],
    )
    .await?;

    cleanup_bundles_best_effort();
    Ok(())
}

async fn run_evidence_bundle_job(
    mut job_client: JobServiceClient<Channel>,
    state: Arc<Mutex<State>>,
    run_id: String,
    job_id: String,
    output_path: PathBuf,
) -> Result<(), Status> {
    let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;
    if job_is_cancelled(&mut job_client, &job_id).await {
        let _ = publish_log(
            &mut job_client,
            &job_id,
            "Evidence bundle cancelled before start\n",
        )
        .await;
        return Ok(());
    }

    publish_state(&mut job_client, &job_id, JobState::Running).await?;
    publish_log(
        &mut job_client,
        &job_id,
        &format!("Creating evidence bundle for run {run_id}\n"),
    )
    .await?;

    let base_metrics = evidence_bundle_metrics(&run_id, &output_path);
    publish_progress(
        &mut job_client,
        &job_id,
        25,
        "loading run",
        base_metrics.clone(),
    )
    .await?;

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Evidence bundle cancelled\n").await;
        return Ok(());
    }

    let run_snapshot = {
        let st = state.lock().await;
        st.runs.iter().find(|item| item.run_id == run_id).cloned()
    };
    let Some(run_snapshot) = run_snapshot else {
        let detail = error_detail(
            ErrorCode::NotFound,
            "run not found",
            "run_id missing from observe state",
            &job_id,
        );
        let _ = publish_failed(&mut job_client, &job_id, detail).await;
        return Err(Status::not_found("run not found"));
    };

    let plan = match evidence_bundle_plan(output_path.clone(), run_snapshot) {
        Ok(plan) => plan,
        Err(err) => {
            let detail = error_detail(
                ErrorCode::Internal,
                "bundle planning failed",
                err.message(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return Err(err);
        }
    };

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Evidence bundle cancelled\n").await;
        return Ok(());
    }

    let mut write_metrics = base_metrics.clone();
    write_metrics.push(metric("item_count", plan.items.len()));
    publish_progress(
        &mut job_client,
        &job_id,
        70,
        "writing bundle",
        write_metrics,
    )
    .await?;

    match write_zip_bundle(plan) {
        Ok(_) => {}
        Err(err) => {
            let detail = error_detail_for_io("write bundle", &err, &job_id);
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return Err(Status::internal(format!("bundle write failed: {err}")));
        }
    }

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Evidence bundle cancelled\n").await;
        return Ok(());
    }

    let _ = record_output(
        state,
        RunOutputEntry {
            output_id: format!("bundle:{job_id}"),
            run_id: run_id.clone(),
            kind: RunOutputKindEntry::Bundle,
            output_type: "evidence_bundle".into(),
            path: output_path.to_string_lossy().to_string(),
            label: "Evidence bundle".into(),
            job_id: Some(job_id.clone()),
            created_at: now_millis(),
            metadata: Vec::new(),
        },
    )
    .await;

    publish_completed(
        &mut job_client,
        &job_id,
        "Evidence bundle created",
        vec![
            KeyValue {
                key: "run_id".into(),
                value: run_id.clone(),
            },
            KeyValue {
                key: "output_path".into(),
                value: output_path.to_string_lossy().to_string(),
            },
        ],
    )
    .await?;

    cleanup_bundles_best_effort();
    Ok(())
}

#[tonic::async_trait]
impl ObserveService for Svc {
    async fn list_runs(
        &self,
        request: Request<ListRunsRequest>,
    ) -> Result<Response<ListRunsResponse>, Status> {
        let req = request.into_inner();
        let page = req.page.unwrap_or(Pagination {
            page_size: DEFAULT_PAGE_SIZE as u32,
            page_token: String::new(),
        });
        let page_size = if page.page_size == 0 {
            DEFAULT_PAGE_SIZE
        } else {
            page.page_size as usize
        };
        let start = if page.page_token.trim().is_empty() {
            0
        } else {
            page.page_token
                .parse::<usize>()
                .map_err(|_| Status::invalid_argument("invalid page_token"))?
        };

        let st = self.state.lock().await;
        let filter = req.filter.unwrap_or_default();
        let runs = st
            .runs
            .iter()
            .filter(|run| run_matches_filter(run, &filter))
            .cloned()
            .collect::<Vec<_>>();
        let total = runs.len();
        if start >= total && total != 0 {
            return Err(Status::invalid_argument("page_token out of range"));
        }

        let end = (start + page_size).min(total);
        let items = runs[start..end]
            .iter()
            .map(|run| run.to_proto())
            .collect::<Vec<_>>();
        let next_token = if end < total {
            end.to_string()
        } else {
            String::new()
        };

        Ok(Response::new(ListRunsResponse {
            runs: items,
            page_info: Some(PageInfo {
                next_page_token: next_token,
            }),
        }))
    }

    async fn list_run_outputs(
        &self,
        request: Request<ListRunOutputsRequest>,
    ) -> Result<Response<ListRunOutputsResponse>, Status> {
        let req = request.into_inner();
        let run_id = req
            .run_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Status::invalid_argument("run_id is required"))?;

        let page = req.page.unwrap_or(Pagination {
            page_size: DEFAULT_PAGE_SIZE as u32,
            page_token: String::new(),
        });
        let page_size = if page.page_size == 0 {
            DEFAULT_PAGE_SIZE
        } else {
            page.page_size as usize
        };
        let start = if page.page_token.trim().is_empty() {
            0
        } else {
            page.page_token
                .parse::<usize>()
                .map_err(|_| Status::invalid_argument("invalid page_token"))?
        };

        let st = self.state.lock().await;
        let filter = req.filter.unwrap_or_default();
        let outputs = st
            .outputs
            .iter()
            .filter(|entry| entry.run_id == run_id && output_matches_filter(entry, &filter))
            .cloned()
            .collect::<Vec<_>>();
        let total = outputs.len();
        if start >= total && total != 0 {
            return Err(Status::invalid_argument("page_token out of range"));
        }

        let end = (start + page_size).min(total);
        let items = outputs[start..end]
            .iter()
            .map(|entry| entry.to_proto())
            .collect::<Vec<_>>();
        let next_token = if end < total {
            end.to_string()
        } else {
            String::new()
        };
        let summary = st
            .runs
            .iter()
            .find(|run| run.run_id == run_id)
            .map(|run| run.output_summary.clone())
            .unwrap_or_else(|| compute_output_summary(&run_id, &st.outputs));

        Ok(Response::new(ListRunOutputsResponse {
            outputs: items,
            page_info: Some(PageInfo {
                next_page_token: next_token,
            }),
            summary: Some(summary.to_proto()),
        }))
    }

    async fn export_support_bundle(
        &self,
        request: Request<ExportSupportBundleRequest>,
    ) -> Result<Response<ExportSupportBundleResponse>, Status> {
        let req = request.into_inner();
        let project_id = normalize_id(req.project_id.as_ref());
        let target_id = normalize_id(req.target_id.as_ref());
        let toolchain_set_id = normalize_id(req.toolchain_set_id.as_ref());
        let run_id = req
            .run_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("run-{}", Uuid::new_v4()));
        let output_path = bundles_dir().join(format!("support-{run_id}.zip"));
        let output_path_resp = output_path.to_string_lossy().to_string();

        let mut job_client = connect_job().await?;
        let job_id = req
            .job_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let correlation_id = req.correlation_id.trim();
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "observe.support_bundle",
                vec![
                    KeyValue {
                        key: "include_logs".into(),
                        value: req.include_logs.to_string(),
                    },
                    KeyValue {
                        key: "include_config".into(),
                        value: req.include_config.to_string(),
                    },
                    KeyValue {
                        key: "include_toolchain_provenance".into(),
                        value: req.include_toolchain_provenance.to_string(),
                    },
                    KeyValue {
                        key: "include_recent_runs".into(),
                        value: req.include_recent_runs.to_string(),
                    },
                    KeyValue {
                        key: "recent_runs_limit".into(),
                        value: req.recent_runs_limit.to_string(),
                    },
                ],
                correlation_id,
                project_id.clone(),
                target_id.clone(),
                toolchain_set_id.clone(),
                Some(RunId {
                    value: run_id.clone(),
                }),
            )
            .await?
        } else {
            job_id
        };

        {
            let mut st = self.state.lock().await;
            let mut run = RunRecordEntry::new(run_id.clone(), "running");
            if !correlation_id.is_empty() {
                run.correlation_id = Some(correlation_id.to_string());
            }
            run.project_id = project_id.clone();
            run.target_id = target_id.clone();
            run.toolchain_set_id = toolchain_set_id.clone();
            run.job_ids.push(job_id.clone());
            upsert_run(&mut st, run);
            if let Err(err) = save_state(&st) {
                drop(st);
                let detail = error_detail_for_io("persist observe state", &err, &job_id);
                let _ = publish_failed(&mut job_client, &job_id, detail).await;
                return Err(Status::internal(format!("failed to persist state: {err}")));
            }
        }

        let state = self.state.clone();
        let run_id_for_job = run_id.clone();
        let job_id_for_job = job_id.clone();
        let output_path_job = output_path.clone();
        tokio::spawn(async move {
            if let Err(err) = run_support_bundle_job(
                job_client,
                state,
                run_id_for_job,
                job_id_for_job,
                output_path_job,
                req,
            )
            .await
            {
                eprintln!("support bundle job failed: {err}");
            }
        });

        Ok(Response::new(ExportSupportBundleResponse {
            job_id: Some(Id { value: job_id }),
            output_path: output_path_resp,
        }))
    }

    async fn export_evidence_bundle(
        &self,
        request: Request<ExportEvidenceBundleRequest>,
    ) -> Result<Response<ExportEvidenceBundleResponse>, Status> {
        let req = request.into_inner();
        let correlation_id = req.correlation_id.trim().to_string();
        let run_id = req
            .run_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty());

        let (run_id, run_meta) = {
            let st = self.state.lock().await;
            let entry = if let Some(ref run_id) = run_id {
                st.runs.iter().find(|item| item.run_id == *run_id)
            } else if !correlation_id.is_empty() {
                st.runs
                    .iter()
                    .find(|item| item.correlation_id.as_deref() == Some(correlation_id.as_str()))
            } else {
                None
            };
            let Some(entry) = entry else {
                return Err(Status::not_found("run not found"));
            };
            let run_id = entry.run_id.clone();
            (
                run_id,
                (
                    entry.project_id.clone(),
                    entry.target_id.clone(),
                    entry.toolchain_set_id.clone(),
                ),
            )
        };

        let mut job_client = connect_job().await?;
        let job_id = req
            .job_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "observe.evidence_bundle",
                vec![KeyValue {
                    key: "run_id".into(),
                    value: run_id.clone(),
                }],
                correlation_id.as_str(),
                run_meta.0.clone(),
                run_meta.1.clone(),
                run_meta.2.clone(),
                Some(RunId {
                    value: run_id.clone(),
                }),
            )
            .await?
        } else {
            job_id
        };
        let output_path = bundles_dir().join(format!("evidence-{job_id}.zip"));
        let output_path_resp = output_path.to_string_lossy().to_string();

        {
            let mut st = self.state.lock().await;
            if let Some(entry) = st.runs.iter_mut().find(|item| item.run_id == run_id) {
                entry.job_ids.push(job_id.clone());
                if !correlation_id.is_empty() && entry.correlation_id.is_none() {
                    entry.correlation_id = Some(correlation_id.clone());
                }
                if let Err(err) = save_state(&st) {
                    drop(st);
                    let detail = error_detail_for_io("persist observe state", &err, &job_id);
                    let _ = publish_failed(&mut job_client, &job_id, detail).await;
                    return Err(Status::internal(format!("failed to persist state: {err}")));
                }
            }
        }

        let state = self.state.clone();
        let run_id_for_job = run_id.clone();
        let job_id_for_job = job_id.clone();
        let output_path_job = output_path.clone();
        tokio::spawn(async move {
            if let Err(err) = run_evidence_bundle_job(
                job_client,
                state,
                run_id_for_job,
                job_id_for_job,
                output_path_job,
            )
            .await
            {
                eprintln!("evidence bundle job failed: {err}");
            }
        });

        Ok(Response::new(ExportEvidenceBundleResponse {
            job_id: Some(Id { value: job_id }),
            output_path: output_path_resp,
        }))
    }

    async fn upsert_run(
        &self,
        request: Request<UpsertRunRequest>,
    ) -> Result<Response<UpsertRunResponse>, Status> {
        let req = request.into_inner();
        let run_id = req
            .run_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Status::invalid_argument("run_id is required"))?;

        let mut st = self.state.lock().await;
        let mut run = st
            .runs
            .iter()
            .find(|entry| entry.run_id == run_id)
            .cloned()
            .unwrap_or_else(|| {
                if req.result.trim().is_empty() {
                    RunRecordEntry::new(run_id.clone(), "running")
                } else {
                    RunRecordEntry::new(run_id.clone(), &req.result)
                }
            });

        if !req.correlation_id.trim().is_empty() {
            run.correlation_id = Some(req.correlation_id.trim().to_string());
        }
        if let Some(project_id) = normalize_id(req.project_id.as_ref()) {
            run.project_id = Some(project_id);
        }
        if let Some(target_id) = normalize_id(req.target_id.as_ref()) {
            run.target_id = Some(target_id);
        }
        if let Some(toolchain_set_id) = normalize_id(req.toolchain_set_id.as_ref()) {
            run.toolchain_set_id = Some(toolchain_set_id);
        }
        if let Some(started_at) = req.started_at.as_ref() {
            run.started_at = started_at.unix_millis;
        }
        if let Some(finished_at) = req.finished_at.as_ref() {
            run.finished_at = Some(finished_at.unix_millis);
        }
        if !req.result.trim().is_empty() {
            run.result = req.result.trim().to_string();
        }

        for job_id in req.job_ids {
            let value = job_id.value.trim();
            if value.is_empty() {
                continue;
            }
            if !run.job_ids.iter().any(|existing| existing == value) {
                run.job_ids.push(value.to_string());
            }
        }
        merge_summary(&mut run, req.summary);
        run.output_summary = compute_output_summary(&run_id, &st.outputs);

        upsert_run(&mut st, run.clone());
        if let Err(err) = save_state(&st) {
            return Err(Status::internal(format!("failed to persist state: {err}")));
        }

        Ok(Response::new(UpsertRunResponse {
            run: Some(run.to_proto()),
        }))
    }

    async fn upsert_run_outputs(
        &self,
        request: Request<UpsertRunOutputsRequest>,
    ) -> Result<Response<UpsertRunOutputsResponse>, Status> {
        let req = request.into_inner();
        let run_id = req
            .run_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Status::invalid_argument("run_id is required"))?;

        let mut entries = Vec::new();
        for output in req.outputs {
            if let Some(output_run_id) = output.run_id.as_ref().map(|id| id.value.trim()) {
                if !output_run_id.is_empty() && output_run_id != run_id {
                    return Err(Status::invalid_argument("output run_id mismatch"));
                }
            }
            let output_id = output.output_id.trim();
            let path = output.path.trim().to_string();
            if path.is_empty() {
                return Err(Status::invalid_argument("output path is required"));
            }
            let output_type = output.output_type.trim().to_string();
            let output_id = if output_id.is_empty() {
                format!("output:{run_id}:{output_type}:{path}")
            } else {
                output_id.to_string()
            };
            let label = output.label.trim().to_string();
            let job_id = output
                .job_id
                .as_ref()
                .map(|id| id.value.trim().to_string())
                .filter(|value| !value.is_empty());
            let created_at = output
                .created_at
                .as_ref()
                .map(|ts| ts.unix_millis)
                .unwrap_or_else(now_millis);
            let metadata = output
                .metadata
                .into_iter()
                .filter(|item| !item.key.trim().is_empty())
                .map(|item| SummaryEntry {
                    key: item.key,
                    value: item.value,
                })
                .collect::<Vec<_>>();

            entries.push(RunOutputEntry {
                output_id,
                run_id: run_id.clone(),
                kind: run_output_kind_entry(output.kind),
                output_type,
                path,
                label,
                job_id,
                created_at,
                metadata,
            });
        }

        let mut st = self.state.lock().await;
        for entry in entries {
            st.outputs.retain(|item| item.output_id != entry.output_id);
            st.outputs.insert(0, entry);
        }
        let summary = refresh_run_output_summary(&mut st, &run_id);
        if let Err(err) = save_state(&st) {
            return Err(Status::internal(format!("failed to persist state: {err}")));
        }

        Ok(Response::new(UpsertRunOutputsResponse {
            summary: Some(summary.to_proto()),
        }))
    }

    async fn reload_state(
        &self,
        _request: Request<ReloadStateRequest>,
    ) -> Result<Response<ReloadStateResponse>, Status> {
        let state = load_state();
        let runs = state.runs.len();
        let outputs = state.outputs.len();
        let mut st = self.state.lock().await;
        *st = state;
        Ok(Response::new(ReloadStateResponse {
            ok: true,
            item_count: runs.saturating_add(outputs) as u32,
            detail: format!("runs={runs} outputs={outputs}"),
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    serve_grpc_with_telemetry(
        "aadk-observe",
        env!("CARGO_PKG_VERSION"),
        "observe",
        "AADK_OBSERVE_ADDR",
        aadk_util::DEFAULT_OBSERVE_ADDR,
        |server| server.add_service(ObserveServiceServer::new(Svc::new())),
    )
    .await
}
