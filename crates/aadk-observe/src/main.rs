use std::{
    collections::BTreeMap,
    fs,
    io,
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use aadk_proto::aadk::v1::{
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    observe_service_server::{ObserveService, ObserveServiceServer},
    ExportEvidenceBundleRequest, ExportEvidenceBundleResponse, ExportSupportBundleRequest,
    ExportSupportBundleResponse, Id, JobCompleted, JobEvent, JobFailed, JobLogAppended,
    JobProgress, JobProgressUpdated, JobState, JobStateChanged, KeyValue, ListRunsRequest,
    ListRunsResponse, LogChunk, PageInfo, Pagination, PublishJobEventRequest, RunRecord,
    StartJobRequest, Timestamp, ErrorCode, ErrorDetail,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tonic::{transport::Channel, Request, Response, Status};
use tracing::{info, warn};
use uuid::Uuid;
use zip::{write::FileOptions, CompressionMethod, ZipWriter};

const STATE_FILE_NAME: &str = "observe.json";
const DEFAULT_PAGE_SIZE: usize = 25;
const MAX_RUNS: usize = 200;

#[derive(Default)]
struct State {
    runs: Vec<RunRecordEntry>,
}

#[derive(Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    runs: Vec<RunRecordEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RunRecordEntry {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    run_id: String,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SummaryEntry {
    key: String,
    value: String,
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

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn now_ts() -> Timestamp {
    Timestamp {
        unix_millis: now_millis(),
    }
}

fn data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/aadk")
    } else {
        PathBuf::from("/tmp/aadk")
    }
}

fn state_file_path() -> PathBuf {
    data_dir().join("state").join(STATE_FILE_NAME)
}

fn bundles_dir() -> PathBuf {
    data_dir().join("bundles")
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn load_state() -> State {
    let path = state_file_path();
    match fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str::<StateFile>(&data) {
            Ok(file) => State { runs: file.runs },
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

impl RunRecordEntry {
    fn new(run_id: String, result: &str) -> Self {
        let now = now_millis();
        Self {
            schema_version: default_schema_version(),
            run_id,
            project_id: None,
            target_id: None,
            toolchain_set_id: None,
            started_at: now,
            finished_at: None,
            result: result.into(),
            job_ids: Vec::new(),
            summary: Vec::new(),
        }
    }

    fn to_proto(&self) -> RunRecord {
        RunRecord {
            run_id: Some(Id {
                value: self.run_id.clone(),
            }),
            project_id: self
                .project_id
                .as_ref()
                .map(|value| Id { value: value.clone() }),
            target_id: self
                .target_id
                .as_ref()
                .map(|value| Id { value: value.clone() }),
            toolchain_set_id: self
                .toolchain_set_id
                .as_ref()
                .map(|value| Id { value: value.clone() }),
            started_at: Some(Timestamp {
                unix_millis: self.started_at,
            }),
            finished_at: self.finished_at.map(|ts| Timestamp { unix_millis: ts }),
            result: self.result.clone(),
            job_ids: self
                .job_ids
                .iter()
                .map(|value| Id { value: value.clone() })
                .collect(),
            summary: self
                .summary
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
        Self {
            state: Arc::new(Mutex::new(load_state())),
        }
    }
}

fn job_addr() -> String {
    std::env::var("AADK_JOB_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".into())
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

async fn start_job(
    client: &mut JobServiceClient<Channel>,
    job_type: &str,
    params: Vec<KeyValue>,
) -> Result<String, Status> {
    let resp = client
        .start_job(StartJobRequest {
            job_type: job_type.into(),
            params,
            project_id: None,
            target_id: None,
            toolchain_set_id: None,
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

fn make_manifest(
    bundle_type: &str,
    run_id: Option<String>,
    req: &ExportSupportBundleRequest,
) -> BundleManifest {
    BundleManifest {
        bundle_type: bundle_type.into(),
        created_at: now_millis(),
        run_id,
        includes: BundleIncludes {
            include_logs: req.include_logs,
            include_config: req.include_config,
            include_toolchain_provenance: req.include_toolchain_provenance,
            include_recent_runs: req.include_recent_runs,
            recent_runs_limit: req.recent_runs_limit,
        },
    }
}

fn support_bundle_plan(
    output_path: PathBuf,
    run_id: &str,
    req: &ExportSupportBundleRequest,
    runs_snapshot: Vec<RunRecordEntry>,
) -> Result<BundlePlan, Status> {
    let mut items = Vec::new();
    let manifest = make_manifest("support", Some(run_id.to_string()), req);
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
            name: "env.json".into(),
            contents: env_json,
        });
    }

    if req.include_recent_runs {
        let limit = if req.recent_runs_limit == 0 {
            runs_snapshot.len()
        } else {
            req.recent_runs_limit as usize
        };
        let slice = runs_snapshot.into_iter().take(limit).collect::<Vec<_>>();
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
        items.push(BundleItem::Generated {
            name: "logs_placeholder.txt".into(),
            contents: b"No logs collected yet.\n".to_vec(),
        });
    }

    Ok(BundlePlan { output_path, items })
}

fn evidence_bundle_plan(
    output_path: PathBuf,
    run: RunRecordEntry,
) -> Result<BundlePlan, Status> {
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

async fn run_support_bundle_job(
    mut job_client: JobServiceClient<Channel>,
    state: Arc<Mutex<State>>,
    run_id: String,
    job_id: String,
    output_path: PathBuf,
    req: ExportSupportBundleRequest,
) -> Result<(), Status> {
    publish_state(&mut job_client, &job_id, JobState::Running).await?;
    publish_log(
        &mut job_client,
        &job_id,
        &format!("Creating support bundle for run {run_id}\n"),
    )
    .await?;

    publish_progress(
        &mut job_client,
        &job_id,
        10,
        "collecting metadata",
        vec![],
    )
    .await?;

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

    publish_progress(&mut job_client, &job_id, 60, "writing bundle", vec![]).await?;

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

    let _ = update_run_state(state, &run_id, |entry| {
        entry.result = "success".into();
        entry.finished_at = Some(now_millis());
        entry.summary.push(SummaryEntry {
            key: "bundle_path".into(),
            value: output_path.to_string_lossy().to_string(),
        });
    })
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

    Ok(())
}

async fn run_evidence_bundle_job(
    mut job_client: JobServiceClient<Channel>,
    state: Arc<Mutex<State>>,
    run_id: String,
    job_id: String,
    output_path: PathBuf,
) -> Result<(), Status> {
    publish_state(&mut job_client, &job_id, JobState::Running).await?;
    publish_log(
        &mut job_client,
        &job_id,
        &format!("Creating evidence bundle for run {run_id}\n"),
    )
    .await?;

    publish_progress(&mut job_client, &job_id, 25, "loading run", vec![]).await?;

    let run_snapshot = {
        let st = state.lock().await;
        st.runs
            .iter()
            .find(|item| item.run_id == run_id)
            .cloned()
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

    publish_progress(&mut job_client, &job_id, 70, "writing bundle", vec![]).await?;

    match write_zip_bundle(plan) {
        Ok(_) => {}
        Err(err) => {
            let detail = error_detail_for_io("write bundle", &err, &job_id);
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return Err(Status::internal(format!("bundle write failed: {err}")));
        }
    }

    let _ = update_run_state(state, &run_id, |entry| {
        entry.summary.push(SummaryEntry {
            key: "evidence_bundle_path".into(),
            value: output_path.to_string_lossy().to_string(),
        });
    })
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
        let total = st.runs.len();
        if start >= total && total != 0 {
            return Err(Status::invalid_argument("page_token out of range"));
        }

        let end = (start + page_size).min(total);
        let items = st.runs[start..end]
            .iter()
            .cloned()
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

    async fn export_support_bundle(
        &self,
        request: Request<ExportSupportBundleRequest>,
    ) -> Result<Response<ExportSupportBundleResponse>, Status> {
        let req = request.into_inner();
        let run_id = format!("run-{}", Uuid::new_v4());
        let output_path = bundles_dir().join(format!("support-{run_id}.zip"));
        let output_path_resp = output_path.to_string_lossy().to_string();

        let mut job_client = connect_job().await?;
        let job_id = start_job(
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
        )
        .await?;

        {
            let mut st = self.state.lock().await;
            let mut run = RunRecordEntry::new(run_id.clone(), "running");
            run.job_ids.push(job_id.clone());
            run.summary.push(SummaryEntry {
                key: "bundle_type".into(),
                value: "support".into(),
            });
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
        let run_id = req
            .run_id
            .map(|id| id.value)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| Status::invalid_argument("run_id is required"))?;

        {
            let st = self.state.lock().await;
            if !st.runs.iter().any(|item| item.run_id == run_id) {
                return Err(Status::not_found("run_id not found"));
            }
        }

        let mut job_client = connect_job().await?;
        let job_id = start_job(
            &mut job_client,
            "observe.evidence_bundle",
            vec![KeyValue {
                key: "run_id".into(),
                value: run_id.clone(),
            }],
        )
        .await?;
        let output_path = bundles_dir().join(format!("evidence-{job_id}.zip"));
        let output_path_resp = output_path.to_string_lossy().to_string();

        {
            let mut st = self.state.lock().await;
            if let Some(entry) = st.runs.iter_mut().find(|item| item.run_id == run_id) {
                entry.job_ids.push(job_id.clone());
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();

    let addr_str =
        std::env::var("AADK_OBSERVE_ADDR").unwrap_or_else(|_| "127.0.0.1:50056".to_string());
    let addr: SocketAddr = addr_str.parse()?;

    let svc = Svc::new();
    info!("aadk-observe listening on {}", addr);

    tonic::transport::Server::builder()
        .add_service(ObserveServiceServer::new(svc))
        .serve(addr)
        .await?;

    Ok(())
}
