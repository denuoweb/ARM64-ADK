use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Instant,
};
use std::io::Read;

use aadk_proto::aadk::v1::{
    build_service_server::{BuildService, BuildServiceServer},
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    project_service_client::ProjectServiceClient,
    Artifact, ArtifactFilter, ArtifactType, BuildRequest, BuildResponse, BuildVariant, ErrorCode,
    ErrorDetail, Id, JobCompleted, JobEvent, JobFailed, JobLogAppended, JobProgress,
    JobProgressUpdated, JobState, JobStateChanged, KeyValue, ListArtifactsRequest,
    ListArtifactsResponse, LogChunk, PublishJobEventRequest, StartJobRequest,
    StreamJobEventsRequest, GetJobRequest, GetProjectRequest,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::{mpsc, watch, Mutex},
};
use tonic::{transport::Channel, Request, Response, Status};
use tracing::{info, warn};

const LOG_CHANNEL_CAPACITY: usize = 1024;
const RECENT_LOG_LIMIT: usize = 200;
const STATE_FILE_NAME: &str = "builds.json";
const MAX_BUILD_RECORDS: usize = 200;

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct BuildState {
    records: Vec<BuildRecord>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct BuildRecord {
    job_id: String,
    project_id: String,
    module: String,
    variant: i32,
    variant_name: String,
    tasks: Vec<String>,
    created_at_unix_millis: i64,
    project_path: String,
    artifacts: Vec<ArtifactRecord>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct ArtifactRecord {
    name: String,
    path: String,
    size_bytes: u64,
    sha256: String,
    artifact_type: i32,
    metadata: Vec<KeyValueRecord>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct KeyValueRecord {
    key: String,
    value: String,
}

#[derive(Clone)]
struct Svc {
    state: Arc<Mutex<BuildState>>,
}

impl Default for Svc {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(load_state())),
        }
    }
}

impl KeyValueRecord {
    fn from_proto(item: &KeyValue) -> Self {
        Self {
            key: item.key.clone(),
            value: item.value.clone(),
        }
    }

    fn into_proto(self) -> KeyValue {
        KeyValue {
            key: self.key,
            value: self.value,
        }
    }
}

impl ArtifactRecord {
    fn from_proto(item: &Artifact) -> Self {
        Self {
            name: item.name.clone(),
            path: item.path.clone(),
            size_bytes: item.size_bytes,
            sha256: item.sha256.clone(),
            artifact_type: item.r#type,
            metadata: item
                .metadata
                .iter()
                .map(KeyValueRecord::from_proto)
                .collect(),
        }
    }

    fn into_proto(self) -> Artifact {
        Artifact {
            name: self.name,
            path: self.path,
            size_bytes: self.size_bytes,
            sha256: self.sha256,
            r#type: self.artifact_type,
            metadata: self
                .metadata
                .into_iter()
                .map(KeyValueRecord::into_proto)
                .collect(),
        }
    }
}

#[derive(Debug)]
struct LogLine {
    stream: &'static str,
    line: String,
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn now_ts() -> aadk_proto::aadk::v1::Timestamp {
    aadk_proto::aadk::v1::Timestamp {
        unix_millis: now_millis(),
    }
}

fn job_addr() -> String {
    std::env::var("AADK_JOB_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".into())
}

fn project_addr() -> String {
    std::env::var("AADK_PROJECT_ADDR").unwrap_or_else(|_| "127.0.0.1:50053".into())
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

async fn connect_project() -> Result<ProjectServiceClient<Channel>, Status> {
    let addr = project_addr();
    let endpoint = format!("http://{addr}");
    let channel = Channel::from_shared(endpoint)
        .map_err(|e| Status::internal(format!("invalid project endpoint: {e}")))?
        .connect()
        .await
        .map_err(|e| Status::unavailable(format!("project service unavailable: {e}")))?;
    Ok(ProjectServiceClient::new(channel))
}

async fn job_is_cancelled(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
) -> bool {
    let resp = client
        .get_job(GetJobRequest {
            job_id: Some(Id { value: job_id.to_string() }),
        })
        .await;
    let job = match resp {
        Ok(resp) => resp.into_inner().job,
        Err(_) => return false,
    };
    match job.and_then(|job| JobState::try_from(job.state).ok()) {
        Some(JobState::Cancelled) => true,
        _ => false,
    }
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
            job_id: Some(Id { value: job_id.clone() }),
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
                        if JobState::try_from(state.new_state)
                            .unwrap_or(JobState::Unspecified)
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

async fn start_job(
    client: &mut JobServiceClient<Channel>,
    job_type: &str,
    params: Vec<KeyValue>,
    project_id: Option<Id>,
) -> Result<String, Status> {
    let resp = client
        .start_job(StartJobRequest {
            job_type: job_type.into(),
            params,
            project_id,
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
                job_id: Some(Id { value: job_id.to_string() }),
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

fn build_progress_metrics(
    project_id: &str,
    project_path: &Path,
    plan: &BuildPlan,
    req: &BuildRequest,
    args: &[String],
) -> Vec<KeyValue> {
    let mut metrics = vec![
        metric("project_id", project_id),
        metric("project_path", project_path.display()),
        metric("variant", plan.variant.label.clone()),
        metric("clean_first", req.clean_first),
    ];

    if let Some(module) = plan.module.as_ref() {
        if !module.trim().is_empty() {
            metrics.push(metric("module", module));
        }
    }

    if !plan.tasks.is_empty() {
        metrics.push(metric("tasks", plan.tasks.join(" ")));
        metrics.push(metric("task_count", plan.tasks.len()));
    }

    if !args.is_empty() {
        metrics.push(metric("gradle_args", args.join(" ")));
        metrics.push(metric("gradle_arg_count", args.len()));
    }

    metrics
}

fn artifact_type_summary(artifacts: &[Artifact]) -> String {
    let mut types = BTreeSet::new();
    for artifact in artifacts {
        let kind = ArtifactType::try_from(artifact.r#type).unwrap_or(ArtifactType::Unspecified);
        types.insert(artifact_type_label(kind).to_string());
    }
    types.into_iter().collect::<Vec<_>>().join(",")
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
                stream: "build".into(),
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

fn job_error_detail(code: ErrorCode, message: &str, technical: String, correlation_id: &str) -> ErrorDetail {
    ErrorDetail {
        code: code as i32,
        message: message.into(),
        technical_details: technical,
        remedies: vec![],
        correlation_id: correlation_id.into(),
    }
}

fn expand_user(path: &str) -> PathBuf {
    if path == "~" || path.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            let rest = path.strip_prefix("~/").unwrap_or("");
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
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

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let payload = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&tmp, payload)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn load_state() -> BuildState {
    let path = state_file_path();
    match fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str::<BuildState>(&data) {
            Ok(state) => state,
            Err(err) => {
                warn!("Failed to parse {}: {}", path.display(), err);
                BuildState::default()
            }
        },
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                warn!("Failed to read {}: {}", path.display(), err);
            }
            BuildState::default()
        }
    }
}

fn save_state(state: &BuildState) -> io::Result<()> {
    write_json_atomic(&state_file_path(), state)
}

fn save_state_best_effort(state: &BuildState) {
    if let Err(err) = save_state(state) {
        warn!("Failed to persist build state: {}", err);
    }
}

fn build_record(
    job_id: &str,
    project_id: &str,
    project_path: &Path,
    variant: BuildVariant,
    variant_name: &str,
    module: Option<&str>,
    tasks: &[String],
    artifacts: &[Artifact],
) -> BuildRecord {
    BuildRecord {
        job_id: job_id.to_string(),
        project_id: project_id.to_string(),
        module: module.unwrap_or_default().to_string(),
        variant: variant as i32,
        variant_name: variant_name.to_string(),
        tasks: tasks.to_vec(),
        created_at_unix_millis: now_millis(),
        project_path: project_path.to_string_lossy().to_string(),
        artifacts: artifacts.iter().map(ArtifactRecord::from_proto).collect(),
    }
}

fn upsert_build_record(state: &mut BuildState, record: BuildRecord) {
    state.records.retain(|item| item.job_id != record.job_id);
    state.records.insert(0, record);
    if state.records.len() > MAX_BUILD_RECORDS {
        state.records.truncate(MAX_BUILD_RECORDS);
    }
}

fn record_variant_label(record: &BuildRecord) -> Option<String> {
    if !record.variant_name.trim().is_empty() {
        return Some(record.variant_name.trim().to_string());
    }
    let variant = BuildVariant::try_from(record.variant).unwrap_or(BuildVariant::Unspecified);
    if variant == BuildVariant::Unspecified {
        return None;
    }
    Some(variant_label(variant).to_string())
}

fn find_latest_record<'a>(
    state: &'a BuildState,
    project_id: &str,
    query: &ArtifactQuery,
) -> Option<&'a BuildRecord> {
    state.records.iter().find(|record| {
        if record.project_id != project_id {
            return false;
        }

        if !query.modules.is_empty() {
            let record_module = normalize_module_for_compare(&record.module);
            if !query
                .modules
                .iter()
                .any(|module| normalize_module_for_compare(module) == record_module)
            {
                return false;
            }
        }

        if let Some(variant) = query.variant.as_ref() {
            let record_variant = match record_variant_label(record) {
                Some(value) => value,
                None => return false,
            };
            if !record_variant.eq_ignore_ascii_case(variant) {
                return false;
            }
        }

        true
    })
}

async fn get_project_path(project_id: &str) -> Result<PathBuf, Status> {
    let mut client = connect_project().await?;
    let resp = client
        .get_project(GetProjectRequest {
            project_id: Some(Id { value: project_id.to_string() }),
        })
        .await
        .map_err(|e| match e.code() {
            tonic::Code::NotFound => Status::not_found(format!("project not found: {project_id}")),
            tonic::Code::InvalidArgument => Status::invalid_argument(e.message().to_string()),
            tonic::Code::Unavailable => Status::unavailable(format!("project service unavailable: {e}")),
            _ => Status::internal(format!("get project failed: {e}")),
        })?
        .into_inner();

    let project = resp
        .project
        .ok_or_else(|| Status::internal("project lookup returned empty response"))?;
    if project.path.trim().is_empty() {
        return Err(Status::internal("project path missing in ProjectService response"));
    }
    Ok(PathBuf::from(project.path))
}

fn looks_like_path(value: &str) -> bool {
    if value.starts_with('/')
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with("~/")
    {
        return true;
    }
    if value.contains(std::path::MAIN_SEPARATOR) {
        return true;
    }
    if cfg!(windows) && value.contains('\\') {
        return true;
    }
    false
}

async fn resolve_project_path(project_id: &str) -> Result<PathBuf, Status> {
    let trimmed = project_id.trim();
    if trimmed.is_empty() {
        return Err(Status::invalid_argument("project_id is required"));
    }

    if looks_like_path(trimmed) {
        let direct = expand_user(trimmed);
        if direct.is_dir() {
            return Ok(direct);
        }
        return Err(Status::not_found(format!(
            "project path not found: {trimmed}"
        )));
    }

    get_project_path(trimmed).await
}

fn variant_label(variant: BuildVariant) -> &'static str {
    match variant {
        BuildVariant::Debug => "debug",
        BuildVariant::Release => "release",
        BuildVariant::Unspecified => "unspecified",
    }
}

fn normalize_module_label(value: &str) -> Result<Option<String>, Status> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let normalized = trimmed.trim_matches(':').replace('/', ":");
    if normalized.is_empty() {
        return Err(Status::invalid_argument("module is invalid"));
    }

    for segment in normalized.split(':') {
        if segment.is_empty() {
            return Err(Status::invalid_argument("module contains empty segment"));
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return Err(Status::invalid_argument(
                "module contains unsupported characters",
            ));
        }
    }

    Ok(Some(normalized))
}

fn normalize_module_for_compare(value: &str) -> String {
    value
        .trim()
        .trim_matches(':')
        .replace('/', ":")
        .to_ascii_lowercase()
}

fn module_path_from_label(label: &str) -> PathBuf {
    let mut path = PathBuf::new();
    for segment in label.split(':') {
        if !segment.is_empty() {
            path.push(segment);
        }
    }
    path
}

fn module_has_build_file(path: &Path) -> bool {
    path.join("build.gradle").is_file() || path.join("build.gradle.kts").is_file()
}

fn module_exists(project_path: &Path, module: &str) -> bool {
    let module_dir = project_path.join(module_path_from_label(module));
    module_dir.is_dir() && module_has_build_file(&module_dir)
}

fn normalize_variant_name(value: &str) -> Result<Option<String>, Status> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().any(|c| c.is_whitespace() || c == '/' || c == '\\' || c == ':') {
        return Err(Status::invalid_argument("variant_name contains invalid characters"));
    }
    Ok(Some(trimmed.to_string()))
}

fn capitalize_first(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = String::new();
    out.push(first.to_ascii_uppercase());
    out.push_str(chars.as_str());
    out
}

#[derive(Clone)]
struct VariantSelection {
    label: String,
    task_suffix: String,
}

#[derive(Clone)]
struct BuildPlan {
    module: Option<String>,
    variant: VariantSelection,
    tasks: Vec<String>,
}

#[derive(Default, Clone)]
struct ArtifactQuery {
    modules: Vec<String>,
    variant: Option<String>,
    types: Vec<ArtifactType>,
    name_contains: Option<String>,
    path_contains: Option<String>,
}

fn resolve_variant_selection(req: &BuildRequest) -> Result<VariantSelection, Status> {
    if let Some(name) = normalize_variant_name(&req.variant_name)? {
        return Ok(VariantSelection {
            task_suffix: capitalize_first(&name),
            label: name,
        });
    }

    let variant = BuildVariant::try_from(req.variant).unwrap_or(BuildVariant::Debug);
    let label = match variant {
        BuildVariant::Release => "release",
        BuildVariant::Debug => "debug",
        BuildVariant::Unspecified => "debug",
    };
    Ok(VariantSelection {
        label: label.to_string(),
        task_suffix: capitalize_first(label),
    })
}

fn normalized_tasks(tasks: &[String]) -> Vec<String> {
    tasks
        .iter()
        .map(|task| task.trim())
        .filter(|task| !task.is_empty())
        .map(|task| task.to_string())
        .collect()
}

fn is_clean_task(task: &str) -> bool {
    task == "clean" || task.ends_with(":clean")
}

fn tasks_for_selection(
    module: Option<&str>,
    variant: &VariantSelection,
    clean_first: bool,
    overrides: &[String],
) -> Vec<String> {
    let mut tasks = if overrides.is_empty() {
        vec![format!("assemble{}", variant.task_suffix)]
    } else {
        overrides.to_vec()
    };

    if clean_first && !tasks.iter().any(|task| is_clean_task(task)) {
        tasks.insert(0, "clean".into());
    }

    if let Some(module) = module {
        tasks = tasks
            .into_iter()
            .map(|task| {
                if task.contains(':') || is_clean_task(&task) {
                    task
                } else {
                    format!(":{}:{}", module, task)
                }
            })
            .collect();
    }

    tasks
}

fn build_plan_for_request(req: &BuildRequest) -> Result<BuildPlan, Status> {
    let module = normalize_module_label(&req.module)?;
    let variant = resolve_variant_selection(req)?;
    let overrides = normalized_tasks(&req.tasks);
    let tasks = tasks_for_selection(module.as_deref(), &variant, req.clean_first, &overrides);

    Ok(BuildPlan {
        module,
        variant,
        tasks,
    })
}

fn arg_is_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn gradle_daemon_enabled() -> bool {
    matches!(
        std::env::var("AADK_GRADLE_DAEMON"),
        Ok(val) if val == "1" || val.eq_ignore_ascii_case("true")
    )
}

fn gradle_stacktrace_enabled() -> bool {
    matches!(
        std::env::var("AADK_GRADLE_STACKTRACE"),
        Ok(val) if val == "1" || val.eq_ignore_ascii_case("true")
    )
}

fn gradle_wrapper_required() -> bool {
    matches!(
        std::env::var("AADK_GRADLE_REQUIRE_WRAPPER"),
        Ok(val) if val == "1" || val.eq_ignore_ascii_case("true")
    )
}

fn gradle_user_home() -> Option<PathBuf> {
    if let Ok(existing) = std::env::var("GRADLE_USER_HOME") {
        if !existing.trim().is_empty() {
            return None;
        }
    }
    if let Ok(configured) = std::env::var("AADK_GRADLE_USER_HOME") {
        if !configured.trim().is_empty() {
            return Some(expand_user(&configured));
        }
    }
    Some(data_dir().join("gradle"))
}

fn expand_gradle_args(items: Vec<KeyValue>) -> Vec<String> {
    let mut args = Vec::new();
    for item in items {
        let key = item.key.trim();
        let value = item.value.trim();
        if key.is_empty() {
            continue;
        }

        if value.is_empty() {
            args.push(key.to_string());
            continue;
        }

        if key == "-P" || key == "-D" || key.ends_with('=') {
            args.push(format!("{key}{value}"));
            continue;
        }

        if key.starts_with('-') {
            args.push(key.to_string());
            args.push(value.to_string());
            continue;
        }

        args.push(format!("{key}={value}"));
    }
    args
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

fn spawn_error(err: io::Error) -> Status {
    if err.kind() == io::ErrorKind::NotFound {
        Status::failed_precondition("gradle not found (missing gradlew or gradle in PATH)")
    } else {
        Status::internal(format!("failed to spawn gradle: {err}"))
    }
}

struct GradleSpawn {
    child: Child,
    description: String,
}

fn spawn_gradle(project_dir: &Path, args: &[String]) -> Result<GradleSpawn, Status> {
    let wrapper_props = project_dir.join("gradle").join("wrapper").join("gradle-wrapper.properties");
    let wrapper = if cfg!(windows) {
        project_dir.join("gradlew.bat")
    } else {
        project_dir.join("gradlew")
    };
    let (mut cmd, description) = if wrapper.is_file() {
        if !wrapper_props.is_file() {
            return Err(Status::failed_precondition(
                "gradle wrapper missing gradle/wrapper/gradle-wrapper.properties",
            ));
        }
        let cmd = if cfg!(windows) {
            Command::new(&wrapper)
        } else if is_executable(&wrapper) {
            Command::new(&wrapper)
        } else {
            let mut cmd = Command::new("sh");
            cmd.arg(&wrapper);
            cmd
        };
        (cmd, wrapper.display().to_string())
    } else {
        if gradle_wrapper_required() {
            return Err(Status::failed_precondition(
                "gradle wrapper not found (AADK_GRADLE_REQUIRE_WRAPPER=1)",
            ));
        }
        (Command::new("gradle"), "gradle (PATH)".into())
    };

    cmd.current_dir(project_dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(home) = gradle_user_home() {
        if let Err(err) = fs::create_dir_all(&home) {
            return Err(Status::internal(format!(
                "failed to create GRADLE_USER_HOME: {err}"
            )));
        }
        cmd.env("GRADLE_USER_HOME", home);
    }

    let child = cmd.spawn().map_err(spawn_error)?;
    Ok(GradleSpawn { child, description })
}

async fn read_lines<R>(reader: R, stream: &'static str, tx: mpsc::Sender<LogLine>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let payload = LogLine { stream, line };
        if tx.send(payload).await.is_err() {
            break;
        }
    }
}

fn append_recent(recent: &mut VecDeque<String>, line: String) {
    if recent.len() >= RECENT_LOG_LIMIT {
        recent.pop_front();
    }
    recent.push_back(line);
}

fn collect_recent(recent: &VecDeque<String>) -> String {
    let mut combined = String::new();
    for line in recent {
        combined.push_str(line);
        if !line.ends_with('\n') {
            combined.push('\n');
        }
    }
    combined
}

fn module_outputs_exist(module_path: &Path) -> bool {
    let outputs = [
        module_path.join("build/outputs/apk"),
        module_path.join("build/outputs/bundle"),
        module_path.join("build/outputs/aar"),
        module_path.join("build/outputs/mapping"),
        module_path.join("build/test-results"),
        module_path.join("build/outputs/androidTest-results"),
    ];
    outputs.iter().any(|path| path.is_dir())
}

fn output_roots_for_modules(
    project_path: &Path,
    modules: &[String],
) -> Vec<(Option<String>, PathBuf)> {
    let mut roots = Vec::new();

    if modules.is_empty() {
        if module_outputs_exist(project_path) || module_has_build_file(project_path) {
            roots.push((None, project_path.to_path_buf()));
        }

        if let Ok(entries) = fs::read_dir(project_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if !module_outputs_exist(&path) && !module_has_build_file(&path) {
                    continue;
                }
                let module_name = entry.file_name().to_string_lossy().to_string();
                roots.push((Some(module_name), path));
            }
        }

        return roots;
    }

    for module in modules {
        let module_dir = project_path.join(module_path_from_label(module));
        if module_dir.is_dir() {
            roots.push((Some(module.clone()), module_dir));
        }
    }

    roots
}

fn output_dirs_for_root(root: &Path) -> Vec<(ArtifactType, PathBuf)> {
    vec![
        (ArtifactType::Apk, root.join("build/outputs/apk")),
        (ArtifactType::Aab, root.join("build/outputs/bundle")),
        (ArtifactType::Aar, root.join("build/outputs/aar")),
        (ArtifactType::Mapping, root.join("build/outputs/mapping")),
        (ArtifactType::TestResult, root.join("build/test-results")),
        (
            ArtifactType::TestResult,
            root.join("build/outputs/androidTest-results"),
        ),
    ]
}

fn variant_search_root(root: &Path, variant: Option<&str>) -> PathBuf {
    let Some(variant) = variant else {
        return root.to_path_buf();
    };
    let trimmed = variant.trim();
    if trimmed.is_empty() {
        return root.to_path_buf();
    }
    let candidate = root.join(trimmed);
    if candidate.is_dir() {
        return candidate;
    }
    let lower = trimmed.to_lowercase();
    let lower_candidate = root.join(&lower);
    if lower_candidate.is_dir() {
        return lower_candidate;
    }
    root.to_path_buf()
}

fn matches_artifact_type(path: &Path, artifact_type: ArtifactType) -> bool {
    match artifact_type {
        ArtifactType::Apk => path.extension().map(|e| e == "apk").unwrap_or(false),
        ArtifactType::Aab => path.extension().map(|e| e == "aab").unwrap_or(false),
        ArtifactType::Aar => path.extension().map(|e| e == "aar").unwrap_or(false),
        ArtifactType::Mapping => path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.eq_ignore_ascii_case("mapping.txt"))
            .unwrap_or(false),
        ArtifactType::TestResult => path.extension().map(|e| e == "xml").unwrap_or(false),
        ArtifactType::Unspecified => false,
    }
}

fn collect_artifact_paths(
    root: &Path,
    artifact_type: ArtifactType,
    out: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_artifact_paths(&path, artifact_type, out)?;
        } else if matches_artifact_type(&path, artifact_type) {
            out.push(path);
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(nibble_to_hex(b >> 4));
        out.push(nibble_to_hex(b & 0x0f));
    }
    out
}

fn nibble_to_hex(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => '0',
    }
}

fn artifact_type_label(artifact_type: ArtifactType) -> &'static str {
    match artifact_type {
        ArtifactType::Apk => "apk",
        ArtifactType::Aab => "aab",
        ArtifactType::Aar => "aar",
        ArtifactType::Mapping => "mapping",
        ArtifactType::TestResult => "test_result",
        ArtifactType::Unspecified => "unspecified",
    }
}

fn artifact_metadata_value<'a>(artifact: &'a Artifact, key: &str) -> Option<&'a str> {
    artifact
        .metadata
        .iter()
        .find(|item| item.key == key)
        .map(|item| item.value.as_str())
}

fn artifact_matches_variant(artifact: &Artifact, variant: &str) -> bool {
    let trimmed = variant.trim();
    if trimmed.is_empty() {
        return true;
    }
    if let Some(value) = artifact_metadata_value(artifact, "variant") {
        if value.eq_ignore_ascii_case(trimmed) {
            return true;
        }
    }
    let needle = trimmed.to_lowercase();
    let name = artifact.name.to_lowercase();
    if name.contains(&needle) {
        return true;
    }
    let path = artifact.path.to_lowercase();
    path.contains(&needle)
}

fn artifact_matches(artifact: &Artifact, query: &ArtifactQuery) -> bool {
    if !query.types.is_empty() {
        let artifact_type = ArtifactType::try_from(artifact.r#type).unwrap_or(ArtifactType::Unspecified);
        if artifact_type == ArtifactType::Unspecified {
            return false;
        }
        if !query.types.iter().any(|t| *t == artifact_type) {
            return false;
        }
    }

    if !query.modules.is_empty() {
        let Some(module) = artifact_metadata_value(artifact, "module") else {
            return false;
        };
        let module_value = normalize_module_for_compare(module);
        if !query
            .modules
            .iter()
            .any(|m| normalize_module_for_compare(m) == module_value)
        {
            return false;
        }
    }

    if let Some(variant) = query.variant.as_ref() {
        if !artifact_matches_variant(artifact, variant) {
            return false;
        }
    }

    if let Some(name_contains) = query.name_contains.as_ref() {
        if !artifact.name.to_lowercase().contains(name_contains) {
            return false;
        }
    }

    if let Some(path_contains) = query.path_contains.as_ref() {
        if !artifact.path.to_lowercase().contains(path_contains) {
            return false;
        }
    }

    true
}

fn collect_artifacts(
    project_path: &Path,
    query: &ArtifactQuery,
    primary_task: Option<&str>,
) -> Vec<Artifact> {
    let mut artifacts = Vec::new();
    let roots = output_roots_for_modules(project_path, &query.modules);
    let variant_filter = query.variant.as_deref();

    for (module, root) in roots {
        for (artifact_type, output_dir) in output_dirs_for_root(&root) {
            if !output_dir.is_dir() {
                continue;
            }
            let search_root = variant_search_root(&output_dir, variant_filter);
            let mut paths = Vec::new();
            if collect_artifact_paths(&search_root, artifact_type, &mut paths).is_err() {
                continue;
            }

            for path in paths {
                if !path.is_file() {
                    continue;
                }
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("artifact.bin")
                    .to_string();
                let size_bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let sha256 = match sha256_file(&path) {
                    Ok(value) => value,
                    Err(err) => {
                        warn!("failed to hash {}: {}", path.display(), err);
                        String::new()
                    }
                };
                let mut metadata = Vec::new();
                if let Some(module) = module.clone() {
                    metadata.push(KeyValue {
                        key: "module".into(),
                        value: module,
                    });
                }
                if let Some(dir) = variant_filter {
                    metadata.push(KeyValue {
                        key: "variant".into(),
                        value: dir.into(),
                    });
                }
                metadata.push(KeyValue {
                    key: "artifact_type".into(),
                    value: artifact_type_label(artifact_type).into(),
                });
                if let Some(task) = primary_task {
                    metadata.push(KeyValue {
                        key: "task".into(),
                        value: task.to_string(),
                    });
                }

                artifacts.push(Artifact {
                    name,
                    path: path.to_string_lossy().to_string(),
                    size_bytes,
                    sha256,
                    metadata,
                    r#type: artifact_type as i32,
                });
            }
        }
    }

    artifacts
        .into_iter()
        .filter(|artifact| artifact_matches(artifact, query))
        .collect()
}

fn artifact_query_from_filter(filter: &ArtifactFilter) -> Result<ArtifactQuery, Status> {
    let mut query = ArtifactQuery::default();

    for module in &filter.modules {
        if let Some(label) = normalize_module_label(module)? {
            query.modules.push(label);
        }
    }

    if let Some(variant) = normalize_variant_name(&filter.variant)? {
        query.variant = Some(variant);
    }

    for raw in &filter.types {
        let artifact_type = ArtifactType::try_from(*raw).unwrap_or(ArtifactType::Unspecified);
        if artifact_type != ArtifactType::Unspecified {
            query.types.push(artifact_type);
        }
    }

    let name_contains = filter.name_contains.trim();
    if !name_contains.is_empty() {
        query.name_contains = Some(name_contains.to_lowercase());
    }

    let path_contains = filter.path_contains.trim();
    if !path_contains.is_empty() {
        query.path_contains = Some(path_contains.to_lowercase());
    }

    Ok(query)
}

fn artifact_query_from_request(req: &ListArtifactsRequest) -> Result<ArtifactQuery, Status> {
    let mut query = match req.filter.as_ref() {
        Some(filter) => artifact_query_from_filter(filter)?,
        None => ArtifactQuery::default(),
    };

    if query.variant.is_none() {
        let variant = BuildVariant::try_from(req.variant).unwrap_or(BuildVariant::Unspecified);
        if variant != BuildVariant::Unspecified {
            query.variant = Some(variant_label(variant).to_string());
        }
    }

    Ok(query)
}

async fn run_build_job(
    state: Arc<Mutex<BuildState>>,
    job_id: String,
    req: BuildRequest,
    plan: BuildPlan,
) {
    let mut job_client = match connect_job().await {
        Ok(client) => client,
        Err(err) => {
            warn!("build job {job_id}: failed to connect job service: {err}");
            return;
        }
    };

    let mut cancel_rx = spawn_cancel_watcher(job_id.clone()).await;
    if job_is_cancelled(&mut job_client, &job_id).await {
        let _ = publish_log(&mut job_client, &job_id, "Build cancelled before start\n").await;
        return;
    }

    let _ = publish_state(&mut job_client, &job_id, JobState::Running).await;
    let _ = publish_log(&mut job_client, &job_id, "Starting Gradle build\n").await;

    let project_id = match req.project_id.as_ref() {
        Some(id) if !id.value.trim().is_empty() => id.value.trim().to_string(),
        _ => {
            let detail = job_error_detail(
                ErrorCode::InvalidArgument,
                "project_id is required",
                "missing project_id in BuildRequest".into(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    let project_path = match resolve_project_path(&project_id).await {
        Ok(path) => path,
        Err(err) => {
            let code = match err.code() {
                tonic::Code::NotFound => ErrorCode::NotFound,
                tonic::Code::InvalidArgument => ErrorCode::InvalidArgument,
                tonic::Code::Unavailable => ErrorCode::Unavailable,
                tonic::Code::FailedPrecondition => ErrorCode::InvalidArgument,
                _ => ErrorCode::Internal,
            };
            let detail = job_error_detail(
                code,
                "project resolution failed",
                err.message().to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    if *cancel_rx.borrow() {
        let _ = publish_log(&mut job_client, &job_id, "Build cancelled before Gradle start\n").await;
        return;
    }

    if let Some(module) = plan.module.as_deref() {
        if !module_exists(&project_path, module) {
            let detail = job_error_detail(
                ErrorCode::InvalidArgument,
                "module not found",
                format!("module not found or missing build file: {module}"),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    }

    let variant = BuildVariant::try_from(req.variant).unwrap_or(BuildVariant::Unspecified);
    let mut args = plan.tasks.clone();
    let extra_args = expand_gradle_args(req.gradle_args);
    args.extend(extra_args);

    if !gradle_daemon_enabled() && !arg_is_flag(&args, "--no-daemon") {
        args.push("--no-daemon".into());
    }
    if gradle_stacktrace_enabled() && !arg_is_flag(&args, "--stacktrace") {
        args.push("--stacktrace".into());
    }

    let _ = publish_progress(
        &mut job_client,
        &job_id,
        10,
        "preflight",
        build_progress_metrics(&project_id, &project_path, &plan, &req, &args),
    )
    .await;

    let _ = publish_log(
        &mut job_client,
        &job_id,
        &format!("Working dir: {}\n", project_path.display()),
    )
    .await;

    let _ = publish_log(
        &mut job_client,
        &job_id,
        &format!("Gradle args: {}\n", args.join(" ")),
    )
    .await;

    if let Some(home) = gradle_user_home() {
        let _ = publish_log(
            &mut job_client,
            &job_id,
            &format!("GRADLE_USER_HOME={}\n", home.display()),
        )
        .await;
    }

    let spawn = match spawn_gradle(&project_path, &args) {
        Ok(child) => child,
        Err(err) => {
            let detail = job_error_detail(
                ErrorCode::BuildFailed,
                "failed to start Gradle",
                err.message().to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };
    let _ = publish_log(
        &mut job_client,
        &job_id,
        &format!("Gradle command: {}\n", spawn.description),
    )
    .await;
    let mut child = spawn.child;

    let stdout = match child.stdout.take() {
        Some(out) => out,
        None => {
            let detail = job_error_detail(
                ErrorCode::BuildFailed,
                "failed to capture gradle stdout",
                "stdout pipe missing".into(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    let stderr = match child.stderr.take() {
        Some(err) => err,
        None => {
            let detail = job_error_detail(
                ErrorCode::BuildFailed,
                "failed to capture gradle stderr",
                "stderr pipe missing".into(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    let (line_tx, mut line_rx) = mpsc::channel::<LogLine>(LOG_CHANNEL_CAPACITY);
    tokio::spawn(read_lines(stdout, "stdout", line_tx.clone()));
    tokio::spawn(read_lines(stderr, "stderr", line_tx));

    let _ = publish_progress(
        &mut job_client,
        &job_id,
        25,
        "gradle running",
        build_progress_metrics(&project_id, &project_path, &plan, &req, &args),
    )
    .await;

    let mut recent = VecDeque::with_capacity(RECENT_LOG_LIMIT);
    let start = Instant::now();
    let mut status: Option<Result<std::process::ExitStatus, io::Error>> = None;

    loop {
        tokio::select! {
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    let _ = publish_log(&mut job_client, &job_id, "Cancellation requested; stopping Gradle\n").await;
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return;
                }
            }
            line = line_rx.recv() => {
                match line {
                    Some(line) => {
                        let mut text = format!("[{}] {}", line.stream, line.line);
                        if !text.ends_with('\n') {
                            text.push('\n');
                        }
                        append_recent(&mut recent, text.clone());
                        let _ = publish_log(&mut job_client, &job_id, &text).await;
                    }
                    None => {
                        if status.is_some() {
                            break;
                        }
                    }
                }
            }
            result = child.wait(), if status.is_none() => {
                status = Some(result);
            }
        }
    }

    let status = match status {
        Some(Ok(status)) => status,
        Some(Err(err)) => {
            let detail = job_error_detail(
                ErrorCode::BuildFailed,
                "gradle process failed",
                err.to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
        None => {
            let detail = job_error_detail(
                ErrorCode::BuildFailed,
                "gradle process did not return status",
                "missing exit status".into(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    let duration_ms = start.elapsed().as_millis();

    if *cancel_rx.borrow() {
        let _ = publish_log(&mut job_client, &job_id, "Build cancelled before completion\n").await;
        return;
    }

    if status.success() {
        let mut query = ArtifactQuery::default();
        if let Some(module) = plan.module.as_ref() {
            query.modules.push(module.clone());
        }
        query.variant = Some(plan.variant.label.clone());
        let primary_task = plan
            .tasks
            .iter()
            .find(|task| !is_clean_task(task))
            .map(|task| task.as_str());
        let artifacts = collect_artifacts(&project_path, &query, primary_task);
        {
            let mut st = state.lock().await;
            let record = build_record(
                &job_id,
                &project_id,
                &project_path,
                variant,
                &plan.variant.label,
                plan.module.as_deref(),
                &plan.tasks,
                &artifacts,
            );
            upsert_build_record(&mut st, record);
            save_state_best_effort(&st);
        }
        let mut outputs = vec![
            KeyValue {
                key: "duration_ms".into(),
                value: duration_ms.to_string(),
            },
            KeyValue {
                key: "artifact_count".into(),
                value: artifacts.len().to_string(),
            },
        ];
        outputs.push(KeyValue {
            key: "variant".into(),
            value: plan.variant.label.clone(),
        });
        if let Some(module) = plan.module.as_ref() {
            outputs.push(KeyValue {
                key: "module".into(),
                value: module.clone(),
            });
        }
        if !plan.tasks.is_empty() {
            outputs.push(KeyValue {
                key: "tasks".into(),
                value: plan.tasks.join(" "),
            });
        }

        for artifact in artifacts.iter().take(10) {
            let artifact_type =
                ArtifactType::try_from(artifact.r#type).unwrap_or(ArtifactType::Unspecified);
            outputs.push(KeyValue {
                key: "artifact_path".into(),
                value: artifact.path.clone(),
            });
            outputs.push(KeyValue {
                key: "artifact_type".into(),
                value: artifact_type_label(artifact_type).into(),
            });
            if artifact_type == ArtifactType::Apk {
                outputs.push(KeyValue {
                    key: "apk_path".into(),
                    value: artifact.path.clone(),
                });
            }
        }

        let mut metrics = build_progress_metrics(&project_id, &project_path, &plan, &req, &args);
        metrics.push(metric("artifact_count", artifacts.len()));
        let type_summary = artifact_type_summary(&artifacts);
        if !type_summary.is_empty() {
            metrics.push(metric("artifact_types", type_summary));
        }
        let _ = publish_progress(&mut job_client, &job_id, 95, "finalizing", metrics).await;
        let _ = publish_completed(
            &mut job_client,
            &job_id,
            "Gradle build finished",
            outputs,
        )
        .await;
    } else {
        let code = status.code().unwrap_or(-1);
        let mut detail = format!("exit_code={code}\n");
        detail.push_str(&collect_recent(&recent));
        let detail = job_error_detail(
            ErrorCode::BuildFailed,
            "Gradle build failed",
            detail,
            &job_id,
        );
        let _ = publish_failed(&mut job_client, &job_id, detail).await;
    }
}

#[tonic::async_trait]
impl BuildService for Svc {
    async fn build(&self, request: Request<BuildRequest>) -> Result<Response<BuildResponse>, Status> {
        let req = request.into_inner();
        let project_id = req
            .project_id
            .clone()
            .ok_or_else(|| Status::invalid_argument("project_id is required"))?;

        let plan = build_plan_for_request(&req)?;
        let mut job_client = connect_job().await?;
        let job_id = req
            .job_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(String::new);
        let job_id = if job_id.is_empty() {
            let mut params = vec![
                KeyValue {
                    key: "variant".into(),
                    value: plan.variant.label.clone(),
                },
                KeyValue {
                    key: "clean_first".into(),
                    value: req.clean_first.to_string(),
                },
            ];
            if let Some(module) = plan.module.as_ref() {
                params.push(KeyValue {
                    key: "module".into(),
                    value: module.clone(),
                });
            }
            if !plan.tasks.is_empty() {
                params.push(KeyValue {
                    key: "tasks".into(),
                    value: plan.tasks.join(" "),
                });
            }
            start_job(
                &mut job_client,
                "build.run",
                params,
                Some(project_id),
            )
            .await?
        } else {
            job_id
        };

        let state = self.state.clone();
        tokio::spawn(run_build_job(state, job_id.clone(), req, plan));

        Ok(Response::new(BuildResponse {
            job_id: Some(Id { value: job_id }),
        }))
    }

    async fn list_artifacts(
        &self,
        request: Request<ListArtifactsRequest>,
    ) -> Result<Response<ListArtifactsResponse>, Status> {
        let req = request.into_inner();
        let project_id = req
            .project_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .unwrap_or_default();
        let query = artifact_query_from_request(&req)?;
        if !project_id.trim().is_empty() {
            let record = {
                let st = self.state.lock().await;
                find_latest_record(&st, &project_id, &query).cloned()
            };
            if let Some(record) = record {
                let artifacts = record
                    .artifacts
                    .into_iter()
                    .map(ArtifactRecord::into_proto)
                    .filter(|artifact| artifact_matches(artifact, &query))
                    .collect();
                return Ok(Response::new(ListArtifactsResponse { artifacts }));
            }
        }

        let project_path = resolve_project_path(&project_id).await?;
        let artifacts = collect_artifacts(&project_path, &query, None);

        Ok(Response::new(ListArtifactsResponse { artifacts }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let addr_str = std::env::var("AADK_BUILD_ADDR").unwrap_or_else(|_| "127.0.0.1:50054".to_string());
    let addr: SocketAddr = addr_str.parse()?;

    let svc = Svc::default();
    info!("aadk-build listening on {}", addr);

    tonic::transport::Server::builder()
        .add_service(BuildServiceServer::new(svc))
        .serve(addr)
        .await?;

    Ok(())
}
