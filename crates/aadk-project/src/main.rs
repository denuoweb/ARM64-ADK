use std::{
    collections::HashMap,
    fs,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
};

use aadk_proto::aadk::v1::{
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    project_service_server::{ProjectService, ProjectServiceServer},
    CreateProjectRequest, CreateProjectResponse, ErrorCode, ErrorDetail, Id, JobCompleted, JobEvent,
    JobFailed, JobLogAppended, JobProgress, JobProgressUpdated, JobState, JobStateChanged,
    KeyValue, ListRecentProjectsRequest, ListRecentProjectsResponse, ListTemplatesRequest,
    ListTemplatesResponse, LogChunk, OpenProjectRequest, OpenProjectResponse, PageInfo, Project,
    PublishJobEventRequest, RunId, SetProjectConfigRequest, SetProjectConfigResponse, StartJobRequest,
    Template, Timestamp, StreamJobEventsRequest, GetJobRequest, GetProjectRequest, GetProjectResponse,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex, watch};
use tonic::{transport::Channel, Request, Response, Status};
use tracing::{info, warn};
use uuid::Uuid;

const STATE_FILE_NAME: &str = "projects.json";
const PROJECT_DIR_NAME: &str = ".aadk";
const PROJECT_META_NAME: &str = "project.json";
const TEMPLATE_ENV: &str = "AADK_PROJECT_TEMPLATES";
const MAX_RECENTS: usize = 50;
const DEFAULT_PAGE_SIZE: usize = 25;
const MIN_SDK_KEY: &str = "minSdk";
const COMPILE_SDK_KEY: &str = "compileSdk";

#[derive(Default)]
struct State {
    recent: Vec<ProjectMetadata>,
}

#[derive(Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    projects: Vec<ProjectMetadata>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProjectMetadata {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    project_id: String,
    name: String,
    path: String,
    created_at: i64,
    last_opened_at: i64,
    #[serde(default)]
    template_id: Option<String>,
    #[serde(default)]
    toolchain_set_id: Option<String>,
    #[serde(default)]
    default_target_id: Option<String>,
}

#[derive(Debug)]
struct TemplateRegistry {
    base_dir: PathBuf,
    templates: Vec<TemplateEntry>,
}

#[derive(Debug, Deserialize)]
struct TemplateRegistryFile {
    #[serde(default)]
    templates: Vec<TemplateEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TemplateEntry {
    template_id: String,
    name: String,
    description: String,
    path: String,
    #[serde(default)]
    defaults: Vec<TemplateDefault>,
    #[serde(default)]
    exclude_dirs: Vec<String>,
    #[serde(default)]
    exclude_files: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TemplateDefault {
    key: String,
    value: String,
}

#[derive(Debug)]
struct TemplateDefaultsError {
    message: String,
    details: String,
}

#[derive(Clone)]
struct ResolvedTemplateDefaults {
    min_sdk: String,
    compile_sdk: String,
    entries: Vec<TemplateDefault>,
}

#[derive(Default)]
struct ParsedTemplateDefaults {
    min_sdk: Option<String>,
    compile_sdk: Option<String>,
}

#[derive(Debug)]
enum CopyUpdate {
    Total(u64),
    File { copied: u64, total: u64 },
}

#[derive(Clone)]
struct Svc {
    state: Arc<Mutex<State>>,
}

struct CreateRequestParts {
    name: String,
    path: PathBuf,
    template_id: String,
    toolchain_set_id: Option<String>,
    resolved_defaults: ResolvedTemplateDefaults,
    run_id: Option<String>,
    correlation_id: String,
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
    Timestamp { unix_millis: now_millis() }
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

fn metadata_file_path(project_dir: &Path) -> PathBuf {
    project_dir.join(PROJECT_DIR_NAME).join(PROJECT_META_NAME)
}

fn templates_registry_path() -> PathBuf {
    if let Ok(path) = std::env::var(TEMPLATE_ENV) {
        PathBuf::from(path)
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates").join("registry.json")
    }
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
            Ok(file) => State { recent: file.projects },
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
        projects: state.recent.clone(),
    };
    write_json_atomic(&state_file_path(), &file)
}

fn upsert_recent(state: &mut State, meta: ProjectMetadata) {
    state.recent.retain(|item| item.project_id != meta.project_id && item.path != meta.path);
    state.recent.insert(0, meta);
    if state.recent.len() > MAX_RECENTS {
        state.recent.truncate(MAX_RECENTS);
    }
}

impl ProjectMetadata {
    fn new(
        project_id: String,
        name: String,
        path: &Path,
        template_id: Option<String>,
        toolchain_set_id: Option<String>,
        default_target_id: Option<String>,
    ) -> Self {
        let now = now_millis();
        Self {
            schema_version: default_schema_version(),
            project_id,
            name,
            path: path.to_string_lossy().to_string(),
            created_at: now,
            last_opened_at: now,
            template_id,
            toolchain_set_id,
            default_target_id,
        }
    }

    fn touch_opened(&mut self) {
        self.last_opened_at = now_millis();
    }

    fn to_proto(&self) -> Project {
        Project {
            project_id: Some(Id {
                value: self.project_id.clone(),
            }),
            name: self.name.clone(),
            path: self.path.clone(),
            created_at: Some(Timestamp {
                unix_millis: self.created_at,
            }),
            last_opened_at: Some(Timestamp {
                unix_millis: self.last_opened_at,
            }),
            toolchain_set_id: self
                .toolchain_set_id
                .as_ref()
                .map(|value| Id { value: value.clone() }),
            default_target_id: self
                .default_target_id
                .as_ref()
                .map(|value| Id { value: value.clone() }),
        }
    }
}

impl TemplateEntry {
    fn is_valid(&self) -> bool {
        !self.template_id.trim().is_empty()
            && !self.name.trim().is_empty()
            && !self.path.trim().is_empty()
    }

    fn to_proto_with_defaults(&self, defaults: &[TemplateDefault]) -> Template {
        Template {
            template_id: Some(Id {
                value: self.template_id.clone(),
            }),
            name: self.name.clone(),
            description: self.description.clone(),
            defaults: defaults
                .iter()
                .map(|item| KeyValue {
                    key: item.key.clone(),
                    value: item.value.clone(),
                })
                .collect(),
        }
    }
}

impl TemplateRegistry {
    fn load() -> Result<Self, Status> {
        let registry_path = templates_registry_path();
        let base_dir = registry_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let data = fs::read_to_string(&registry_path).map_err(|err| {
            Status::not_found(format!(
                "template registry not found: {} ({})",
                registry_path.display(),
                err
            ))
        })?;
        let file: TemplateRegistryFile = serde_json::from_str(&data)
            .map_err(|err| Status::internal(format!("invalid template registry: {err}")))?;
        let mut templates = Vec::new();
        for entry in file.templates {
            if entry.is_valid() {
                templates.push(entry);
            } else {
                warn!(
                    "Skipping invalid template entry in {}",
                    registry_path.display()
                );
            }
        }
        if templates.is_empty() {
            return Err(Status::failed_precondition(
                "template registry has no valid templates",
            ));
        }
        Ok(Self { base_dir, templates })
    }

    fn list_with_defaults(&self) -> Result<Vec<Template>, Status> {
        let mut templates = Vec::new();
        for template in &self.templates {
            let path = self.resolve_path(template);
            let resolved = resolve_template_defaults(template, &path, &[])
                .map_err(|err| Status::failed_precondition(format!("{}: {}", err.message, err.details)))?;
            templates.push(template.to_proto_with_defaults(&resolved.entries));
        }
        Ok(templates)
    }

    fn find(&self, template_id: &str) -> Option<TemplateEntry> {
        self.templates
            .iter()
            .find(|entry| entry.template_id == template_id)
            .cloned()
    }

    fn resolve_path(&self, template: &TemplateEntry) -> PathBuf {
        let raw = PathBuf::from(&template.path);
        if raw.is_absolute() {
            raw
        } else {
            self.base_dir.join(raw)
        }
    }
}

fn template_defaults_error(message: &str, details: impl ToString) -> TemplateDefaultsError {
    TemplateDefaultsError {
        message: message.to_string(),
        details: details.to_string(),
    }
}

fn normalize_sdk_value(key: &str, raw: Option<&str>) -> Result<String, TemplateDefaultsError> {
    let Some(raw) = raw else {
        return Err(template_defaults_error(
            "template defaults missing required key",
            format!("missing {key}"),
        ));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(template_defaults_error(
            "template defaults missing required key",
            format!("empty {key}"),
        ));
    }
    let digits: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return Err(template_defaults_error(
            "template defaults invalid",
            format!("{key} must be numeric (got '{trimmed}')"),
        ));
    }
    Ok(digits)
}

fn parse_value_after_keyword(input: &str) -> Option<String> {
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.peek() {
        if ch.is_whitespace() || *ch == '=' || *ch == '(' {
            chars.next();
        } else {
            break;
        }
    }

    let mut token = String::new();
    let Some(first) = chars.next() else {
        return None;
    };
    if first == '"' || first == '\'' {
        for ch in chars {
            if ch == first {
                break;
            }
            token.push(ch);
        }
    } else {
        token.push(first);
        for ch in chars {
            if ch.is_whitespace() || ch == ')' || ch == ',' {
                break;
            }
            token.push(ch);
        }
    }

    let digits: String = token.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

fn find_sdk_value(contents: &str, keys: &[&str]) -> Option<String> {
    for key in keys {
        let mut search = contents;
        while let Some(index) = search.find(key) {
            let after = &search[index + key.len()..];
            if let Some(value) = parse_value_after_keyword(after) {
                return Some(value);
            }
            search = after;
        }
    }
    None
}

fn collect_gradle_files(
    root: &Path,
    template: &TemplateEntry,
    files: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        let name = entry.file_name().to_string_lossy().to_string();
        if file_type.is_dir() {
            if template.exclude_dirs.iter().any(|item| item == &name) {
                continue;
            }
            collect_gradle_files(&path, template, files)?;
        } else if file_type.is_file() {
            if template.exclude_files.iter().any(|item| item == &name) {
                continue;
            }
            if name == "build.gradle" || name == "build.gradle.kts" {
                files.push(path);
            }
        }
    }
    Ok(())
}

fn parse_template_defaults_from_gradle(
    template_path: &Path,
    template: &TemplateEntry,
) -> ParsedTemplateDefaults {
    let mut parsed = ParsedTemplateDefaults::default();
    let mut files = Vec::new();
    if collect_gradle_files(template_path, template, &mut files).is_err() {
        return parsed;
    }

    for file in files {
        let Ok(contents) = fs::read_to_string(&file) else {
            continue;
        };
        if parsed.min_sdk.is_none() {
            parsed.min_sdk = find_sdk_value(&contents, &["minSdk", "minSdkVersion"]);
        }
        if parsed.compile_sdk.is_none() {
            parsed.compile_sdk = find_sdk_value(&contents, &["compileSdk", "compileSdkVersion"]);
        }
        if parsed.min_sdk.is_some() && parsed.compile_sdk.is_some() {
            break;
        }
    }
    parsed
}

fn resolve_template_defaults(
    template: &TemplateEntry,
    template_path: &Path,
    params: &[KeyValue],
) -> Result<ResolvedTemplateDefaults, TemplateDefaultsError> {
    let mut values: HashMap<String, String> = HashMap::new();
    let mut keys: Vec<String> = Vec::new();

    for item in &template.defaults {
        if item.key.trim().is_empty() {
            continue;
        }
        if !values.contains_key(&item.key) {
            keys.push(item.key.clone());
        }
        values.insert(item.key.clone(), item.value.clone());
    }

    for item in params {
        let key = item.key.trim();
        if key.is_empty() {
            continue;
        }
        let key = key.to_string();
        if !values.contains_key(&key) {
            keys.push(key.clone());
        }
        values.insert(key, item.value.clone());
    }

    if !values.contains_key(MIN_SDK_KEY) || !values.contains_key(COMPILE_SDK_KEY) {
        let parsed = parse_template_defaults_from_gradle(template_path, template);
        if !values.contains_key(MIN_SDK_KEY) {
            if let Some(value) = parsed.min_sdk {
                values.insert(MIN_SDK_KEY.to_string(), value);
                keys.push(MIN_SDK_KEY.to_string());
            }
        }
        if !values.contains_key(COMPILE_SDK_KEY) {
            if let Some(value) = parsed.compile_sdk {
                values.insert(COMPILE_SDK_KEY.to_string(), value);
                keys.push(COMPILE_SDK_KEY.to_string());
            }
        }
    }

    let min_sdk = normalize_sdk_value(MIN_SDK_KEY, values.get(MIN_SDK_KEY).map(|v| v.as_str()))?;
    let compile_sdk =
        normalize_sdk_value(COMPILE_SDK_KEY, values.get(COMPILE_SDK_KEY).map(|v| v.as_str()))?;
    values.insert(MIN_SDK_KEY.to_string(), min_sdk.clone());
    values.insert(COMPILE_SDK_KEY.to_string(), compile_sdk.clone());

    let mut entries = Vec::new();
    let mut seen = HashMap::new();
    for key in keys {
        if seen.insert(key.clone(), true).is_some() {
            continue;
        }
        if let Some(value) = values.get(&key) {
            entries.push(TemplateDefault {
                key,
                value: value.clone(),
            });
        }
    }

    if !entries.iter().any(|item| item.key == MIN_SDK_KEY) {
        entries.push(TemplateDefault {
            key: MIN_SDK_KEY.to_string(),
            value: min_sdk.clone(),
        });
    }
    if !entries.iter().any(|item| item.key == COMPILE_SDK_KEY) {
        entries.push(TemplateDefault {
            key: COMPILE_SDK_KEY.to_string(),
            value: compile_sdk.clone(),
        });
    }

    Ok(ResolvedTemplateDefaults {
        min_sdk,
        compile_sdk,
        entries,
    })
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

fn cancel_requested(cancel_rx: &watch::Receiver<bool>) -> bool {
    *cancel_rx.borrow()
}

async fn start_job(
    client: &mut JobServiceClient<Channel>,
    job_type: &str,
    params: Vec<KeyValue>,
    correlation_id: &str,
    project_id: Option<Id>,
    run_id: Option<RunId>,
) -> Result<String, Status> {
    let resp = client
        .start_job(StartJobRequest {
            job_type: job_type.into(),
            params,
            project_id,
            target_id: None,
            toolchain_set_id: None,
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
                stream: "project".into(),
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
    detail: ErrorDetail,
) -> Result<(), Status> {
    publish_job_event(client, job_id, JobPayload::Failed(JobFailed { error: Some(detail) })).await
}

fn error_code_for_io(err: &io::Error) -> ErrorCode {
    match err.kind() {
        io::ErrorKind::NotFound => ErrorCode::NotFound,
        io::ErrorKind::AlreadyExists => ErrorCode::AlreadyExists,
        io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        io::ErrorKind::InvalidInput => ErrorCode::InvalidArgument,
        _ => ErrorCode::Internal,
    }
}

fn error_detail_for_io(context: &str, err: &io::Error, job_id: &str) -> ErrorDetail {
    ErrorDetail {
        code: error_code_for_io(err) as i32,
        message: format!("{context} failed"),
        technical_details: err.to_string(),
        remedies: vec![],
        correlation_id: job_id.to_string(),
    }
}

fn error_detail_for_message(code: ErrorCode, message: &str, detail: &str, job_id: &str) -> ErrorDetail {
    ErrorDetail {
        code: code as i32,
        message: message.into(),
        technical_details: detail.into(),
        remedies: vec![],
        correlation_id: job_id.to_string(),
    }
}

fn validate_target_path(path: &Path) -> Result<(), Status> {
    if path.exists() {
        if !path.is_dir() {
            return Err(Status::already_exists("path exists and is not a directory"));
        }
        let mut entries = fs::read_dir(path)
            .map_err(|err| Status::internal(format!("failed to read path: {err}")))?;
        if entries.next().is_some() {
            return Err(Status::already_exists("path exists and is not empty"));
        }
    }
    Ok(())
}

fn read_project_metadata(project_dir: &Path) -> io::Result<Option<ProjectMetadata>> {
    let path = metadata_file_path(project_dir);
    match fs::read_to_string(&path) {
        Ok(data) => {
            let meta: ProjectMetadata = serde_json::from_str(&data)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(Some(meta))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn write_project_metadata(project_dir: &Path, meta: &ProjectMetadata) -> io::Result<()> {
    let path = metadata_file_path(project_dir);
    write_json_atomic(&path, meta)
}

fn count_files(path: &Path, template: &TemplateEntry) -> io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if file_type.is_dir() {
            if template.exclude_dirs.iter().any(|item| item == name.as_ref()) {
                continue;
            }
            total += count_files(&entry.path(), template)?;
        } else if file_type.is_file() {
            if template.exclude_files.iter().any(|item| item == name.as_ref()) {
                continue;
            }
            total += 1;
        }
    }
    Ok(total)
}

fn copy_dir_recursive<F>(
    src: &Path,
    dest: &Path,
    template: &TemplateEntry,
    cancel_flag: &AtomicBool,
    copied: &mut u64,
    total: u64,
    on_file: &mut F,
) -> io::Result<()>
where
    F: FnMut(u64, u64),
{
    for entry in fs::read_dir(src)? {
        if cancel_flag.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "copy cancelled"));
        }
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if file_type.is_dir() {
            if template.exclude_dirs.iter().any(|item| item == name.as_ref()) {
                continue;
            }
            let dest_dir = dest.join(name.as_ref());
            fs::create_dir_all(&dest_dir)?;
            copy_dir_recursive(&entry.path(), &dest_dir, template, cancel_flag, copied, total, on_file)?;
        } else if file_type.is_file() {
            if template.exclude_files.iter().any(|item| item == name.as_ref()) {
                continue;
            }
            fs::copy(entry.path(), dest.join(name.as_ref()))?;
            *copied += 1;
            on_file(*copied, total);
        }
    }
    Ok(())
}

fn copy_template_with_progress(
    src: &Path,
    dest: &Path,
    template: &TemplateEntry,
    tx: mpsc::UnboundedSender<CopyUpdate>,
    cancel_flag: &AtomicBool,
) -> io::Result<u64> {
    let total = count_files(src, template)?;
    let _ = tx.send(CopyUpdate::Total(total));
    fs::create_dir_all(dest)?;
    let mut copied = 0;
    copy_dir_recursive(src, dest, template, cancel_flag, &mut copied, total, &mut |copied, total| {
        let _ = tx.send(CopyUpdate::File { copied, total });
    })?;
    Ok(total)
}

fn project_progress_metrics(
    req: &CreateRequestParts,
    template: &TemplateEntry,
    project_id: &str,
) -> Vec<KeyValue> {
    let mut metrics = vec![
        metric("project_id", project_id),
        metric("project_name", &req.name),
        metric("project_path", req.path.display()),
        metric("template_id", &req.template_id),
        metric("template_name", &template.name),
    ];
    if let Some(run_id) = req
        .run_id
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        metrics.push(metric("run_id", run_id));
    }
    if !req.correlation_id.trim().is_empty() {
        metrics.push(metric("correlation_id", req.correlation_id.trim()));
    }
    if !req.resolved_defaults.min_sdk.trim().is_empty() {
        metrics.push(metric("min_sdk", &req.resolved_defaults.min_sdk));
    }
    if !req.resolved_defaults.compile_sdk.trim().is_empty() {
        metrics.push(metric("compile_sdk", &req.resolved_defaults.compile_sdk));
    }
    if let Some(toolchain_set_id) = req.toolchain_set_id.as_ref() {
        if !toolchain_set_id.trim().is_empty() {
            metrics.push(metric("toolchain_set_id", toolchain_set_id));
        }
    }
    metrics
}

async fn run_create_job(
    mut job_client: JobServiceClient<Channel>,
    state: Arc<Mutex<State>>,
    req: CreateRequestParts,
    template: TemplateEntry,
    template_path: PathBuf,
    project_id: String,
    job_id: String,
) -> Result<(), Status> {
    let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;
    if job_is_cancelled(&mut job_client, &job_id).await {
        let _ = publish_log(&mut job_client, &job_id, "Project creation cancelled before start\n").await;
        return Ok(());
    }

    let cancel_flag = Arc::new(AtomicBool::new(false));
    let cancel_flag_task = cancel_flag.clone();
    let mut cancel_rx_task = cancel_rx.clone();
    tokio::spawn(async move {
        while cancel_rx_task.changed().await.is_ok() {
            if *cancel_rx_task.borrow() {
                cancel_flag_task.store(true, Ordering::Relaxed);
                break;
            }
        }
    });

    publish_state(&mut job_client, &job_id, JobState::Running).await?;
    publish_log(
        &mut job_client,
        &job_id,
        &format!("Creating project '{}'\n", req.name),
    )
    .await?;
    publish_log(
        &mut job_client,
        &job_id,
        &format!("Template: {}\n", template.name),
    )
    .await?;
    publish_log(
        &mut job_client,
        &job_id,
        &format!("Path: {}\n", req.path.display()),
    )
    .await?;

    if !req.path.exists() {
        fs::create_dir_all(&req.path)
            .map_err(|err| Status::internal(format!("failed to create project dir: {err}")))?;
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<CopyUpdate>();
    let copy_src = template_path.clone();
    let copy_dest = req.path.clone();
    let copy_template = template.clone();
    let copy_cancel = cancel_flag.clone();

    let copy_handle = tokio::task::spawn_blocking(move || {
        copy_template_with_progress(&copy_src, &copy_dest, &copy_template, tx, &copy_cancel)
    });

    let base_metrics = project_progress_metrics(&req, &template, &project_id);
    let mut last_percent = 0u32;

    while let Some(update) = rx.recv().await {
        if cancel_requested(&cancel_rx) {
            let _ = publish_log(&mut job_client, &job_id, "Project creation cancelled\n").await;
            break;
        }
        match update {
            CopyUpdate::Total(total) => {
                let _ = publish_log(
                    &mut job_client,
                    &job_id,
                    &format!("Copying {total} files...\n"),
                )
                .await;
            }
            CopyUpdate::File { copied, total } => {
                if total == 0 {
                    continue;
                }
                let percent = ((copied * 100) / total) as u32;
                if percent != last_percent {
                    last_percent = percent;
                    let _ = publish_progress(
                        &mut job_client,
                        &job_id,
                        percent,
                        "Copying template",
                        {
                            let mut metrics = base_metrics.clone();
                            metrics.push(metric("files_copied", copied));
                            metrics.push(metric("files_total", total));
                            metrics
                        },
                    )
                    .await;
                }
            }
        }
    }

    match copy_handle.await {
        Ok(Ok(_)) => {
            if cancel_requested(&cancel_rx) {
                let _ = publish_log(&mut job_client, &job_id, "Project creation cancelled\n").await;
                return Ok(());
            }
        }
        Ok(Err(err)) => {
            if err.kind() == io::ErrorKind::Interrupted || cancel_requested(&cancel_rx) {
                let _ = publish_log(&mut job_client, &job_id, "Project creation cancelled\n").await;
                return Ok(());
            }
            let detail = error_detail_for_io("copy template", &err, &job_id);
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return Err(Status::internal(format!("copy template failed: {err}")));
        }
        Err(err) => {
            if cancel_requested(&cancel_rx) {
                let _ = publish_log(&mut job_client, &job_id, "Project creation cancelled\n").await;
                return Ok(());
            }
            let detail = error_detail_for_message(
                ErrorCode::Internal,
                "copy template failed",
                &format!("join error: {err}"),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return Err(Status::internal(format!("copy task failed: {err}")));
        }
    }

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Project creation cancelled\n").await;
        return Ok(());
    }

    let mut meta = ProjectMetadata::new(
        project_id.clone(),
        req.name.clone(),
        &req.path,
        Some(req.template_id.clone()),
        req.toolchain_set_id.clone(),
        None,
    );
    meta.touch_opened();

    if let Err(err) = write_project_metadata(&req.path, &meta) {
        let detail = error_detail_for_io("write project metadata", &err, &job_id);
        let _ = publish_failed(&mut job_client, &job_id, detail).await;
        return Err(Status::internal(format!("metadata write failed: {err}")));
    }

    {
        let mut st = state.lock().await;
        upsert_recent(&mut st, meta.clone());
        if let Err(err) = save_state(&st) {
            let detail = error_detail_for_io("persist project state", &err, &job_id);
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return Err(Status::internal(format!("state persistence failed: {err}")));
        }
    }

    publish_log(&mut job_client, &job_id, "Project created successfully.\n").await?;

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Project creation cancelled\n").await;
        return Ok(());
    }
    publish_completed(
        &mut job_client,
        &job_id,
        "Project created",
        vec![
            KeyValue {
                key: "project_id".into(),
                value: project_id.clone(),
            },
            KeyValue {
                key: "project_path".into(),
                value: req.path.to_string_lossy().to_string(),
            },
            KeyValue {
                key: "min_sdk".into(),
                value: req.resolved_defaults.min_sdk.clone(),
            },
            KeyValue {
                key: "compile_sdk".into(),
                value: req.resolved_defaults.compile_sdk.clone(),
            },
        ],
    )
    .await?;

    Ok(())
}

#[tonic::async_trait]
impl ProjectService for Svc {
    async fn list_templates(
        &self,
        _request: Request<ListTemplatesRequest>,
    ) -> Result<Response<ListTemplatesResponse>, Status> {
        let registry = TemplateRegistry::load()?;
        Ok(Response::new(ListTemplatesResponse {
            templates: registry.list_with_defaults()?,
        }))
    }

    async fn create_project(
        &self,
        request: Request<CreateProjectRequest>,
    ) -> Result<Response<CreateProjectResponse>, Status> {
        let req = request.into_inner();
        let name = req.name.trim();
        if name.is_empty() {
            return Err(Status::invalid_argument("name is required"));
        }
        let path_str = req.path.trim();
        if path_str.is_empty() {
            return Err(Status::invalid_argument("path is required"));
        }

        let template_id = req
            .template_id
            .and_then(|id| if id.value.trim().is_empty() { None } else { Some(id.value) })
            .ok_or_else(|| Status::invalid_argument("template_id is required"))?;

        let registry = TemplateRegistry::load()?;
        let template = registry
            .find(&template_id)
            .ok_or_else(|| Status::not_found(format!("template not found: {template_id}")))?;

        let project_path = expand_user(path_str);
        validate_target_path(&project_path)?;

        let template_path = registry.resolve_path(&template);
        if !template_path.is_dir() {
            return Err(Status::not_found(format!(
                "template path not found: {}",
                template_path.display()
            )));
        }

        let resolved_defaults =
            resolve_template_defaults(&template, &template_path, &req.params).map_err(|err| {
                Status::failed_precondition(format!("{}: {}", err.message, err.details))
            })?;

        let project_id = format!("proj-{}", Uuid::new_v4());
        let mut job_client = connect_job().await?;
        let job_id = req
            .job_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(String::new);
        let correlation_id = req.correlation_id.trim();
        let params = vec![
            KeyValue {
                key: "template_id".into(),
                value: template_id.clone(),
            },
            KeyValue {
                key: "name".into(),
                value: name.to_string(),
            },
            KeyValue {
                key: "path".into(),
                value: project_path.to_string_lossy().to_string(),
            },
            KeyValue {
                key: "min_sdk".into(),
                value: resolved_defaults.min_sdk.clone(),
            },
            KeyValue {
                key: "compile_sdk".into(),
                value: resolved_defaults.compile_sdk.clone(),
            },
        ];
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "project.create",
                params,
                correlation_id,
                Some(Id {
                    value: project_id.clone(),
                }),
                req.run_id.clone(),
            )
            .await?
        } else {
            job_id
        };

        let create = CreateRequestParts {
            name: name.to_string(),
            path: project_path,
            template_id: template_id.clone(),
            toolchain_set_id: req.toolchain_set_id.map(|id| id.value),
            resolved_defaults,
            run_id: req.run_id.as_ref().map(|id| id.value.clone()),
            correlation_id: correlation_id.to_string(),
        };

        let state = self.state.clone();
        let project_id_for_job = project_id.clone();
        let job_id_for_job = job_id.clone();
        tokio::spawn(async move {
            if let Err(err) = run_create_job(
                job_client,
                state,
                create,
                template,
                template_path,
                project_id_for_job,
                job_id_for_job,
            )
            .await
            {
                eprintln!("create_project job failed: {err}");
            }
        });

        Ok(Response::new(CreateProjectResponse {
            job_id: Some(Id { value: job_id }),
            project_id: Some(Id { value: project_id }),
        }))
    }

    async fn open_project(
        &self,
        request: Request<OpenProjectRequest>,
    ) -> Result<Response<OpenProjectResponse>, Status> {
        let req = request.into_inner();
        let path_str = req.path.trim();
        if path_str.is_empty() {
            return Err(Status::invalid_argument("path is required"));
        }
        let project_path = expand_user(path_str);
        if !project_path.is_dir() {
            return Err(Status::not_found("path does not exist"));
        }

        let mut meta = match read_project_metadata(&project_path) {
            Ok(Some(meta)) => meta,
            Ok(None) => {
                let name = project_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Project")
                    .to_string();
                ProjectMetadata::new(
                    format!("proj-{}", Uuid::new_v4()),
                    name,
                    &project_path,
                    None,
                    None,
                    None,
                )
            }
            Err(err) => {
                return Err(Status::internal(format!(
                    "failed to read project metadata: {err}"
                )))
            }
        };

        meta.path = project_path.to_string_lossy().to_string();
        meta.touch_opened();

        if let Err(err) = write_project_metadata(&project_path, &meta) {
            return Err(Status::internal(format!(
                "failed to write project metadata: {err}"
            )));
        }

        {
            let mut st = self.state.lock().await;
            upsert_recent(&mut st, meta.clone());
            save_state(&st).map_err(|err| {
                Status::internal(format!("failed to persist recent projects: {err}"))
            })?;
        }

        Ok(Response::new(OpenProjectResponse {
            project: Some(meta.to_proto()),
        }))
    }

    async fn get_project(
        &self,
        request: Request<GetProjectRequest>,
    ) -> Result<Response<GetProjectResponse>, Status> {
        let req = request.into_inner();
        let project_id = req
            .project_id
            .map(|id| id.value)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| Status::invalid_argument("project_id is required"))?;

        let st = self.state.lock().await;
        let project = st
            .recent
            .iter()
            .find(|item| item.project_id == project_id)
            .map(|item| item.to_proto());

        match project {
            Some(project) => Ok(Response::new(GetProjectResponse {
                project: Some(project),
            })),
            None => Err(Status::not_found("project_id not found")),
        }
    }

    async fn list_recent_projects(
        &self,
        request: Request<ListRecentProjectsRequest>,
    ) -> Result<Response<ListRecentProjectsResponse>, Status> {
        let req = request.into_inner();
        let page = req.page.unwrap_or_default();
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
        let total = st.recent.len();
        if start >= total && total != 0 {
            return Err(Status::invalid_argument("page_token out of range"));
        }

        let end = (start + page_size).min(total);
        let items = st.recent[start..end]
            .iter()
            .cloned()
            .map(|meta| meta.to_proto())
            .collect::<Vec<_>>();
        let next_token = if end < total {
            end.to_string()
        } else {
            String::new()
        };

        Ok(Response::new(ListRecentProjectsResponse {
            projects: items,
            page_info: Some(PageInfo {
                next_page_token: next_token,
            }),
        }))
    }

    async fn set_project_config(
        &self,
        request: Request<SetProjectConfigRequest>,
    ) -> Result<Response<SetProjectConfigResponse>, Status> {
        let req = request.into_inner();
        let project_id = req
            .project_id
            .map(|id| id.value)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| Status::invalid_argument("project_id is required"))?;

        let toolchain_set_update = req.toolchain_set_id.map(|id| {
            let value = id.value.trim().to_string();
            if value.is_empty() {
                None
            } else {
                Some(value)
            }
        });
        let default_target_update = req.default_target_id.map(|id| {
            let value = id.value.trim().to_string();
            if value.is_empty() {
                None
            } else {
                Some(value)
            }
        });

        if toolchain_set_update.is_none() && default_target_update.is_none() {
            return Err(Status::invalid_argument(
                "toolchain_set_id or default_target_id is required",
            ));
        }

        let mut st = self.state.lock().await;
        let pos = st
            .recent
            .iter()
            .position(|item| item.project_id == project_id)
            .ok_or_else(|| Status::not_found("project not found"))?;

        let project_path = PathBuf::from(&st.recent[pos].path);
        let mut meta = st.recent[pos].clone();
        if let Some(value) = toolchain_set_update {
            meta.toolchain_set_id = value;
        }
        if let Some(value) = default_target_update {
            meta.default_target_id = value;
        }

        if let Err(err) = write_project_metadata(&project_path, &meta) {
            return Err(Status::internal(format!(
                "failed to write project metadata: {err}"
            )));
        }

        st.recent.remove(pos);
        upsert_recent(&mut st, meta);
        save_state(&st)
            .map_err(|err| Status::internal(format!("failed to persist state: {err}")))?;

        Ok(Response::new(SetProjectConfigResponse { ok: true }))
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
        std::env::var("AADK_PROJECT_ADDR").unwrap_or_else(|_| "127.0.0.1:50053".to_string());
    let addr: SocketAddr = addr_str.parse()?;

    let svc = Svc::new();
    info!("aadk-project listening on {}", addr);

    tonic::transport::Server::builder()
        .add_service(ProjectServiceServer::new(svc))
        .serve(addr)
        .await?;

    Ok(())
}
