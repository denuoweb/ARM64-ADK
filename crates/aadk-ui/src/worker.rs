use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use aadk_proto::aadk::v1::{
    build_service_client::BuildServiceClient, job_event::Payload as JobPayload,
    job_service_client::JobServiceClient, observe_service_client::ObserveServiceClient,
    project_service_client::ProjectServiceClient, target_service_client::TargetServiceClient,
    toolchain_service_client::ToolchainServiceClient,
    workflow_service_client::WorkflowServiceClient, Artifact, ArtifactType, BuildRequest,
    CancelJobRequest, CleanupToolchainCacheRequest, CreateProjectRequest,
    CreateToolchainSetRequest, ExportEvidenceBundleRequest, ExportSupportBundleRequest,
    GetActiveToolchainSetRequest, GetCuttlefishStatusRequest, GetDefaultTargetRequest,
    GetJobRequest, Id, InstallApkRequest, InstallCuttlefishRequest, InstallToolchainRequest,
    InstalledToolchain, Job, JobEvent, JobEventKind, JobFilter, JobHistoryFilter, JobState,
    KeyValue, LaunchRequest, ListArtifactsRequest, ListAvailableRequest, ListInstalledRequest,
    ListJobHistoryRequest, ListJobsRequest, ListProvidersRequest, ListRecentProjectsRequest,
    ListRunOutputsRequest, ListRunsRequest, ListTargetsRequest, ListTemplatesRequest,
    ListToolchainSetsRequest, OpenProjectRequest, Pagination, ReloadStateRequest,
    ResolveCuttlefishBuildRequest, RunFilter, RunId, RunOutputFilter, RunOutputKind,
    SetActiveToolchainSetRequest, SetDefaultTargetRequest, SetProjectConfigRequest,
    StartCuttlefishRequest, StartJobRequest, StopCuttlefishRequest, StreamJobEventsRequest,
    StreamLogcatRequest, StreamRunEventsRequest, Timestamp, ToolchainKind,
    UninstallToolchainRequest, UpdateToolchainRequest, VerifyToolchainRequest,
    WorkflowPipelineRequest,
};
use aadk_util::{
    collect_job_history, default_export_path, expand_user, now_millis, open_state_archive,
    save_state_archive_to, state_export_path, StateArchiveOptions, StateOpGuard,
};
use futures_util::StreamExt;
use serde::Serialize;
use tonic::transport::Channel;

use crate::commands::{AppEvent, UiCommand};
use crate::config::{write_json_atomic, AppConfig};
use crate::models::{ProjectTemplateOption, TargetOption, ToolchainSetOption};
use crate::ui_events::UiEventSender;
use crate::utils::{infer_application_id_from_apk_path, parse_list_tokens};

#[derive(Default)]
pub(crate) struct AppState {
    pub(crate) current_job_id: Option<String>,
    home_stream: Option<tokio::task::AbortHandle>,
}

#[derive(Serialize)]
struct UiLogExport {
    exported_at_unix_millis: i64,
    job_id: String,
    config: AppConfig,
    job: Option<JobSummary>,
    events: Vec<LogExportEvent>,
}

#[derive(Serialize)]
struct JobSummary {
    job_id: String,
    job_type: String,
    state: String,
    created_at_unix_millis: i64,
    started_at_unix_millis: Option<i64>,
    finished_at_unix_millis: Option<i64>,
    display_name: String,
    correlation_id: String,
    run_id: String,
    project_id: String,
    target_id: String,
    toolchain_set_id: String,
}

#[derive(Serialize)]
struct LogExportEvent {
    at_unix_millis: i64,
    kind: String,
    summary: String,
    data: Option<String>,
}

fn parse_kv_lines(raw: &str) -> Vec<KeyValue> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let (key, value) = match trimmed.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => (trimmed, ""),
            };
            if key.is_empty() {
                None
            } else {
                Some(KeyValue {
                    key: key.to_string(),
                    value: value.to_string(),
                })
            }
        })
        .collect()
}

fn to_optional_id(raw: &str) -> Option<Id> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(Id {
            value: trimmed.to_string(),
        })
    }
}

fn run_id_from_optional(raw: &str) -> Option<RunId> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(RunId {
            value: trimmed.to_string(),
        })
    }
}

fn kv_pairs(items: &[KeyValue]) -> String {
    items
        .iter()
        .map(|kv| format!("{}={}", kv.key, kv.value))
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_job_states(raw: &str) -> (Vec<i32>, Vec<String>) {
    let mut states = Vec::new();
    let mut unknown = Vec::new();
    for token in parse_list_tokens(raw) {
        let state = match token.to_ascii_lowercase().as_str() {
            "queued" => Some(JobState::Queued),
            "running" => Some(JobState::Running),
            "success" | "succeeded" => Some(JobState::Success),
            "failed" | "failure" => Some(JobState::Failed),
            "cancelled" | "canceled" => Some(JobState::Cancelled),
            _ => None,
        };
        if let Some(state) = state {
            states.push(state as i32);
        } else {
            unknown.push(token);
        }
    }
    (states, unknown)
}

fn parse_job_event_kinds(raw: &str) -> (Vec<i32>, Vec<String>) {
    let mut kinds = Vec::new();
    let mut unknown = Vec::new();
    for token in parse_list_tokens(raw) {
        let kind = match token.to_ascii_lowercase().as_str() {
            "state" | "state_changed" => Some(JobEventKind::StateChanged),
            "progress" => Some(JobEventKind::Progress),
            "log" | "logs" => Some(JobEventKind::Log),
            "completed" | "complete" => Some(JobEventKind::Completed),
            "failed" | "failure" => Some(JobEventKind::Failed),
            _ => None,
        };
        if let Some(kind) = kind {
            kinds.push(kind as i32);
        } else {
            unknown.push(token);
        }
    }
    (kinds, unknown)
}

fn parse_optional_millis(raw: &str) -> Result<Option<i64>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<i64>()
        .map(Some)
        .map_err(|_| format!("invalid unix millis: {trimmed}"))
}

fn latest_installed_by_kind(
    items: &[InstalledToolchain],
    kind: ToolchainKind,
) -> Option<&InstalledToolchain> {
    items
        .iter()
        .filter(|item| {
            item.provider
                .as_ref()
                .map(|provider| provider.kind == kind as i32)
                .unwrap_or(false)
        })
        .max_by_key(|item| {
            item.installed_at
                .as_ref()
                .map(|ts| ts.unix_millis)
                .unwrap_or_default()
        })
}

impl JobSummary {
    fn from_proto(job: &Job) -> Self {
        let state = JobState::try_from(job.state).unwrap_or(JobState::Unspecified);
        Self {
            job_id: job
                .job_id
                .as_ref()
                .map(|id| id.value.clone())
                .unwrap_or_default(),
            job_type: job.job_type.clone(),
            state: format!("{state:?}"),
            created_at_unix_millis: job
                .created_at
                .as_ref()
                .map(|ts| ts.unix_millis)
                .unwrap_or_default(),
            started_at_unix_millis: job.started_at.as_ref().map(|ts| ts.unix_millis),
            finished_at_unix_millis: job.finished_at.as_ref().map(|ts| ts.unix_millis),
            display_name: job.display_name.clone(),
            correlation_id: job.correlation_id.clone(),
            run_id: job
                .run_id
                .as_ref()
                .map(|id| id.value.clone())
                .unwrap_or_default(),
            project_id: job
                .project_id
                .as_ref()
                .map(|id| id.value.clone())
                .unwrap_or_default(),
            target_id: job
                .target_id
                .as_ref()
                .map(|id| id.value.clone())
                .unwrap_or_default(),
            toolchain_set_id: job
                .toolchain_set_id
                .as_ref()
                .map(|id| id.value.clone())
                .unwrap_or_default(),
        }
    }
}

impl LogExportEvent {
    fn from_proto(event: &JobEvent) -> Self {
        let at_unix_millis = event
            .at
            .as_ref()
            .map(|ts| ts.unix_millis)
            .unwrap_or_default();
        match event.payload.as_ref() {
            Some(JobPayload::StateChanged(state)) => {
                let state = JobState::try_from(state.new_state).unwrap_or(JobState::Unspecified);
                Self {
                    at_unix_millis,
                    kind: "state_changed".into(),
                    summary: format!("{state:?}"),
                    data: None,
                }
            }
            Some(JobPayload::Progress(progress)) => {
                if let Some(p) = progress.progress.as_ref() {
                    let metrics = kv_pairs(&p.metrics);
                    let summary = if metrics.is_empty() {
                        format!("{}% {}", p.percent, p.phase)
                    } else {
                        format!("{}% {} ({metrics})", p.percent, p.phase)
                    };
                    Self {
                        at_unix_millis,
                        kind: "progress".into(),
                        summary,
                        data: None,
                    }
                } else {
                    Self {
                        at_unix_millis,
                        kind: "progress".into(),
                        summary: "missing progress payload".into(),
                        data: None,
                    }
                }
            }
            Some(JobPayload::Log(log)) => {
                if let Some(chunk) = log.chunk.as_ref() {
                    let summary = format!("stream={} truncated={}", chunk.stream, chunk.truncated);
                    let data = Some(String::from_utf8_lossy(&chunk.data).to_string());
                    Self {
                        at_unix_millis,
                        kind: "log".into(),
                        summary,
                        data,
                    }
                } else {
                    Self {
                        at_unix_millis,
                        kind: "log".into(),
                        summary: "missing log chunk".into(),
                        data: None,
                    }
                }
            }
            Some(JobPayload::Completed(completed)) => {
                let outputs = kv_pairs(&completed.outputs);
                let summary = if outputs.is_empty() {
                    completed.summary.clone()
                } else {
                    format!("{} ({outputs})", completed.summary)
                };
                Self {
                    at_unix_millis,
                    kind: "completed".into(),
                    summary,
                    data: None,
                }
            }
            Some(JobPayload::Failed(failed)) => {
                let summary = failed
                    .error
                    .as_ref()
                    .map(|err| format!("{} ({})", err.message, err.code))
                    .unwrap_or_else(|| "failed".into());
                Self {
                    at_unix_millis,
                    kind: "failed".into(),
                    summary,
                    data: None,
                }
            }
            None => Self {
                at_unix_millis,
                kind: "unknown".into(),
                summary: "missing payload".into(),
                data: None,
            },
        }
    }
}

fn job_event_lines(event: &JobEvent) -> Vec<String> {
    let export = LogExportEvent::from_proto(event);
    let mut lines = Vec::new();
    lines.push(format!(
        "{} {}: {}\n",
        export.at_unix_millis, export.kind, export.summary
    ));
    if let Some(data) = export.data {
        if data.ends_with('\n') {
            lines.push(data);
        } else {
            lines.push(format!("{data}\n"));
        }
    }
    lines
}

fn stream_job_event_lines(job_id: &str, event: &JobEvent) -> Vec<String> {
    match event.payload.as_ref() {
        Some(JobPayload::Log(log)) => {
            if let Some(chunk) = log.chunk.as_ref() {
                let mut text = String::from_utf8_lossy(&chunk.data).to_string();
                if text.is_empty() {
                    return vec![format!(
                        "job {job_id}: log: stream={} truncated={}\n",
                        chunk.stream, chunk.truncated
                    )];
                }
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                vec![text]
            } else {
                vec![format!("job {job_id}: log: missing chunk\n")]
            }
        }
        _ => {
            let export = LogExportEvent::from_proto(event);
            vec![format!(
                "job {job_id}: {} {}: {}\n",
                export.at_unix_millis, export.kind, export.summary
            )]
        }
    }
}

fn run_stream_lines(event: &JobEvent) -> Vec<String> {
    let job_id = event
        .job_id
        .as_ref()
        .map(|id| id.value.as_str())
        .unwrap_or("-");
    match event.payload.as_ref() {
        Some(JobPayload::Log(log)) => {
            if let Some(chunk) = log.chunk.as_ref() {
                let mut text = String::from_utf8_lossy(&chunk.data).to_string();
                if text.is_empty() {
                    return vec![format!(
                        "job {job_id}: log: stream={} truncated={}\n",
                        chunk.stream, chunk.truncated
                    )];
                }
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                vec![
                    format!(
                        "job {job_id}: log: stream={} truncated={}\n",
                        chunk.stream, chunk.truncated
                    ),
                    text,
                ]
            } else {
                vec![format!("job {job_id}: log: missing chunk\n")]
            }
        }
        _ => stream_job_event_lines(job_id, event),
    }
}

fn format_job_row(job: &Job) -> String {
    let job_id = job
        .job_id
        .as_ref()
        .map(|id| id.value.as_str())
        .unwrap_or("-");
    let state = JobState::try_from(job.state).unwrap_or(JobState::Unspecified);
    let created = job
        .created_at
        .as_ref()
        .map(|ts| ts.unix_millis)
        .unwrap_or_default();
    let finished = job
        .finished_at
        .as_ref()
        .map(|ts| ts.unix_millis)
        .unwrap_or_default();
    let run_id = job
        .run_id
        .as_ref()
        .map(|id| id.value.as_str())
        .unwrap_or("-");
    let correlation_id = if job.correlation_id.trim().is_empty() {
        "-"
    } else {
        job.correlation_id.as_str()
    };
    format!(
        "- {} type={} state={:?} run_id={} corr={} created={} finished={} display={}\n",
        job_id, job.job_type, state, run_id, correlation_id, created, finished, job.display_name
    )
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

fn run_output_kind_label(kind: i32) -> &'static str {
    match RunOutputKind::try_from(kind).unwrap_or(RunOutputKind::Unspecified) {
        RunOutputKind::Bundle => "bundle",
        RunOutputKind::Artifact => "artifact",
        RunOutputKind::Unspecified => "unspecified",
    }
}

fn metadata_value<'a>(items: &'a [KeyValue], key: &str) -> Option<&'a str> {
    items
        .iter()
        .find(|item| item.key == key)
        .map(|item| item.value.as_str())
}

fn format_artifact_line(artifact: &Artifact) -> String {
    let kind = ArtifactType::try_from(artifact.r#type).unwrap_or(ArtifactType::Unspecified);
    let module = metadata_value(&artifact.metadata, "module").unwrap_or("-");
    let variant = metadata_value(&artifact.metadata, "variant").unwrap_or("-");
    let task = metadata_value(&artifact.metadata, "task").unwrap_or("-");
    format!(
        "- [{}] {} module={} variant={} task={} size={} path={}\n",
        artifact_type_label(kind),
        artifact.name,
        module,
        variant,
        task,
        artifact.size_bytes,
        artifact.path
    )
}

fn job_state_label(job: &Job) -> String {
    let state = JobState::try_from(job.state).unwrap_or(JobState::Unspecified);
    format!("{state:?}")
}

fn format_active_job(job: &Job) -> String {
    let job_id = job
        .job_id
        .as_ref()
        .map(|id| id.value.as_str())
        .unwrap_or("-");
    let job_type = if job.job_type.trim().is_empty() {
        "-"
    } else {
        job.job_type.as_str()
    };
    let created_at = job
        .created_at
        .as_ref()
        .map(|ts| ts.unix_millis)
        .unwrap_or_default();
    format!(
        "latest job is {} job_id={job_id} job_type={job_type} created={created_at}",
        job_state_label(job)
    )
}

async fn latest_active_job(addr: &str) -> Result<Option<Job>, Box<dyn std::error::Error>> {
    let mut client = JobServiceClient::new(connect(addr).await?);
    let resp = client
        .list_jobs(ListJobsRequest {
            page: Some(Pagination {
                page_size: 50,
                page_token: String::new(),
            }),
            filter: Some(JobFilter {
                job_types: vec![],
                states: vec![JobState::Queued as i32, JobState::Running as i32],
                created_after: None,
                created_before: None,
                finished_after: None,
                finished_before: None,
                correlation_id: String::new(),
                run_id: None,
            }),
        })
        .await?
        .into_inner();
    let latest = resp.jobs.into_iter().max_by_key(|job| {
        job.created_at
            .as_ref()
            .map(|ts| ts.unix_millis)
            .unwrap_or(0)
    });
    Ok(latest)
}

fn format_state_exclusions(opts: &StateArchiveOptions) -> String {
    let mut excluded = Vec::new();
    if opts.exclude_downloads {
        excluded.push("downloads");
    }
    if opts.exclude_toolchains {
        excluded.push("toolchains");
    }
    if opts.exclude_bundles {
        excluded.push("bundles");
    }
    if opts.exclude_telemetry {
        excluded.push("telemetry");
    }
    excluded.push("state-exports");
    excluded.push("state-ops");
    excluded.join(", ")
}

fn log_reload_result(
    ui: &UiEventSender,
    name: &str,
    addr: &str,
    resp: &aadk_proto::aadk::v1::ReloadStateResponse,
) {
    let detail = if resp.detail.trim().is_empty() {
        "-"
    } else {
        resp.detail.as_str()
    };
    ui.send(AppEvent::Log {
        page: "settings",
        line: format!(
            "  {name}: ok={} items={} addr={addr} detail={detail}\n",
            resp.ok, resp.item_count
        ),
    })
    .ok();
}

async fn reload_job_state(
    addr: &str,
) -> Result<aadk_proto::aadk::v1::ReloadStateResponse, Box<dyn std::error::Error>> {
    let mut client = JobServiceClient::new(connect(addr).await?);
    let resp = client
        .reload_state(ReloadStateRequest {})
        .await?
        .into_inner();
    Ok(resp)
}

async fn reload_toolchain_state(
    addr: &str,
) -> Result<aadk_proto::aadk::v1::ReloadStateResponse, Box<dyn std::error::Error>> {
    let mut client = ToolchainServiceClient::new(connect(addr).await?);
    let resp = client
        .reload_state(ReloadStateRequest {})
        .await?
        .into_inner();
    Ok(resp)
}

async fn reload_project_state(
    addr: &str,
) -> Result<aadk_proto::aadk::v1::ReloadStateResponse, Box<dyn std::error::Error>> {
    let mut client = ProjectServiceClient::new(connect(addr).await?);
    let resp = client
        .reload_state(ReloadStateRequest {})
        .await?
        .into_inner();
    Ok(resp)
}

async fn reload_build_state(
    addr: &str,
) -> Result<aadk_proto::aadk::v1::ReloadStateResponse, Box<dyn std::error::Error>> {
    let mut client = BuildServiceClient::new(connect(addr).await?);
    let resp = client
        .reload_state(ReloadStateRequest {})
        .await?
        .into_inner();
    Ok(resp)
}

async fn reload_targets_state(
    addr: &str,
) -> Result<aadk_proto::aadk::v1::ReloadStateResponse, Box<dyn std::error::Error>> {
    let mut client = TargetServiceClient::new(connect(addr).await?);
    let resp = client
        .reload_state(ReloadStateRequest {})
        .await?
        .into_inner();
    Ok(resp)
}

async fn reload_observe_state(
    addr: &str,
) -> Result<aadk_proto::aadk::v1::ReloadStateResponse, Box<dyn std::error::Error>> {
    let mut client = ObserveServiceClient::new(connect(addr).await?);
    let resp = client
        .reload_state(ReloadStateRequest {})
        .await?
        .into_inner();
    Ok(resp)
}

async fn reload_workflow_state(
    addr: &str,
) -> Result<aadk_proto::aadk::v1::ReloadStateResponse, Box<dyn std::error::Error>> {
    let mut client = WorkflowServiceClient::new(connect(addr).await?);
    let resp = client
        .reload_state(ReloadStateRequest {})
        .await?
        .into_inner();
    Ok(resp)
}

async fn reload_all_state(
    cfg: &AppConfig,
    ui: &UiEventSender,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut errors = Vec::new();

    match reload_job_state(&cfg.job_addr).await {
        Ok(resp) => log_reload_result(ui, "job", &cfg.job_addr, &resp),
        Err(err) => errors.push(format!("job: {err}")),
    }
    match reload_toolchain_state(&cfg.toolchain_addr).await {
        Ok(resp) => log_reload_result(ui, "toolchain", &cfg.toolchain_addr, &resp),
        Err(err) => errors.push(format!("toolchain: {err}")),
    }
    match reload_project_state(&cfg.project_addr).await {
        Ok(resp) => log_reload_result(ui, "project", &cfg.project_addr, &resp),
        Err(err) => errors.push(format!("project: {err}")),
    }
    match reload_build_state(&cfg.build_addr).await {
        Ok(resp) => log_reload_result(ui, "build", &cfg.build_addr, &resp),
        Err(err) => errors.push(format!("build: {err}")),
    }
    match reload_targets_state(&cfg.targets_addr).await {
        Ok(resp) => log_reload_result(ui, "targets", &cfg.targets_addr, &resp),
        Err(err) => errors.push(format!("targets: {err}")),
    }
    match reload_observe_state(&cfg.observe_addr).await {
        Ok(resp) => log_reload_result(ui, "observe", &cfg.observe_addr, &resp),
        Err(err) => errors.push(format!("observe: {err}")),
    }
    match reload_workflow_state(&cfg.workflow_addr).await {
        Ok(resp) => log_reload_result(ui, "workflow", &cfg.workflow_addr, &resp),
        Err(err) => errors.push(format!("workflow: {err}")),
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("reload failures: {}", errors.join("; ")).into())
    }
}

pub(crate) async fn handle_command(
    cmd: UiCommand,
    worker_state: &mut AppState,
    ui: UiEventSender,
    stream_tasks: &mut tokio::task::JoinSet<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        UiCommand::HomeStartJob {
            cfg,
            job_type,
            params_raw,
            project_id,
            target_id,
            toolchain_set_id,
            correlation_id,
        } => {
            ui.send(AppEvent::HomeResetStatus).ok();
            ui.send(AppEvent::Log {
                page: "home",
                line: format!("Connecting to JobService at {}\n", cfg.job_addr),
            })
            .ok();

            let job_type = job_type.trim().to_string();
            if job_type.is_empty() {
                ui.send(AppEvent::Log {
                    page: "home",
                    line: "job_type is required\n".into(),
                })
                .ok();
                return Ok(());
            }

            let mut client = JobServiceClient::new(connect(&cfg.job_addr).await?);
            let resp = client
                .start_job(StartJobRequest {
                    job_type: job_type.clone(),
                    params: parse_kv_lines(&params_raw),
                    project_id: to_optional_id(&project_id),
                    target_id: to_optional_id(&target_id),
                    toolchain_set_id: to_optional_id(&toolchain_set_id),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await?
                .into_inner();

            let job_id = resp
                .job
                .and_then(|r| r.job_id)
                .map(|i| i.value)
                .unwrap_or_default();
            if job_id.is_empty() {
                ui.send(AppEvent::Log {
                    page: "home",
                    line: "StartJob returned empty job_id\n".into(),
                })
                .ok();
                return Ok(());
            }
            worker_state.current_job_id = Some(job_id.clone());
            ui.send(AppEvent::SetCurrentJob {
                job_id: Some(job_id.clone()),
            })
            .ok();
            ui.send(AppEvent::Log {
                page: "home",
                line: format!("Started job: {job_id} ({job_type})\n"),
            })
            .ok();
            ui.send(AppEvent::HomeState {
                state: "Queued".into(),
            })
            .ok();
            start_home_stream(
                worker_state,
                stream_tasks,
                cfg.job_addr.clone(),
                job_id.clone(),
                ui.clone(),
            );
        }

        UiCommand::HomeWatchJob { cfg, job_id } => {
            let job_id = job_id.trim().to_string();
            if job_id.is_empty() {
                ui.send(AppEvent::Log {
                    page: "home",
                    line: "job_id is required to watch\n".into(),
                })
                .ok();
                return Ok(());
            }
            ui.send(AppEvent::HomeResetStatus).ok();
            worker_state.current_job_id = Some(job_id.clone());
            ui.send(AppEvent::SetCurrentJob {
                job_id: Some(job_id.clone()),
            })
            .ok();
            ui.send(AppEvent::Log {
                page: "home",
                line: format!("Watching job: {job_id}\n"),
            })
            .ok();
            start_home_stream(
                worker_state,
                stream_tasks,
                cfg.job_addr.clone(),
                job_id.clone(),
                ui.clone(),
            );
        }

        UiCommand::HomeCancelCurrent { cfg } => {
            if let Some(job_id) = worker_state.current_job_id.clone() {
                ui.send(AppEvent::Log {
                    page: "home",
                    line: format!("Cancelling job: {job_id}\n"),
                })
                .ok();
                let mut client = JobServiceClient::new(connect(&cfg.job_addr).await?);
                let resp = client
                    .cancel_job(CancelJobRequest {
                        job_id: Some(Id {
                            value: job_id.clone(),
                        }),
                    })
                    .await?
                    .into_inner();
                ui.send(AppEvent::Log {
                    page: "home",
                    line: format!("Cancel accepted: {}\n", resp.accepted),
                })
                .ok();
            }
        }

        UiCommand::JobsList {
            cfg,
            job_types,
            states,
            created_after,
            created_before,
            finished_after,
            finished_before,
            correlation_id,
            run_id,
            page_size,
            page_token,
            page,
        } => {
            ui.send(AppEvent::Log {
                page,
                line: format!("Listing jobs via {}\n", cfg.job_addr),
            })
            .ok();
            let (state_filters, unknown_states) = parse_job_states(&states);
            if !unknown_states.is_empty() {
                ui.send(AppEvent::Log {
                    page,
                    line: format!("Unknown states: {}\n", unknown_states.join(", ")),
                })
                .ok();
            }
            let created_after = match parse_optional_millis(&created_after) {
                Ok(value) => value,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "jobs",
                        line: format!("{err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let created_before = match parse_optional_millis(&created_before) {
                Ok(value) => value,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "jobs",
                        line: format!("{err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let finished_after = match parse_optional_millis(&finished_after) {
                Ok(value) => value,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "jobs",
                        line: format!("{err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let finished_before = match parse_optional_millis(&finished_before) {
                Ok(value) => value,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "jobs",
                        line: format!("{err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            let filter = JobFilter {
                job_types: parse_list_tokens(&job_types),
                states: state_filters,
                created_after: created_after.map(|ms| Timestamp { unix_millis: ms }),
                created_before: created_before.map(|ms| Timestamp { unix_millis: ms }),
                finished_after: finished_after.map(|ms| Timestamp { unix_millis: ms }),
                finished_before: finished_before.map(|ms| Timestamp { unix_millis: ms }),
                correlation_id: correlation_id.trim().to_string(),
                run_id: run_id_from_optional(&run_id),
            };

            let mut client = JobServiceClient::new(connect(&cfg.job_addr).await?);
            let resp = client
                .list_jobs(ListJobsRequest {
                    page: Some(Pagination {
                        page_size: page_size.max(1),
                        page_token,
                    }),
                    filter: Some(filter),
                })
                .await?
                .into_inner();

            if resp.jobs.is_empty() {
                ui.send(AppEvent::Log {
                    page,
                    line: "No jobs found.\n".into(),
                })
                .ok();
            } else if page == "jobs" {
                ui.send(AppEvent::Log {
                    page,
                    line: format!("Jobs ({})\n", resp.jobs.len()),
                })
                .ok();
                for job in &resp.jobs {
                    ui.send(AppEvent::Log {
                        page,
                        line: format_job_row(job),
                    })
                    .ok();
                }
            } else {
                let mut grouped: BTreeMap<String, Vec<&Job>> = BTreeMap::new();
                for job in &resp.jobs {
                    let run_key = job
                        .run_id
                        .as_ref()
                        .map(|id| id.value.clone())
                        .filter(|value| !value.trim().is_empty())
                        .or_else(|| {
                            let corr = job.correlation_id.trim();
                            if corr.is_empty() {
                                None
                            } else {
                                Some(format!("correlation_id={corr}"))
                            }
                        })
                        .unwrap_or_else(|| "unassigned".to_string());
                    grouped.entry(run_key).or_default().push(job);
                }
                ui.send(AppEvent::Log {
                    page,
                    line: format!("Runs ({})\n", grouped.len()),
                })
                .ok();
                for (run_key, jobs) in grouped {
                    ui.send(AppEvent::Log {
                        page,
                        line: format!("run={run_key} jobs={}\n", jobs.len()),
                    })
                    .ok();
                    for job in jobs {
                        ui.send(AppEvent::Log {
                            page,
                            line: format_job_row(job),
                        })
                        .ok();
                    }
                }
            }
            if let Some(page_info) = resp.page_info {
                if !page_info.next_page_token.is_empty() {
                    ui.send(AppEvent::Log {
                        page,
                        line: format!("next_page_token={}\n", page_info.next_page_token),
                    })
                    .ok();
                }
            }
        }

        UiCommand::JobsHistory {
            cfg,
            job_id,
            kinds,
            after,
            before,
            page_size,
            page_token,
        } => {
            let job_id = job_id.trim().to_string();
            if job_id.is_empty() {
                ui.send(AppEvent::Log {
                    page: "jobs",
                    line: "job_id is required for history\n".into(),
                })
                .ok();
                return Ok(());
            }
            ui.send(AppEvent::Log {
                page: "jobs",
                line: format!("Listing job history for {job_id} via {}\n", cfg.job_addr),
            })
            .ok();
            let (kind_filters, unknown_kinds) = parse_job_event_kinds(&kinds);
            if !unknown_kinds.is_empty() {
                ui.send(AppEvent::Log {
                    page: "jobs",
                    line: format!("Unknown kinds: {}\n", unknown_kinds.join(", ")),
                })
                .ok();
            }
            let after = match parse_optional_millis(&after) {
                Ok(value) => value,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "jobs",
                        line: format!("{err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let before = match parse_optional_millis(&before) {
                Ok(value) => value,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "jobs",
                        line: format!("{err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            let filter = JobHistoryFilter {
                kinds: kind_filters,
                after: after.map(|ms| Timestamp { unix_millis: ms }),
                before: before.map(|ms| Timestamp { unix_millis: ms }),
            };

            let channel = match connect(&cfg.job_addr).await {
                Ok(channel) => channel,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "jobs",
                        line: format!("JobService connection failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let mut client = JobServiceClient::new(channel);
            let resp = match client
                .list_job_history(ListJobHistoryRequest {
                    job_id: Some(Id {
                        value: job_id.clone(),
                    }),
                    page: Some(Pagination {
                        page_size: page_size.max(1),
                        page_token,
                    }),
                    filter: Some(filter),
                })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "jobs",
                        line: format!("List job history failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            if resp.events.is_empty() {
                ui.send(AppEvent::Log {
                    page: "jobs",
                    line: "No events found.\n".into(),
                })
                .ok();
            } else {
                ui.send(AppEvent::Log {
                    page: "jobs",
                    line: format!("Events ({})\n", resp.events.len()),
                })
                .ok();
                for event in &resp.events {
                    for line in job_event_lines(event) {
                        ui.send(AppEvent::Log { page: "jobs", line }).ok();
                    }
                }
            }
            if let Some(page_info) = resp.page_info {
                if !page_info.next_page_token.is_empty() {
                    ui.send(AppEvent::Log {
                        page: "jobs",
                        line: format!("next_page_token={}\n", page_info.next_page_token),
                    })
                    .ok();
                }
            }
        }

        UiCommand::JobsExportLogs {
            cfg,
            job_id,
            output_path,
            page,
        } => {
            let job_id = job_id.trim().to_string();
            if job_id.is_empty() {
                ui.send(AppEvent::Log {
                    page,
                    line: "job_id is required for export\n".into(),
                })
                .ok();
                return Ok(());
            }
            ui.send(AppEvent::Log {
                page,
                line: format!("Exporting logs for {job_id}\n"),
            })
            .ok();

            let path = if output_path.trim().is_empty() {
                default_export_path("ui-job-export", &job_id)
            } else {
                PathBuf::from(output_path)
            };

            let mut client = JobServiceClient::new(connect(&cfg.job_addr).await?);
            let job = match client
                .get_job(GetJobRequest {
                    job_id: Some(Id {
                        value: job_id.clone(),
                    }),
                })
                .await
            {
                Ok(resp) => resp.into_inner().job,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page,
                        line: format!("GetJob failed: {err}\n"),
                    })
                    .ok();
                    None
                }
            };
            let events = collect_job_history(&mut client, &job_id).await?;
            let export = UiLogExport {
                exported_at_unix_millis: now_millis(),
                job_id: job_id.clone(),
                config: cfg.clone(),
                job: job.as_ref().map(JobSummary::from_proto),
                events: events.iter().map(LogExportEvent::from_proto).collect(),
            };
            write_json_atomic(&path, &export)?;
            ui.send(AppEvent::Log {
                page,
                line: format!("Exported logs to {}\n", path.display()),
            })
            .ok();
        }

        UiCommand::ToolchainListProviders { cfg } => {
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!("Connecting to ToolchainService at {}\n", cfg.toolchain_addr),
            })
            .ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client
                .list_providers(ListProvidersRequest {})
                .await?
                .into_inner();

            ui.send(AppEvent::Log {
                page: "toolchains",
                line: "Providers:\n".into(),
            })
            .ok();
            for p in resp.providers {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: format!("- {} (kind={})\n", p.name, p.kind),
                })
                .ok();
            }
        }

        UiCommand::ToolchainListAvailable { cfg, provider_id } => {
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!(
                    "Listing available for {provider_id} via {}\n",
                    cfg.toolchain_addr
                ),
            })
            .ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let provider_id_value = provider_id.clone();
            let resp = client
                .list_available(ListAvailableRequest {
                    provider_id: Some(Id {
                        value: provider_id.clone(),
                    }),
                    page: None,
                })
                .await?
                .into_inner();
            let versions = resp
                .items
                .iter()
                .filter_map(|item| item.version.as_ref().map(|v| v.version.clone()))
                .collect::<Vec<_>>();
            ui.send(AppEvent::ToolchainAvailable {
                provider_id: provider_id_value,
                versions,
            })
            .ok();

            if resp.items.is_empty() {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "No available toolchains.\n".into(),
                })
                .ok();
            } else {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "Available:\n".into(),
                })
                .ok();
                for item in resp.items {
                    let name = item
                        .provider
                        .as_ref()
                        .map(|p| p.name.as_str())
                        .unwrap_or("unknown");
                    let version = item
                        .version
                        .as_ref()
                        .map(|v| v.version.as_str())
                        .unwrap_or("unknown");
                    let url = item.artifact.as_ref().map(|a| a.url.as_str()).unwrap_or("");
                    let sha = item
                        .artifact
                        .as_ref()
                        .map(|a| a.sha256.as_str())
                        .unwrap_or("");
                    let size = item.artifact.as_ref().map(|a| a.size_bytes).unwrap_or(0);
                    ui.send(AppEvent::Log { page: "toolchains", line: format!("- {name} {version}\n  url={url}\n  sha256={sha}\n  size_bytes={size}\n") }).ok();
                }
            }
        }

        UiCommand::ToolchainInstall {
            cfg,
            provider_id,
            version,
            verify,
            job_id,
            correlation_id,
        } => {
            let provider_id = provider_id.trim().to_string();
            if provider_id.is_empty() {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "Toolchain install requires a provider.\n".into(),
                })
                .ok();
                return Ok(());
            }
            let version = version.trim().to_string();
            if version.is_empty() {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "Toolchain install requires a version.\n".into(),
                })
                .ok();
                return Ok(());
            }
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!("Installing {provider_id} {version} (verify={verify})\n"),
            })
            .ok();
            let channel = match connect(&cfg.toolchain_addr).await {
                Ok(channel) => channel,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: format!("ToolchainService connection failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let mut client = ToolchainServiceClient::new(channel);
            let resp = match client
                .install_toolchain(InstallToolchainRequest {
                    provider_id: Some(Id { value: provider_id }),
                    version,
                    install_root: "".into(),
                    verify_hash: verify,
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: format!("Install toolchain failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!("Install job queued: {job_id}\n"),
            })
            .ok();
            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                stream_tasks.spawn(async move {
                    if let Err(err) =
                        stream_job_events(job_addr, job_id.clone(), "toolchains", ui_stream).await
                    {
                        let _ = ui_err.send(AppEvent::Log {
                            page: "toolchains",
                            line: format!("job stream error ({job_id}): {err}\n"),
                        });
                    }
                });
            }
        }

        UiCommand::ToolchainUpdate {
            cfg,
            toolchain_id,
            version,
            verify,
            remove_cached,
            job_id,
            correlation_id,
        } => {
            let toolchain_id = toolchain_id.trim().to_string();
            let version = version.trim().to_string();
            if toolchain_id.is_empty() || version.is_empty() {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "Toolchain update requires toolchain id and version.\n".into(),
                })
                .ok();
                return Ok(());
            }
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!("Updating {toolchain_id} to {version} (verify={verify})\n"),
            })
            .ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client
                .update_toolchain(UpdateToolchainRequest {
                    toolchain_id: Some(Id {
                        value: toolchain_id,
                    }),
                    version,
                    verify_hash: verify,
                    remove_cached_artifact: remove_cached,
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await?
                .into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!("Update job queued: {job_id}\n"),
            })
            .ok();
            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                stream_tasks.spawn(async move {
                    if let Err(err) =
                        stream_job_events(job_addr, job_id.clone(), "toolchains", ui_stream).await
                    {
                        let _ = ui_err.send(AppEvent::Log {
                            page: "toolchains",
                            line: format!("job stream error ({job_id}): {err}\n"),
                        });
                    }
                });
            }
        }

        UiCommand::ToolchainUninstall {
            cfg,
            toolchain_id,
            remove_cached,
            force,
            job_id,
            correlation_id,
        } => {
            let toolchain_id = toolchain_id.trim().to_string();
            if toolchain_id.is_empty() {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "Toolchain uninstall requires toolchain id.\n".into(),
                })
                .ok();
                return Ok(());
            }
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!("Uninstalling {toolchain_id} (force={force})\n"),
            })
            .ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client
                .uninstall_toolchain(UninstallToolchainRequest {
                    toolchain_id: Some(Id {
                        value: toolchain_id,
                    }),
                    remove_cached_artifact: remove_cached,
                    force,
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await?
                .into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!("Uninstall job queued: {job_id}\n"),
            })
            .ok();
            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                stream_tasks.spawn(async move {
                    if let Err(err) =
                        stream_job_events(job_addr, job_id.clone(), "toolchains", ui_stream).await
                    {
                        let _ = ui_err.send(AppEvent::Log {
                            page: "toolchains",
                            line: format!("job stream error ({job_id}): {err}\n"),
                        });
                    }
                });
            }
        }

        UiCommand::ToolchainCleanupCache {
            cfg,
            dry_run,
            remove_all,
            job_id,
            correlation_id,
        } => {
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!(
                    "Cleaning cache via {} (dry_run={dry_run}, remove_all={remove_all})\n",
                    cfg.toolchain_addr
                ),
            })
            .ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client
                .cleanup_toolchain_cache(CleanupToolchainCacheRequest {
                    dry_run,
                    remove_all,
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await?
                .into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!("Cleanup job queued: {job_id}\n"),
            })
            .ok();
            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                stream_tasks.spawn(async move {
                    if let Err(err) =
                        stream_job_events(job_addr, job_id.clone(), "toolchains", ui_stream).await
                    {
                        let _ = ui_err.send(AppEvent::Log {
                            page: "toolchains",
                            line: format!("job stream error ({job_id}): {err}\n"),
                        });
                    }
                });
            }
        }

        UiCommand::ToolchainListInstalled { cfg, kind } => {
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!("Listing installed toolchains via {}\n", cfg.toolchain_addr),
            })
            .ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client
                .list_installed(ListInstalledRequest { kind: kind as i32 })
                .await?
                .into_inner();

            if resp.items.is_empty() {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "No installed toolchains.\n".into(),
                })
                .ok();
            } else {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "Installed:\n".into(),
                })
                .ok();
                for item in resp.items {
                    let id = item
                        .toolchain_id
                        .as_ref()
                        .map(|i| i.value.as_str())
                        .unwrap_or("");
                    let name = item
                        .provider
                        .as_ref()
                        .map(|p| p.name.as_str())
                        .unwrap_or("unknown");
                    let version = item
                        .version
                        .as_ref()
                        .map(|v| v.version.as_str())
                        .unwrap_or("unknown");
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: format!(
                            "- {id} {name} {version} verified={}\n  path={}\n",
                            item.verified, item.install_path
                        ),
                    })
                    .ok();
                }
            }
        }

        UiCommand::ToolchainListSets { cfg } => {
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!("Listing toolchain sets via {}\n", cfg.toolchain_addr),
            })
            .ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client
                .list_toolchain_sets(ListToolchainSetsRequest { page: None })
                .await?
                .into_inner();

            if resp.sets.is_empty() {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "No toolchain sets found.\n".into(),
                })
                .ok();
            } else {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "Toolchain sets:\n".into(),
                })
                .ok();
                for set in resp.sets {
                    let set_id = set
                        .toolchain_set_id
                        .as_ref()
                        .map(|id| id.value.as_str())
                        .unwrap_or("");
                    let sdk = set
                        .sdk_toolchain_id
                        .as_ref()
                        .map(|id| id.value.as_str())
                        .unwrap_or("");
                    let ndk = set
                        .ndk_toolchain_id
                        .as_ref()
                        .map(|id| id.value.as_str())
                        .unwrap_or("");
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: format!(
                            "- {} ({})\n  sdk={}\n  ndk={}\n",
                            set.display_name, set_id, sdk, ndk
                        ),
                    })
                    .ok();
                }
            }

            if let Some(page_info) = resp.page_info {
                if !page_info.next_page_token.trim().is_empty() {
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: format!("next_page_token={}\n", page_info.next_page_token),
                    })
                    .ok();
                }
            }
        }

        UiCommand::ToolchainVerifyInstalled {
            cfg,
            job_id,
            correlation_id,
        } => {
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!(
                    "Verifying installed toolchains via {}\n",
                    cfg.toolchain_addr
                ),
            })
            .ok();
            let channel = match connect(&cfg.toolchain_addr).await {
                Ok(channel) => channel,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: format!("ToolchainService connection failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let mut client = ToolchainServiceClient::new(channel);
            let resp = match client
                .list_installed(ListInstalledRequest {
                    kind: ToolchainKind::Unspecified as i32,
                })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: format!("List installed toolchains failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            if resp.items.is_empty() {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "No installed toolchains to verify.\n".into(),
                })
                .ok();
            } else {
                for item in resp.items {
                    let Some(id) = item.toolchain_id.as_ref().map(|i| i.value.clone()) else {
                        ui.send(AppEvent::Log {
                            page: "toolchains",
                            line: "Skipping toolchain with missing id.\n".into(),
                        })
                        .ok();
                        continue;
                    };
                    let verify = match client
                        .verify_toolchain(VerifyToolchainRequest {
                            toolchain_id: Some(Id { value: id.clone() }),
                            job_id: job_id
                                .as_ref()
                                .filter(|value| !value.trim().is_empty())
                                .map(|value| Id {
                                    value: value.clone(),
                                }),
                            correlation_id: correlation_id.trim().to_string(),
                            run_id: run_id_from_optional(&correlation_id),
                        })
                        .await
                    {
                        Ok(resp) => resp.into_inner(),
                        Err(err) => {
                            ui.send(AppEvent::Log {
                                page: "toolchains",
                                line: format!("- {id} verify request failed: {err}\n"),
                            })
                            .ok();
                            continue;
                        }
                    };

                    if verify.verified {
                        ui.send(AppEvent::Log {
                            page: "toolchains",
                            line: format!("- {id} verified OK\n"),
                        })
                        .ok();
                    } else if let Some(err) = verify.error {
                        ui.send(AppEvent::Log {
                            page: "toolchains",
                            line: format!("- {id} verify failed: {} ({})\n", err.message, err.code),
                        })
                        .ok();
                    } else {
                        ui.send(AppEvent::Log {
                            page: "toolchains",
                            line: format!("- {id} verify failed (no details)\n"),
                        })
                        .ok();
                    }

                    if let Some(job_id) = verify.job_id.map(|i| i.value) {
                        if !job_id.is_empty() {
                            let job_addr = cfg.job_addr.clone();
                            let ui_stream = ui.clone();
                            let ui_err = ui.clone();
                            stream_tasks.spawn(async move {
                                if let Err(err) = stream_job_events(
                                    job_addr,
                                    job_id.clone(),
                                    "toolchains",
                                    ui_stream,
                                )
                                .await
                                {
                                    let _ = ui_err.send(AppEvent::Log {
                                        page: "toolchains",
                                        line: format!("job stream error ({job_id}): {err}\n"),
                                    });
                                }
                            });
                        }
                    }
                }
            }
        }

        UiCommand::ToolchainCreateSet {
            cfg,
            sdk_toolchain_id,
            ndk_toolchain_id,
            display_name,
        } => {
            let sdk_id = sdk_toolchain_id
                .as_ref()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string());
            let ndk_id = ndk_toolchain_id
                .as_ref()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string());
            if sdk_id.is_none() && ndk_id.is_none() {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "Provide an SDK and/or NDK toolchain id.\n".into(),
                })
                .ok();
                return Ok(());
            }

            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!("Creating toolchain set via {}\n", cfg.toolchain_addr),
            })
            .ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client
                .create_toolchain_set(CreateToolchainSetRequest {
                    sdk_toolchain_id: sdk_id.map(|value| Id { value }),
                    ndk_toolchain_id: ndk_id.map(|value| Id { value }),
                    display_name: display_name.trim().to_string(),
                })
                .await?;

            if let Some(set) = resp.into_inner().set {
                let set_id = set
                    .toolchain_set_id
                    .as_ref()
                    .map(|id| id.value.as_str())
                    .unwrap_or("");
                let sdk = set
                    .sdk_toolchain_id
                    .as_ref()
                    .map(|id| id.value.as_str())
                    .unwrap_or("");
                let ndk = set
                    .ndk_toolchain_id
                    .as_ref()
                    .map(|id| id.value.as_str())
                    .unwrap_or("");
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: format!(
                        "Created set {set_id}\n  display_name={}\n  sdk={}\n  ndk={}\n",
                        set.display_name, sdk, ndk
                    ),
                })
                .ok();
            } else {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "Create toolchain set returned no set.\n".into(),
                })
                .ok();
            }
        }

        UiCommand::ToolchainCreateActiveLatest { cfg, display_name } => {
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!(
                    "Creating active toolchain set from latest installed via {}\n",
                    cfg.toolchain_addr
                ),
            })
            .ok();
            let channel = match connect(&cfg.toolchain_addr).await {
                Ok(channel) => channel,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: format!("ToolchainService connection failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let mut client = ToolchainServiceClient::new(channel);
            let installed = match client
                .list_installed(ListInstalledRequest {
                    kind: ToolchainKind::Unspecified as i32,
                })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: format!("List installed toolchains failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let items = installed.items;
            let Some(sdk) = latest_installed_by_kind(&items, ToolchainKind::Sdk) else {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "No installed SDK toolchains found. Install an SDK first.\n".into(),
                })
                .ok();
                return Ok(());
            };
            let Some(ndk) = latest_installed_by_kind(&items, ToolchainKind::Ndk) else {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "No installed NDK toolchains found. Install an NDK first.\n".into(),
                })
                .ok();
                return Ok(());
            };

            let sdk_id = match sdk
                .toolchain_id
                .as_ref()
                .map(|id| id.value.as_str())
                .filter(|value| !value.trim().is_empty())
            {
                Some(value) => value.to_string(),
                None => {
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: "Latest SDK toolchain is missing an id.\n".into(),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let ndk_id = match ndk
                .toolchain_id
                .as_ref()
                .map(|id| id.value.as_str())
                .filter(|value| !value.trim().is_empty())
            {
                Some(value) => value.to_string(),
                None => {
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: "Latest NDK toolchain is missing an id.\n".into(),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let sdk_version = sdk
                .version
                .as_ref()
                .map(|v| v.version.as_str())
                .unwrap_or("unknown");
            let ndk_version = ndk
                .version
                .as_ref()
                .map(|v| v.version.as_str())
                .unwrap_or("unknown");

            let display_name = display_name.trim();
            let display_name = if display_name.is_empty() {
                format!("SDK {sdk_version} + NDK {ndk_version}")
            } else {
                display_name.to_string()
            };

            let created = match client
                .create_toolchain_set(CreateToolchainSetRequest {
                    sdk_toolchain_id: Some(Id {
                        value: sdk_id.clone(),
                    }),
                    ndk_toolchain_id: Some(Id {
                        value: ndk_id.clone(),
                    }),
                    display_name: display_name.clone(),
                })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: format!("Create toolchain set failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            let Some(set) = created.set else {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "Create toolchain set returned no set.\n".into(),
                })
                .ok();
                return Ok(());
            };
            let set_id = match set
                .toolchain_set_id
                .as_ref()
                .map(|id| id.value.as_str())
                .filter(|value| !value.trim().is_empty())
            {
                Some(value) => value.to_string(),
                None => {
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: "Created toolchain set is missing an id.\n".into(),
                    })
                    .ok();
                    return Ok(());
                }
            };
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!(
                    "Created set {set_id}\n  display_name={}\n  sdk={}\n  ndk={}\n",
                    set.display_name, sdk_id, ndk_id
                ),
            })
            .ok();

            let resp = match client
                .set_active_toolchain_set(SetActiveToolchainSetRequest {
                    toolchain_set_id: Some(Id {
                        value: set_id.clone(),
                    }),
                })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "toolchains",
                        line: format!("Set active toolchain set failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            if resp.ok {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: format!("Active toolchain set updated to {set_id}\n"),
                })
                .ok();
                ui.send(AppEvent::UpdateActiveContext {
                    project_id: None,
                    project_path: None,
                    toolchain_set_id: Some(set_id.clone()),
                    target_id: None,
                    run_id: None,
                })
                .ok();
            } else {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "Failed to set active toolchain set.\n".into(),
                })
                .ok();
            }
        }

        UiCommand::ToolchainSetActive {
            cfg,
            toolchain_set_id,
        } => {
            let set_id = toolchain_set_id.trim().to_string();
            if set_id.is_empty() {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "Set active requires a toolchain set id.\n".into(),
                })
                .ok();
                return Ok(());
            }
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!("Setting active toolchain set via {}\n", cfg.toolchain_addr),
            })
            .ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client
                .set_active_toolchain_set(SetActiveToolchainSetRequest {
                    toolchain_set_id: Some(Id {
                        value: set_id.clone(),
                    }),
                })
                .await?
                .into_inner();

            if resp.ok {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: format!("Active toolchain set set to {set_id}\n"),
                })
                .ok();
                ui.send(AppEvent::UpdateActiveContext {
                    project_id: None,
                    project_path: None,
                    toolchain_set_id: Some(set_id.clone()),
                    target_id: None,
                    run_id: None,
                })
                .ok();
            } else {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "Failed to set active toolchain set.\n".into(),
                })
                .ok();
            }
        }

        UiCommand::ToolchainGetActive { cfg } => {
            ui.send(AppEvent::Log {
                page: "toolchains",
                line: format!("Fetching active toolchain set via {}\n", cfg.toolchain_addr),
            })
            .ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client
                .get_active_toolchain_set(GetActiveToolchainSetRequest {})
                .await?
                .into_inner();
            if let Some(set) = resp.set {
                let set_id = set
                    .toolchain_set_id
                    .as_ref()
                    .map(|id| id.value.as_str())
                    .unwrap_or("");
                let sdk = set
                    .sdk_toolchain_id
                    .as_ref()
                    .map(|id| id.value.as_str())
                    .unwrap_or("");
                let ndk = set
                    .ndk_toolchain_id
                    .as_ref()
                    .map(|id| id.value.as_str())
                    .unwrap_or("");
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: format!(
                        "Active set {set_id}\n  display_name={}\n  sdk={}\n  ndk={}\n",
                        set.display_name, sdk, ndk
                    ),
                })
                .ok();
                if !set_id.trim().is_empty() {
                    ui.send(AppEvent::UpdateActiveContext {
                        project_id: None,
                        project_path: None,
                        toolchain_set_id: Some(set_id.to_string()),
                        target_id: None,
                        run_id: None,
                    })
                    .ok();
                }
            } else {
                ui.send(AppEvent::Log {
                    page: "toolchains",
                    line: "No active toolchain set configured.\n".into(),
                })
                .ok();
            }
        }

        UiCommand::ProjectListTemplates { cfg } => {
            ui.send(AppEvent::Log {
                page: "projects",
                line: format!("Connecting to ProjectService at {}\n", cfg.project_addr),
            })
            .ok();
            let mut client = ProjectServiceClient::new(connect(&cfg.project_addr).await?);
            let resp = client
                .list_templates(ListTemplatesRequest {})
                .await?
                .into_inner();

            ui.send(AppEvent::Log {
                page: "projects",
                line: "Templates:\n".into(),
            })
            .ok();
            let mut options = Vec::new();
            for t in resp.templates {
                let template_id = t
                    .template_id
                    .as_ref()
                    .map(|i| i.value.clone())
                    .unwrap_or_default();
                if template_id.is_empty() {
                    ui.send(AppEvent::Log {
                        page: "projects",
                        line: format!("- {} (missing template_id)\n", t.name),
                    })
                    .ok();
                    continue;
                }

                let defaults = if t.defaults.is_empty() {
                    "defaults: none".to_string()
                } else {
                    let pairs = t
                        .defaults
                        .iter()
                        .map(|kv| format!("{}={}", kv.key, kv.value))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("defaults: {pairs}")
                };

                ui.send(AppEvent::Log {
                    page: "projects",
                    line: format!(
                        "- {} ({})\n  {}\n  {}\n",
                        t.name, template_id, t.description, defaults
                    ),
                })
                .ok();
                options.push(ProjectTemplateOption {
                    id: template_id,
                    name: t.name,
                });
            }
            ui.send(AppEvent::ProjectTemplates { templates: options })
                .ok();
        }

        UiCommand::ProjectListRecent { cfg } => {
            ui.send(AppEvent::Log {
                page: "projects",
                line: format!("Listing recent projects via {}\n", cfg.project_addr),
            })
            .ok();
            let mut client = ProjectServiceClient::new(connect(&cfg.project_addr).await?);
            let resp = client
                .list_recent_projects(ListRecentProjectsRequest { page: None })
                .await?
                .into_inner();

            if resp.projects.is_empty() {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: "No recent projects.\n".into(),
                })
                .ok();
            } else {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: "Recent projects:\n".into(),
                })
                .ok();
                for project in resp.projects {
                    let id = project
                        .project_id
                        .as_ref()
                        .map(|i| i.value.as_str())
                        .unwrap_or("");
                    ui.send(AppEvent::Log {
                        page: "projects",
                        line: format!("- {} ({})\n  path={}\n", project.name, id, project.path),
                    })
                    .ok();
                }
            }

            if let Some(page_info) = resp.page_info {
                if !page_info.next_page_token.trim().is_empty() {
                    ui.send(AppEvent::Log {
                        page: "projects",
                        line: format!("next_page_token={}\n", page_info.next_page_token),
                    })
                    .ok();
                }
            }
        }

        UiCommand::ProjectLoadDefaults { cfg } => {
            ui.send(AppEvent::Log {
                page: "projects",
                line: format!("Loading toolchain sets via {}\n", cfg.toolchain_addr),
            })
            .ok();
            match connect(&cfg.toolchain_addr).await {
                Ok(channel) => {
                    let mut client = ToolchainServiceClient::new(channel);
                    match client
                        .list_toolchain_sets(ListToolchainSetsRequest { page: None })
                        .await
                    {
                        Ok(resp) => {
                            let mut sets = Vec::new();
                            for set in resp.into_inner().sets {
                                let set_id = set
                                    .toolchain_set_id
                                    .as_ref()
                                    .map(|id| id.value.as_str())
                                    .unwrap_or("");
                                if set_id.is_empty() {
                                    continue;
                                }
                                let name = if set.display_name.trim().is_empty() {
                                    "Toolchain Set"
                                } else {
                                    set.display_name.as_str()
                                };
                                let sdk = set
                                    .sdk_toolchain_id
                                    .as_ref()
                                    .map(|id| id.value.as_str())
                                    .unwrap_or("");
                                let ndk = set
                                    .ndk_toolchain_id
                                    .as_ref()
                                    .map(|id| id.value.as_str())
                                    .unwrap_or("");
                                let label = format!("{name} ({set_id}) sdk={sdk} ndk={ndk}");
                                sets.push(ToolchainSetOption {
                                    id: set_id.to_string(),
                                    label,
                                });
                            }
                            ui.send(AppEvent::ProjectToolchainSets { sets }).ok();
                        }
                        Err(err) => {
                            ui.send(AppEvent::Log {
                                page: "projects",
                                line: format!("List toolchain sets failed: {err}\n"),
                            })
                            .ok();
                        }
                    }
                }
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "projects",
                        line: format!("ToolchainService connection failed: {err}\n"),
                    })
                    .ok();
                }
            }

            ui.send(AppEvent::Log {
                page: "projects",
                line: format!("Loading targets via {}\n", cfg.targets_addr),
            })
            .ok();
            match connect(&cfg.targets_addr).await {
                Ok(channel) => {
                    let mut client = TargetServiceClient::new(channel);
                    match client
                        .list_targets(ListTargetsRequest {
                            include_offline: true,
                        })
                        .await
                    {
                        Ok(resp) => {
                            let mut targets = Vec::new();
                            for target in resp.into_inner().targets {
                                let target_id = target
                                    .target_id
                                    .as_ref()
                                    .map(|id| id.value.as_str())
                                    .unwrap_or("");
                                if target_id.is_empty() {
                                    continue;
                                }
                                let label = format!(
                                    "{} [{}] {} ({})",
                                    target.display_name, target.provider, target.state, target_id
                                );
                                targets.push(TargetOption {
                                    id: target_id.to_string(),
                                    label,
                                });
                            }
                            ui.send(AppEvent::ProjectTargets { targets }).ok();
                        }
                        Err(err) => {
                            ui.send(AppEvent::Log {
                                page: "projects",
                                line: format!("List targets failed: {err}\n"),
                            })
                            .ok();
                        }
                    }
                }
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "projects",
                        line: format!("TargetService connection failed: {err}\n"),
                    })
                    .ok();
                }
            }
        }

        UiCommand::ProjectCreate {
            cfg,
            name,
            path,
            template_id,
            job_id,
            correlation_id,
        } => {
            let name = name.trim().to_string();
            let path = path.trim().to_string();
            let template_id = template_id.trim().to_string();

            if name.is_empty() {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: "Create project requires a name.\n".into(),
                })
                .ok();
                return Ok(());
            }
            if path.is_empty() {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: "Create project requires a path.\n".into(),
                })
                .ok();
                return Ok(());
            }
            if template_id.is_empty() || template_id == "none" {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: "Select a template before creating the project.\n".into(),
                })
                .ok();
                return Ok(());
            }

            ui.send(AppEvent::Log {
                page: "projects",
                line: format!("Creating project via {}\n", cfg.project_addr),
            })
            .ok();
            let channel = match connect(&cfg.project_addr).await {
                Ok(channel) => channel,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "projects",
                        line: format!("ProjectService connection failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let mut client = ProjectServiceClient::new(channel);
            let resp = match client
                .create_project(CreateProjectRequest {
                    name: name.clone(),
                    path: path.clone(),
                    template_id: Some(Id {
                        value: template_id.clone(),
                    }),
                    params: vec![],
                    toolchain_set_id: None,
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "projects",
                        line: format!("Create project failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            let project_id = resp.project_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page: "projects",
                line: format!("Create queued: project_id={project_id} job_id={job_id}\n"),
            })
            .ok();
            if !project_id.trim().is_empty() {
                ui.send(AppEvent::ProjectSelected {
                    project_id: project_id.clone(),
                    project_path: path.clone(),
                    opened_existing: false,
                })
                .ok();
            }

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                stream_tasks.spawn(async move {
                    if let Err(err) =
                        stream_job_events(job_addr, job_id.clone(), "projects", ui_stream).await
                    {
                        let _ = ui_err.send(AppEvent::Log {
                            page: "projects",
                            line: format!("job stream error ({job_id}): {err}\n"),
                        });
                    }
                });
            }
        }

        UiCommand::ProjectOpen { cfg, path } => {
            let path = path.trim().to_string();
            if path.is_empty() {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: "Open project requires a path.\n".into(),
                })
                .ok();
                return Ok(());
            }

            ui.send(AppEvent::Log {
                page: "projects",
                line: format!("Opening project via {}\n", cfg.project_addr),
            })
            .ok();
            let channel = match connect(&cfg.project_addr).await {
                Ok(channel) => channel,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "projects",
                        line: format!("ProjectService connection failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let mut client = ProjectServiceClient::new(channel);
            let resp = match client
                .open_project(OpenProjectRequest { path: path.clone() })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "projects",
                        line: format!("Open project failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            if let Some(project) = resp.project {
                let id = project
                    .project_id
                    .as_ref()
                    .map(|i| i.value.as_str())
                    .unwrap_or("");
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: format!(
                        "Opened: {} ({})\n  path={}\n",
                        project.name, id, project.path
                    ),
                })
                .ok();
                if !id.trim().is_empty() {
                    ui.send(AppEvent::ProjectSelected {
                        project_id: id.to_string(),
                        project_path: project.path.clone(),
                        opened_existing: true,
                    })
                    .ok();
                }
            } else {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: "Open project returned no project.\n".into(),
                })
                .ok();
            }
        }

        UiCommand::ProjectSetConfig {
            cfg,
            project_id,
            toolchain_set_id,
            default_target_id,
        } => {
            let project_id = project_id.trim().to_string();
            if project_id.is_empty() {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: "Set project config requires a project id.\n".into(),
                })
                .ok();
                return Ok(());
            }
            let toolchain_set_id = toolchain_set_id
                .as_ref()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string());
            let default_target_id = default_target_id
                .as_ref()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string());

            if toolchain_set_id.is_none() && default_target_id.is_none() {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: "Provide a toolchain set id and/or default target id.\n".into(),
                })
                .ok();
                return Ok(());
            }

            ui.send(AppEvent::Log {
                page: "projects",
                line: format!("Updating project config via {}\n", cfg.project_addr),
            })
            .ok();
            let mut client = ProjectServiceClient::new(connect(&cfg.project_addr).await?);
            let resp = client
                .set_project_config(SetProjectConfigRequest {
                    project_id: Some(Id {
                        value: project_id.clone(),
                    }),
                    toolchain_set_id: toolchain_set_id.map(|value| Id { value }),
                    default_target_id: default_target_id.map(|value| Id { value }),
                })
                .await?;

            if resp.into_inner().ok {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: format!("Updated config for {project_id}\n"),
                })
                .ok();
            } else {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: "Project config update returned ok=false.\n".into(),
                })
                .ok();
            }
        }

        UiCommand::ProjectUseActiveDefaults { cfg, project_id } => {
            let project_id = project_id.trim().to_string();
            if project_id.is_empty() {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: "Use active defaults requires a project id.\n".into(),
                })
                .ok();
                return Ok(());
            }

            let mut toolchain_set_id = None;
            match connect(&cfg.toolchain_addr).await {
                Ok(channel) => {
                    let mut client = ToolchainServiceClient::new(channel);
                    match client
                        .get_active_toolchain_set(GetActiveToolchainSetRequest {})
                        .await
                    {
                        Ok(resp) => {
                            toolchain_set_id = resp
                                .into_inner()
                                .set
                                .and_then(|set| set.toolchain_set_id)
                                .map(|id| id.value)
                                .filter(|value| !value.trim().is_empty());
                        }
                        Err(err) => {
                            ui.send(AppEvent::Log {
                                page: "projects",
                                line: format!("Active toolchain set lookup failed: {err}\n"),
                            })
                            .ok();
                        }
                    }
                }
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "projects",
                        line: format!("ToolchainService connection failed: {err}\n"),
                    })
                    .ok();
                }
            }

            let mut default_target_id = None;
            match connect(&cfg.targets_addr).await {
                Ok(channel) => {
                    let mut client = TargetServiceClient::new(channel);
                    match client.get_default_target(GetDefaultTargetRequest {}).await {
                        Ok(resp) => {
                            default_target_id = resp
                                .into_inner()
                                .target
                                .and_then(|target| target.target_id)
                                .map(|id| id.value)
                                .filter(|value| !value.trim().is_empty());
                        }
                        Err(err) => {
                            ui.send(AppEvent::Log {
                                page: "projects",
                                line: format!("Default target lookup failed: {err}\n"),
                            })
                            .ok();
                        }
                    }
                }
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "projects",
                        line: format!("TargetService connection failed: {err}\n"),
                    })
                    .ok();
                }
            }

            if toolchain_set_id.is_none() && default_target_id.is_none() {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: "No active toolchain set or default target found.\n".into(),
                })
                .ok();
                return Ok(());
            }

            ui.send(AppEvent::Log {
                page: "projects",
                line: format!("Applying active defaults via {}\n", cfg.project_addr),
            })
            .ok();
            let mut client = ProjectServiceClient::new(connect(&cfg.project_addr).await?);
            let resp = client
                .set_project_config(SetProjectConfigRequest {
                    project_id: Some(Id {
                        value: project_id.clone(),
                    }),
                    toolchain_set_id: toolchain_set_id.clone().map(|value| Id { value }),
                    default_target_id: default_target_id.clone().map(|value| Id { value }),
                })
                .await?;

            if resp.into_inner().ok {
                ui.send(AppEvent::Log { page: "projects", line: format!("Applied defaults to {project_id}\n  toolchain_set_id={}\n  default_target_id={}\n", toolchain_set_id.unwrap_or_default(), default_target_id.unwrap_or_default()) }).ok();
            } else {
                ui.send(AppEvent::Log {
                    page: "projects",
                    line: "Project config update returned ok=false.\n".into(),
                })
                .ok();
            }
        }

        UiCommand::TargetsList { cfg } => {
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Connecting to TargetService at {}\n", cfg.targets_addr),
            })
            .ok();
            let channel = match connect(&cfg.targets_addr).await {
                Ok(channel) => channel,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "targets",
                        line: format!("TargetService connection failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let mut client = TargetServiceClient::new(channel);
            let resp = match client
                .list_targets(ListTargetsRequest {
                    include_offline: true,
                })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "targets",
                        line: format!("List targets failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            ui.send(AppEvent::Log {
                page: "targets",
                line: "Targets:\n".into(),
            })
            .ok();
            let active_target = cfg.active_target_id.trim().to_string();
            let mut has_active = false;
            let mut preferred_target: Option<String> = None;
            let mut fallback_target: Option<String> = None;
            for t in resp.targets {
                let id = t
                    .target_id
                    .as_ref()
                    .map(|i| i.value.clone())
                    .unwrap_or_default();
                if !active_target.is_empty() && id.eq_ignore_ascii_case(&active_target) {
                    has_active = true;
                }
                if !id.trim().is_empty() {
                    if fallback_target.is_none() {
                        fallback_target = Some(id.clone());
                    }
                    let state = t.state.trim().to_ascii_lowercase();
                    if preferred_target.is_none() && (state == "device" || state == "online") {
                        preferred_target = Some(id.clone());
                    }
                }
                ui.send(AppEvent::Log {
                    page: "targets",
                    line: format!(
                        "- {} [{}] {} ({})\n",
                        t.display_name, id, t.state, t.provider
                    ),
                })
                .ok();
            }
            if active_target.is_empty() || !has_active {
                let target_id = preferred_target.or(fallback_target);
                if let Some(target_id) = target_id {
                    if !target_id.trim().is_empty() {
                        ui.send(AppEvent::UpdateActiveContext {
                            project_id: None,
                            project_path: None,
                            toolchain_set_id: None,
                            target_id: Some(target_id),
                            run_id: None,
                        })
                        .ok();
                    }
                }
            }
        }

        UiCommand::TargetsSetDefault { cfg, target_id } => {
            let target_id = target_id.trim().to_string();
            if target_id.is_empty() {
                ui.send(AppEvent::Log {
                    page: "targets",
                    line: "Set default target requires a target id.\n".into(),
                })
                .ok();
                return Ok(());
            }
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Setting default target via {}\n", cfg.targets_addr),
            })
            .ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client
                .set_default_target(SetDefaultTargetRequest {
                    target_id: Some(Id {
                        value: target_id.clone(),
                    }),
                })
                .await?
                .into_inner();

            if resp.ok {
                ui.send(AppEvent::Log {
                    page: "targets",
                    line: format!("Default target set to {target_id}\n"),
                })
                .ok();
                ui.send(AppEvent::UpdateActiveContext {
                    project_id: None,
                    project_path: None,
                    toolchain_set_id: None,
                    target_id: Some(target_id.clone()),
                    run_id: None,
                })
                .ok();
            } else {
                ui.send(AppEvent::Log {
                    page: "targets",
                    line: format!("Failed to set default target to {target_id}\n"),
                })
                .ok();
            }
        }

        UiCommand::TargetsGetDefault { cfg } => {
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Fetching default target via {}\n", cfg.targets_addr),
            })
            .ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client
                .get_default_target(GetDefaultTargetRequest {})
                .await?
                .into_inner();
            if let Some(target) = resp.target {
                let id = target
                    .target_id
                    .as_ref()
                    .map(|i| i.value.as_str())
                    .unwrap_or("");
                ui.send(AppEvent::Log {
                    page: "targets",
                    line: format!(
                        "Default target: {} ({}) [{}]\n",
                        target.display_name, id, target.state
                    ),
                })
                .ok();
                if !id.trim().is_empty() {
                    ui.send(AppEvent::UpdateActiveContext {
                        project_id: None,
                        project_path: None,
                        toolchain_set_id: None,
                        target_id: Some(id.to_string()),
                        run_id: None,
                    })
                    .ok();
                }
            } else {
                ui.send(AppEvent::Log {
                    page: "targets",
                    line: "No default target configured.\n".into(),
                })
                .ok();
            }
        }

        UiCommand::TargetsInstallCuttlefish {
            cfg,
            force,
            branch,
            target,
            build_id,
            job_id,
            correlation_id,
        } => {
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Connecting to TargetService at {}\n", cfg.targets_addr),
            })
            .ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client
                .install_cuttlefish(InstallCuttlefishRequest {
                    force,
                    branch: branch.trim().to_string(),
                    target: target.trim().to_string(),
                    build_id: build_id.trim().to_string(),
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await?
                .into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Cuttlefish install job: {job_id}\n"),
            })
            .ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                stream_tasks.spawn(async move {
                    if let Err(err) =
                        stream_job_events(job_addr, job_id.clone(), "targets", ui_stream).await
                    {
                        let _ = ui_err.send(AppEvent::Log {
                            page: "targets",
                            line: format!("job stream error ({job_id}): {err}\n"),
                        });
                    }
                });
            }
        }

        UiCommand::TargetsResolveCuttlefishBuild {
            cfg,
            branch,
            target,
            build_id,
        } => {
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Resolving Cuttlefish build via {}\n", cfg.targets_addr),
            })
            .ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client
                .resolve_cuttlefish_build(ResolveCuttlefishBuildRequest {
                    branch: branch.trim().to_string(),
                    target: target.trim().to_string(),
                    build_id: build_id.trim().to_string(),
                })
                .await?
                .into_inner();

            ui.send(AppEvent::Log {
                page: "targets",
                line: format!(
                    "Resolved build_id={} product={} (branch={}, target={})\n",
                    resp.build_id, resp.product, resp.branch, resp.target
                ),
            })
            .ok();
            ui.send(AppEvent::SetCuttlefishBuildId {
                build_id: resp.build_id,
            })
            .ok();
        }

        UiCommand::TargetsStartCuttlefish {
            cfg,
            show_full_ui,
            job_id,
            correlation_id,
        } => {
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Connecting to TargetService at {}\n", cfg.targets_addr),
            })
            .ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client
                .start_cuttlefish(StartCuttlefishRequest {
                    show_full_ui,
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await?
                .into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Cuttlefish start job: {job_id}\n"),
            })
            .ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                stream_tasks.spawn(async move {
                    if let Err(err) =
                        stream_job_events(job_addr, job_id.clone(), "targets", ui_stream).await
                    {
                        let _ = ui_err.send(AppEvent::Log {
                            page: "targets",
                            line: format!("job stream error ({job_id}): {err}\n"),
                        });
                    }
                });
            }
        }

        UiCommand::TargetsStopCuttlefish {
            cfg,
            job_id,
            correlation_id,
        } => {
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Connecting to TargetService at {}\n", cfg.targets_addr),
            })
            .ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client
                .stop_cuttlefish(StopCuttlefishRequest {
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await?
                .into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Cuttlefish stop job: {job_id}\n"),
            })
            .ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                stream_tasks.spawn(async move {
                    if let Err(err) =
                        stream_job_events(job_addr, job_id.clone(), "targets", ui_stream).await
                    {
                        let _ = ui_err.send(AppEvent::Log {
                            page: "targets",
                            line: format!("job stream error ({job_id}): {err}\n"),
                        });
                    }
                });
            }
        }

        UiCommand::TargetsCuttlefishStatus { cfg } => {
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Connecting to TargetService at {}\n", cfg.targets_addr),
            })
            .ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client
                .get_cuttlefish_status(GetCuttlefishStatusRequest {})
                .await?
                .into_inner();
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!(
                    "Cuttlefish state: {} (adb={})\n",
                    resp.state, resp.adb_serial
                ),
            })
            .ok();
            for kv in resp.details {
                ui.send(AppEvent::Log {
                    page: "targets",
                    line: format!("- {}: {}\n", kv.key, kv.value),
                })
                .ok();
            }
        }

        UiCommand::TargetsInstallApk {
            cfg,
            target_id,
            apk_path,
            job_id,
            correlation_id,
        } => {
            let target_id = target_id.trim().to_string();
            let apk_path = apk_path.trim().to_string();
            if target_id.is_empty() {
                ui.send(AppEvent::Log {
                    page: "targets",
                    line: "Install APK requires a target id.\n".into(),
                })
                .ok();
                return Ok(());
            }
            if apk_path.is_empty() {
                ui.send(AppEvent::Log {
                    page: "targets",
                    line: "Install APK requires an apk path.\n".into(),
                })
                .ok();
                return Ok(());
            }

            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Connecting to TargetService at {}\n", cfg.targets_addr),
            })
            .ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = match client
                .install_apk(InstallApkRequest {
                    target_id: Some(Id {
                        value: target_id.clone(),
                    }),
                    project_id: None,
                    apk_path: apk_path.clone(),
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "targets",
                        line: format!("Install APK request failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Install APK job: {job_id}\n"),
            })
            .ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                stream_tasks.spawn(async move {
                    if let Err(err) =
                        stream_job_events(job_addr, job_id.clone(), "targets", ui_stream).await
                    {
                        let _ = ui_err.send(AppEvent::Log {
                            page: "targets",
                            line: format!("job stream error ({job_id}): {err}\n"),
                        });
                    }
                });
            }
        }

        UiCommand::TargetsLaunchApp {
            cfg,
            target_id,
            apk_path,
            application_id,
            activity,
            job_id,
            correlation_id,
        } => {
            let target_id = target_id.trim().to_string();
            let apk_path = apk_path.trim().to_string();
            let mut application_id = application_id.trim().to_string();
            let activity = activity.trim().to_string();
            if target_id.is_empty() {
                ui.send(AppEvent::Log {
                    page: "targets",
                    line: "Launch requires a target id.\n".into(),
                })
                .ok();
                return Ok(());
            }
            if application_id.is_empty() {
                if let Some(inferred) = infer_application_id_from_apk_path(&apk_path) {
                    application_id = inferred;
                    ui.send(AppEvent::Log {
                        page: "targets",
                        line: format!("Inferred application id from APK: {application_id}\n"),
                    })
                    .ok();
                }
            }
            if application_id.is_empty() {
                ui.send(AppEvent::Log {
                    page: "targets",
                    line: "Launch requires an application id (provide one or select an APK from a project).\n".into(),
                })
                .ok();
                return Ok(());
            }

            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Connecting to TargetService at {}\n", cfg.targets_addr),
            })
            .ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = match client
                .launch(LaunchRequest {
                    target_id: Some(Id {
                        value: target_id.clone(),
                    }),
                    application_id: application_id.clone(),
                    activity,
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "targets",
                        line: format!("Launch request failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!("Launch job: {job_id}\n"),
            })
            .ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                stream_tasks.spawn(async move {
                    if let Err(err) =
                        stream_job_events(job_addr, job_id.clone(), "targets", ui_stream).await
                    {
                        let _ = ui_err.send(AppEvent::Log {
                            page: "targets",
                            line: format!("job stream error ({job_id}): {err}\n"),
                        });
                    }
                });
            }
        }

        UiCommand::TargetsStreamLogcat {
            cfg,
            target_id,
            filter,
        } => {
            ui.send(AppEvent::Log {
                page: "targets",
                line: format!(
                    "Streaming logcat from {target_id} via {}\n",
                    cfg.targets_addr
                ),
            })
            .ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let mut stream = client
                .stream_logcat(StreamLogcatRequest {
                    target_id: Some(Id {
                        value: target_id.clone(),
                    }),
                    filter,
                    include_history: true,
                })
                .await?
                .into_inner();

            while let Some(item) = stream.next().await {
                match item {
                    Ok(evt) => {
                        let line = String::from_utf8_lossy(&evt.line).to_string();
                        ui.send(AppEvent::Log {
                            page: "targets",
                            line,
                        })
                        .ok();
                    }
                    Err(s) => {
                        ui.send(AppEvent::Log {
                            page: "targets",
                            line: format!("logcat stream error: {s}\n"),
                        })
                        .ok();
                        break;
                    }
                }
            }
        }

        UiCommand::ObserveListRuns {
            cfg,
            run_id,
            correlation_id,
            result,
            page_size,
            page_token,
            page,
        } => {
            ui.send(AppEvent::Log {
                page,
                line: format!("Listing runs via {}\n", cfg.observe_addr),
            })
            .ok();
            let mut client = ObserveServiceClient::new(connect(&cfg.observe_addr).await?);
            let run_id_value = run_id.trim().to_string();
            let correlation_value = correlation_id.trim().to_string();
            let result_value = result.trim().to_string();
            let filter = if run_id_value.is_empty()
                && correlation_value.is_empty()
                && result_value.is_empty()
            {
                None
            } else {
                Some(RunFilter {
                    run_id: run_id_from_optional(&run_id_value),
                    correlation_id: correlation_value,
                    project_id: None,
                    target_id: None,
                    toolchain_set_id: None,
                    result: result_value,
                })
            };

            let resp = client
                .list_runs(ListRunsRequest {
                    page: Some(Pagination {
                        page_size: page_size.max(1),
                        page_token,
                    }),
                    filter,
                })
                .await?
                .into_inner();

            if resp.runs.is_empty() {
                ui.send(AppEvent::Log {
                    page,
                    line: "No runs recorded.\n".into(),
                })
                .ok();
            } else {
                ui.send(AppEvent::Log {
                    page,
                    line: "Runs:\n".into(),
                })
                .ok();
                for run in resp.runs {
                    let run_id = run.run_id.as_ref().map(|i| i.value.as_str()).unwrap_or("-");
                    let project_id = run
                        .project_id
                        .as_ref()
                        .map(|i| i.value.as_str())
                        .unwrap_or("-");
                    let target_id = run
                        .target_id
                        .as_ref()
                        .map(|i| i.value.as_str())
                        .unwrap_or("-");
                    let toolchain_set_id = run
                        .toolchain_set_id
                        .as_ref()
                        .map(|i| i.value.as_str())
                        .unwrap_or("-");
                    let started = run.started_at.as_ref().map(|t| t.unix_millis).unwrap_or(0);
                    let finished = run.finished_at.as_ref().map(|t| t.unix_millis).unwrap_or(0);
                    ui.send(AppEvent::Log {
                        page,
                        line: format!(
                            "- {} result={} corr={} project={} target={} toolchain_set={} started={} finished={} jobs={}\n",
                            run_id,
                            run.result,
                            run.correlation_id,
                            project_id,
                            target_id,
                            toolchain_set_id,
                            started,
                            finished,
                            run.job_ids.len()
                        ),
                    }).ok();
                    if !run.job_ids.is_empty() {
                        let job_ids = run
                            .job_ids
                            .iter()
                            .map(|id| id.value.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        ui.send(AppEvent::Log {
                            page,
                            line: format!("  job_ids: {job_ids}\n"),
                        })
                        .ok();
                    }
                    if let Some(summary) = run.output_summary.as_ref() {
                        let updated = summary
                            .updated_at
                            .as_ref()
                            .map(|ts| ts.unix_millis)
                            .unwrap_or(0);
                        ui.send(AppEvent::Log {
                            page,
                            line: format!(
                                "  outputs: bundles={} artifacts={} updated={} last_bundle_id={}\n",
                                summary.bundle_count,
                                summary.artifact_count,
                                updated,
                                summary.last_bundle_id
                            ),
                        })
                        .ok();
                    }
                    if !run.summary.is_empty() {
                        ui.send(AppEvent::Log {
                            page,
                            line: format!("  summary: {}\n", kv_pairs(&run.summary)),
                        })
                        .ok();
                    }
                }
            }

            if let Some(page_info) = resp.page_info {
                if !page_info.next_page_token.trim().is_empty() {
                    ui.send(AppEvent::Log {
                        page,
                        line: format!("next_page_token={}\n", page_info.next_page_token),
                    })
                    .ok();
                }
            }
        }

        UiCommand::ObserveListOutputs {
            cfg,
            run_id,
            kind,
            output_type,
            path_contains,
            label_contains,
            page_size,
            page_token,
            page,
        } => {
            let run_id = run_id.trim().to_string();
            if run_id.is_empty() {
                ui.send(AppEvent::Log {
                    page,
                    line: "Run id is required for output listing.\n".into(),
                })
                .ok();
                return Ok(());
            }
            ui.send(AppEvent::Log {
                page,
                line: format!("Listing outputs via {}\n", cfg.observe_addr),
            })
            .ok();
            let mut client = ObserveServiceClient::new(connect(&cfg.observe_addr).await?);
            let resp = client
                .list_run_outputs(ListRunOutputsRequest {
                    run_id: Some(RunId { value: run_id }),
                    page: Some(Pagination {
                        page_size: page_size.max(1),
                        page_token,
                    }),
                    filter: Some(RunOutputFilter {
                        kind,
                        output_type: output_type.trim().to_string(),
                        path_contains: path_contains.trim().to_string(),
                        label_contains: label_contains.trim().to_string(),
                    }),
                })
                .await?
                .into_inner();

            if let Some(summary) = resp.summary.as_ref() {
                let updated = summary
                    .updated_at
                    .as_ref()
                    .map(|ts| ts.unix_millis)
                    .unwrap_or(0);
                ui.send(AppEvent::Log {
                    page,
                    line: format!(
                        "Output summary: bundles={} artifacts={} updated={} last_bundle_id={}\n",
                        summary.bundle_count,
                        summary.artifact_count,
                        updated,
                        summary.last_bundle_id
                    ),
                })
                .ok();
            }

            if resp.outputs.is_empty() {
                ui.send(AppEvent::Log {
                    page,
                    line: "No outputs recorded.\n".into(),
                })
                .ok();
            } else {
                for output in resp.outputs {
                    let job_id = output
                        .job_id
                        .as_ref()
                        .map(|id| id.value.as_str())
                        .unwrap_or("-");
                    let created_at = output
                        .created_at
                        .as_ref()
                        .map(|ts| ts.unix_millis)
                        .unwrap_or(0);
                    ui.send(AppEvent::Log {
                        page,
                        line: format!(
                            "- {} kind={} type={} path={} label={} job_id={} created_at={}\n",
                            output.output_id,
                            run_output_kind_label(output.kind),
                            output.output_type,
                            output.path,
                            output.label,
                            job_id,
                            created_at
                        ),
                    })
                    .ok();
                    if !output.metadata.is_empty() {
                        ui.send(AppEvent::Log {
                            page,
                            line: format!("  metadata: {}\n", kv_pairs(&output.metadata)),
                        })
                        .ok();
                    }
                }
            }

            if let Some(page_info) = resp.page_info {
                if !page_info.next_page_token.trim().is_empty() {
                    ui.send(AppEvent::Log {
                        page,
                        line: format!("next_page_token={}\n", page_info.next_page_token),
                    })
                    .ok();
                }
            }
        }

        UiCommand::ObserveExportSupport {
            cfg,
            include_logs,
            include_config,
            include_toolchain_provenance,
            include_recent_runs,
            recent_runs_limit,
            job_id,
            correlation_id,
            page,
        } => {
            ui.send(AppEvent::Log {
                page,
                line: format!("Connecting to ObserveService at {}\n", cfg.observe_addr),
            })
            .ok();
            let mut client = ObserveServiceClient::new(connect(&cfg.observe_addr).await?);
            let resp = client
                .export_support_bundle(ExportSupportBundleRequest {
                    include_logs,
                    include_config,
                    include_toolchain_provenance,
                    include_recent_runs,
                    recent_runs_limit,
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    project_id: None,
                    target_id: None,
                    toolchain_set_id: None,
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await?
                .into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page,
                line: format!(
                    "Support bundle job: {job_id}\nOutput path: {}\n",
                    resp.output_path
                ),
            })
            .ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                stream_tasks.spawn(async move {
                    if let Err(err) =
                        stream_job_events(job_addr, job_id.clone(), page, ui_stream).await
                    {
                        let _ = ui_err.send(AppEvent::Log {
                            page,
                            line: format!("job stream error ({job_id}): {err}\n"),
                        });
                    }
                });
            }
        }

        UiCommand::ObserveExportEvidence {
            cfg,
            run_id,
            job_id,
            correlation_id,
            page,
        } => {
            let run_id = run_id.trim().to_string();
            if run_id.is_empty() {
                ui.send(AppEvent::Log {
                    page,
                    line: "Run id is required for evidence export.\n".into(),
                })
                .ok();
                return Ok(());
            }
            ui.send(AppEvent::Log {
                page,
                line: format!("Connecting to ObserveService at {}\n", cfg.observe_addr),
            })
            .ok();
            let mut client = ObserveServiceClient::new(connect(&cfg.observe_addr).await?);
            let resp = client
                .export_evidence_bundle(ExportEvidenceBundleRequest {
                    run_id: Some(RunId {
                        value: run_id.clone(),
                    }),
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    correlation_id: correlation_id.trim().to_string(),
                })
                .await?
                .into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page,
                line: format!(
                    "Evidence bundle job: {job_id}\nOutput path: {}\n",
                    resp.output_path
                ),
            })
            .ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                stream_tasks.spawn(async move {
                    if let Err(err) =
                        stream_job_events(job_addr, job_id.clone(), page, ui_stream).await
                    {
                        let _ = ui_err.send(AppEvent::Log {
                            page,
                            line: format!("job stream error ({job_id}): {err}\n"),
                        });
                    }
                });
            }
        }

        UiCommand::StreamRunEvents {
            cfg,
            run_id,
            correlation_id,
            include_history,
            page,
        } => {
            let run_id_value = run_id.trim().to_string();
            let correlation_value = correlation_id.trim().to_string();
            if run_id_value.is_empty() && correlation_value.is_empty() {
                ui.send(AppEvent::Log {
                    page,
                    line: "run_id or correlation_id is required to stream\n".into(),
                })
                .ok();
                return Ok(());
            }
            ui.send(AppEvent::Log {
                page,
                line: format!("Streaming run events via {}\n", cfg.job_addr),
            })
            .ok();

            let job_addr = cfg.job_addr.clone();
            let ui_stream = ui.clone();
            let ui_err = ui.clone();
            stream_tasks.spawn(async move {
                if let Err(err) = stream_run_events(
                    job_addr,
                    run_id_value.clone(),
                    correlation_value.clone(),
                    include_history,
                    page,
                    ui_stream,
                )
                .await
                {
                    let _ = ui_err.send(AppEvent::Log {
                        page,
                        line: format!("run stream error: {err}\n"),
                    });
                }
            });
        }

        UiCommand::WorkflowRunPipeline {
            cfg,
            run_id,
            correlation_id,
            job_id,
            project_id,
            project_path,
            project_name,
            template_id,
            toolchain_id,
            toolchain_set_id,
            target_id,
            build_variant,
            module,
            variant_name,
            tasks,
            apk_path,
            application_id,
            activity,
            options,
            stream_history,
        } => {
            ui.send(AppEvent::Log {
                page: "workflow",
                line: format!("Connecting to WorkflowService at {}\n", cfg.workflow_addr),
            })
            .ok();
            let channel = match connect(&cfg.workflow_addr).await {
                Ok(channel) => channel,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "workflow",
                        line: format!("WorkflowService connection failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let mut client = WorkflowServiceClient::new(channel);

            let resp = match client
                .run_pipeline(WorkflowPipelineRequest {
                    run_id: run_id_from_optional(&run_id),
                    correlation_id: correlation_id.trim().to_string(),
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    project_id: to_optional_id(&project_id),
                    project_path: project_path.trim().to_string(),
                    project_name: project_name.trim().to_string(),
                    template_id: to_optional_id(&template_id),
                    toolchain_id: to_optional_id(&toolchain_id),
                    toolchain_set_id: to_optional_id(&toolchain_set_id),
                    target_id: to_optional_id(&target_id),
                    build_variant: build_variant as i32,
                    module,
                    variant_name,
                    tasks,
                    apk_path,
                    application_id,
                    activity,
                    options,
                })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "workflow",
                        line: format!("RunPipeline failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            let run_id_value = resp
                .run_id
                .as_ref()
                .map(|id| id.value.clone())
                .unwrap_or_default();
            let job_id_value = resp
                .job_id
                .as_ref()
                .map(|id| id.value.clone())
                .unwrap_or_default();
            let project_id_value = resp
                .project_id
                .as_ref()
                .map(|id| id.value.clone())
                .unwrap_or_default();
            ui.send(AppEvent::Log {
                page: "workflow",
                line: format!(
                    "Pipeline started: run_id={} job_id={} project_id={}\n",
                    run_id_value, job_id_value, project_id_value
                ),
            })
            .ok();
            let project_id_update = if !project_id_value.trim().is_empty() {
                Some(project_id_value.clone())
            } else if !project_id.trim().is_empty() {
                Some(project_id.trim().to_string())
            } else {
                None
            };
            let project_path_update =
                if project_id_update.is_some() || !project_path.trim().is_empty() {
                    Some(project_path.trim().to_string())
                } else {
                    None
                };
            let toolchain_set_update = if !toolchain_set_id.trim().is_empty() {
                Some(toolchain_set_id.trim().to_string())
            } else {
                None
            };
            let target_update = if !target_id.trim().is_empty() {
                Some(target_id.trim().to_string())
            } else {
                None
            };
            let run_id_update = if !run_id_value.trim().is_empty() {
                Some(run_id_value.clone())
            } else if !run_id.trim().is_empty() {
                Some(run_id.trim().to_string())
            } else {
                None
            };
            if project_id_update.is_some()
                || project_path_update.is_some()
                || toolchain_set_update.is_some()
                || target_update.is_some()
                || run_id_update.is_some()
            {
                ui.send(AppEvent::UpdateActiveContext {
                    project_id: project_id_update,
                    project_path: project_path_update,
                    toolchain_set_id: toolchain_set_update,
                    target_id: target_update,
                    run_id: run_id_update,
                })
                .ok();
            }
            if !resp.outputs.is_empty() {
                ui.send(AppEvent::Log {
                    page: "workflow",
                    line: format!("Outputs: {}\n", kv_pairs(&resp.outputs)),
                })
                .ok();
            }

            let stream_run_id = if run_id_value.trim().is_empty() {
                run_id.trim().to_string()
            } else {
                run_id_value.clone()
            };
            let stream_corr = correlation_id.trim().to_string();
            if stream_run_id.is_empty() && stream_corr.is_empty() {
                ui.send(AppEvent::Log {
                    page: "workflow",
                    line: "No run_id or correlation_id to stream.\n".into(),
                })
                .ok();
                return Ok(());
            }

            let job_addr = cfg.job_addr.clone();
            let ui_stream = ui.clone();
            let ui_err = ui.clone();
            stream_tasks.spawn(async move {
                if let Err(err) = stream_run_events(
                    job_addr,
                    stream_run_id.clone(),
                    stream_corr.clone(),
                    stream_history,
                    "workflow",
                    ui_stream,
                )
                .await
                {
                    let _ = ui_err.send(AppEvent::Log {
                        page: "workflow",
                        line: format!("run stream error: {err}\n"),
                    });
                }
            });
        }

        UiCommand::BuildRun {
            cfg,
            project_ref,
            variant,
            variant_name,
            module,
            tasks,
            clean_first,
            gradle_args,
            job_id,
            correlation_id,
        } => {
            if project_ref.trim().is_empty() {
                ui.send(AppEvent::Log {
                    page: "console",
                    line: "Project path or id is required.\n".into(),
                })
                .ok();
                return Ok(());
            }

            ui.send(AppEvent::Log {
                page: "console",
                line: format!("Connecting to BuildService at {}\n", cfg.build_addr),
            })
            .ok();
            let channel = match connect(&cfg.build_addr).await {
                Ok(channel) => channel,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "console",
                        line: format!("BuildService connection failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };
            let mut client = BuildServiceClient::new(channel);

            let resp = match client
                .build(BuildRequest {
                    project_id: Some(Id {
                        value: project_ref.trim().to_string(),
                    }),
                    variant: variant as i32,
                    clean_first,
                    gradle_args,
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id {
                            value: value.clone(),
                        }),
                    module,
                    variant_name,
                    tasks,
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "console",
                        line: format!("Build request failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            if job_id.is_empty() {
                ui.send(AppEvent::Log {
                    page: "console",
                    line: "Build returned empty job_id.\n".into(),
                })
                .ok();
                return Ok(());
            }
            ui.send(AppEvent::Log {
                page: "console",
                line: format!("Build started: job_id={job_id}\n"),
            })
            .ok();

            let job_addr = cfg.job_addr.clone();
            let ui_stream = ui.clone();
            let ui_err = ui.clone();
            stream_tasks.spawn(async move {
                if let Err(err) =
                    stream_job_events(job_addr, job_id.clone(), "console", ui_stream).await
                {
                    let _ = ui_err.send(AppEvent::Log {
                        page: "console",
                        line: format!("job stream error ({job_id}): {err}\n"),
                    });
                }
            });
        }

        UiCommand::BuildListArtifacts {
            cfg,
            project_ref,
            variant,
            filter,
        } => {
            if project_ref.trim().is_empty() {
                ui.send(AppEvent::Log {
                    page: "console",
                    line: "Project path or id is required.\n".into(),
                })
                .ok();
                return Ok(());
            }

            ui.send(AppEvent::Log {
                page: "console",
                line: format!("Listing artifacts via {}\n", cfg.build_addr),
            })
            .ok();
            let mut client = BuildServiceClient::new(connect(&cfg.build_addr).await?);
            let resp = match client
                .list_artifacts(ListArtifactsRequest {
                    project_id: Some(Id {
                        value: project_ref.trim().to_string(),
                    }),
                    variant: variant as i32,
                    filter: Some(filter),
                })
                .await
            {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "console",
                        line: format!("List artifacts failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            if resp.artifacts.is_empty() {
                ui.send(AppEvent::Log {
                    page: "console",
                    line: "No artifacts found.\n".into(),
                })
                .ok();
                return Ok(());
            }

            let total = resp.artifacts.len();
            let mut by_module: BTreeMap<String, Vec<Artifact>> = BTreeMap::new();
            for artifact in resp.artifacts {
                let module = metadata_value(&artifact.metadata, "module").unwrap_or("");
                let label = if module.trim().is_empty() {
                    "<root>".to_string()
                } else {
                    module.to_string()
                };
                by_module.entry(label).or_default().push(artifact);
            }

            ui.send(AppEvent::Log {
                page: "console",
                line: format!("Artifacts ({total})\n"),
            })
            .ok();
            for (module, artifacts) in by_module {
                ui.send(AppEvent::Log {
                    page: "console",
                    line: format!("Module: {module}\n"),
                })
                .ok();
                for artifact in artifacts {
                    ui.send(AppEvent::Log {
                        page: "console",
                        line: format_artifact_line(&artifact),
                    })
                    .ok();
                }
            }
        }

        UiCommand::StateSave {
            cfg,
            output_path,
            exclude_downloads,
            exclude_toolchains,
            exclude_bundles,
            exclude_telemetry,
        } => {
            ui.send(AppEvent::Log {
                page: "settings",
                line: "Saving local AADK state...\n".into(),
            })
            .ok();
            match latest_active_job(&cfg.job_addr).await {
                Ok(Some(job)) => {
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!("State operation blocked: {}\n", format_active_job(&job)),
                    })
                    .ok();
                    return Ok(());
                }
                Ok(None) => {}
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!("State operation blocked: job check failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            }
            ui.send(AppEvent::Log {
                page: "settings",
                line: "Waiting for state-op FIFO...\n".into(),
            })
            .ok();
            let _guard = match StateOpGuard::acquire("ui.save") {
                Ok(guard) => guard,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!("State queue error: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            let opts = StateArchiveOptions {
                exclude_downloads,
                exclude_toolchains,
                exclude_bundles,
                exclude_telemetry,
            };
            let output_path = if output_path.trim().is_empty() {
                state_export_path()
            } else {
                expand_user(&output_path)
            };
            match save_state_archive_to(&output_path, &opts) {
                Ok(result) => {
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!("Saved archive: {}\n", result.output_path.display()),
                    })
                    .ok();
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!(
                            "Archive contents: files={} dirs={} bytes={}\n",
                            result.file_count, result.dir_count, result.total_bytes
                        ),
                    })
                    .ok();
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!("Exclusions: {}\n", format_state_exclusions(&opts)),
                    })
                    .ok();
                }
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!("Save state failed: {err}\n"),
                    })
                    .ok();
                }
            }
        }

        UiCommand::StateOpen {
            cfg,
            archive_path,
            exclude_downloads,
            exclude_toolchains,
            exclude_bundles,
            exclude_telemetry,
        } => {
            if archive_path.trim().is_empty() {
                ui.send(AppEvent::Log {
                    page: "settings",
                    line: "Open state requires a zip path.\n".into(),
                })
                .ok();
                return Ok(());
            }
            ui.send(AppEvent::Log {
                page: "settings",
                line: "Opening AADK state archive...\n".into(),
            })
            .ok();
            match latest_active_job(&cfg.job_addr).await {
                Ok(Some(job)) => {
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!("State operation blocked: {}\n", format_active_job(&job)),
                    })
                    .ok();
                    return Ok(());
                }
                Ok(None) => {}
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!("State operation blocked: job check failed: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            }
            ui.send(AppEvent::Log {
                page: "settings",
                line: "Waiting for state-op FIFO...\n".into(),
            })
            .ok();
            let _guard = match StateOpGuard::acquire("ui.open") {
                Ok(guard) => guard,
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!("State queue error: {err}\n"),
                    })
                    .ok();
                    return Ok(());
                }
            };

            let opts = StateArchiveOptions {
                exclude_downloads,
                exclude_toolchains,
                exclude_bundles,
                exclude_telemetry,
            };
            let archive_path = expand_user(&archive_path);
            match open_state_archive(&archive_path, &opts) {
                Ok(result) => {
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!("Opened archive: {}\n", archive_path.display()),
                    })
                    .ok();
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!(
                            "Archive restored: files={} dirs={}\n",
                            result.restored_files, result.restored_dirs
                        ),
                    })
                    .ok();
                    if !result.preserved_dirs.is_empty() {
                        ui.send(AppEvent::Log {
                            page: "settings",
                            line: format!("Preserved: {}\n", result.preserved_dirs.join(", ")),
                        })
                        .ok();
                    }
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!("Exclusions: {}\n", format_state_exclusions(&opts)),
                    })
                    .ok();

                    let new_cfg = AppConfig::load();
                    if let Err(err) = reload_all_state(&new_cfg, &ui).await {
                        ui.send(AppEvent::Log {
                            page: "settings",
                            line: format!("Reload state failed: {err}\n"),
                        })
                        .ok();
                    }
                    ui.send(AppEvent::ConfigReloaded {
                        cfg: Box::new(new_cfg),
                    })
                    .ok();
                }
                Err(err) => {
                    ui.send(AppEvent::Log {
                        page: "settings",
                        line: format!("Open state failed: {err}\n"),
                    })
                    .ok();
                }
            }
        }

        UiCommand::StateReload { cfg } => {
            ui.send(AppEvent::Log {
                page: "settings",
                line: "Reloading services from disk...\n".into(),
            })
            .ok();
            if let Err(err) = reload_all_state(&cfg, &ui).await {
                ui.send(AppEvent::Log {
                    page: "settings",
                    line: format!("Reload state failed: {err}\n"),
                })
                .ok();
            }
        }

        UiCommand::ResetAllState { cfg } => {
            let _ = cfg;
            let data_dir = aadk_util::data_dir();
            let dir_label = data_dir.display();
            ui.send(AppEvent::Log {
                page: "settings",
                line: format!("Resetting AADK state under {dir_label}\n"),
            })
            .ok();

            let is_safe = data_dir
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name == "aadk")
                .unwrap_or(false);
            if !is_safe {
                ui.send(AppEvent::Log {
                    page: "settings",
                    line: format!("Refusing to reset unexpected path: {dir_label}\n"),
                })
                .ok();
                ui.send(AppEvent::ResetAllStateComplete { ok: false }).ok();
                return Ok(());
            }

            if let Err(err) = reset_local_state(&data_dir) {
                ui.send(AppEvent::Log {
                    page: "settings",
                    line: format!("Reset failed: {err}\n"),
                })
                .ok();
                ui.send(AppEvent::ResetAllStateComplete { ok: false }).ok();
                return Ok(());
            }

            if let Some(handle) = worker_state.home_stream.take() {
                handle.abort();
            }
            worker_state.current_job_id = None;
            ui.send(AppEvent::SetCurrentJob { job_id: None }).ok();
            ui.send(AppEvent::HomeResetStatus).ok();
            ui.send(AppEvent::Log {
                page: "settings",
                line: "Reset complete.\n".into(),
            })
            .ok();
            ui.send(AppEvent::ResetAllStateComplete { ok: true }).ok();
        }
    }

    Ok(())
}

fn start_home_stream(
    worker_state: &mut AppState,
    stream_tasks: &mut tokio::task::JoinSet<()>,
    job_addr: String,
    job_id: String,
    ui: UiEventSender,
) {
    if let Some(handle) = worker_state.home_stream.take() {
        handle.abort();
    }
    let job_id_for_log = job_id.clone();
    let ui_err = ui.clone();
    let abort = stream_tasks.spawn(async move {
        if let Err(err) = stream_job_events_home(&job_addr, &job_id, ui.clone()).await {
            let _ = ui_err.send(AppEvent::Log {
                page: "home",
                line: format!("job stream error ({job_id_for_log}): {err}\n"),
            });
        }
    });
    worker_state.home_stream = Some(abort);
}

fn reset_local_state(data_dir: &Path) -> io::Result<()> {
    const PRESERVE_DIRS: [&str; 3] = ["toolchains", "downloads", "cuttlefish"];
    let entries = match fs::read_dir(data_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if PRESERVE_DIRS.contains(&name_str.as_ref()) {
            continue;
        }
        let path = entry.path();
        if name_str == "state" {
            clear_state_dir(&path)?;
            continue;
        }
        remove_path(&path)?;
    }

    Ok(())
}

fn clear_state_dir(state_dir: &Path) -> io::Result<()> {
    const PRESERVE_FILES: [&str; 1] = ["toolchains.json"];
    let entries = match fs::read_dir(state_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if PRESERVE_FILES.contains(&name_str.as_ref()) {
            continue;
        }
        remove_path(&entry.path())?;
    }

    Ok(())
}

fn remove_path(path: &Path) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if meta.file_type().is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

async fn stream_job_events_home(
    addr: &str,
    job_id: &str,
    ui: UiEventSender,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = JobServiceClient::new(connect(addr).await?);
    let mut stream = client
        .stream_job_events(StreamJobEventsRequest {
            job_id: Some(Id {
                value: job_id.to_string(),
            }),
            include_history: true,
        })
        .await?
        .into_inner();
    let mut last_progress: Option<(u32, Instant)> = None;
    let progress_throttle = Duration::from_millis(250);

    while let Some(item) = stream.next().await {
        match item {
            Ok(evt) => {
                if let Some(payload) = evt.payload.as_ref() {
                    match payload {
                        JobPayload::StateChanged(state) => {
                            let state = JobState::try_from(state.new_state)
                                .unwrap_or(JobState::Unspecified);
                            ui.send(AppEvent::HomeState {
                                state: format!("{state:?}"),
                            })
                            .ok();
                            if matches!(
                                state,
                                JobState::Success | JobState::Failed | JobState::Cancelled
                            ) {
                                ui.send(AppEvent::HomeResult {
                                    result: format!("{state:?}"),
                                })
                                .ok();
                            }
                        }
                        JobPayload::Progress(progress) => {
                            if let Some(p) = progress.progress.as_ref() {
                                let now = Instant::now();
                                let should_emit = match last_progress {
                                    Some((last_percent, last_at)) => {
                                        p.percent != last_percent
                                            || now.duration_since(last_at) >= progress_throttle
                                    }
                                    None => true,
                                };
                                if should_emit {
                                    ui.send(AppEvent::HomeProgress {
                                        progress: format!("{}% {}", p.percent, p.phase),
                                    })
                                    .ok();
                                    last_progress = Some((p.percent, now));
                                }
                            }
                        }
                        JobPayload::Completed(completed) => {
                            ui.send(AppEvent::HomeState {
                                state: "Success".into(),
                            })
                            .ok();
                            ui.send(AppEvent::HomeResult {
                                result: completed.summary.clone(),
                            })
                            .ok();
                        }
                        JobPayload::Failed(failed) => {
                            let message = failed
                                .error
                                .as_ref()
                                .map(|err| err.message.clone())
                                .unwrap_or_else(|| "failed".into());
                            ui.send(AppEvent::HomeState {
                                state: "Failed".into(),
                            })
                            .ok();
                            ui.send(AppEvent::HomeResult { result: message }).ok();
                        }
                        JobPayload::Log(_) => {}
                    }
                }
                for line in job_event_lines(&evt) {
                    ui.send(AppEvent::Log { page: "home", line }).ok();
                }
                if let Some(JobPayload::Completed(_)) = evt.payload.as_ref() {
                    break;
                }
                if let Some(JobPayload::Failed(_)) = evt.payload.as_ref() {
                    break;
                }
                if let Some(JobPayload::StateChanged(state)) = evt.payload.as_ref() {
                    let state =
                        JobState::try_from(state.new_state).unwrap_or(JobState::Unspecified);
                    if matches!(state, JobState::Cancelled) {
                        break;
                    }
                }
            }
            Err(err) => {
                ui.send(AppEvent::Log {
                    page: "home",
                    line: format!("job stream error: {err}\n"),
                })
                .ok();
                break;
            }
        }
    }

    Ok(())
}

async fn stream_job_events(
    addr: String,
    job_id: String,
    page: &'static str,
    ui: UiEventSender,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = JobServiceClient::new(connect(&addr).await?);
    let mut stream = client
        .stream_job_events(StreamJobEventsRequest {
            job_id: Some(Id {
                value: job_id.clone(),
            }),
            include_history: true,
        })
        .await?
        .into_inner();

    while let Some(item) = stream.next().await {
        match item {
            Ok(evt) => {
                if let Some(JobPayload::Completed(completed)) = evt.payload.as_ref() {
                    if page == "console" {
                        if let Some(apk) = completed.outputs.iter().find(|kv| kv.key == "apk_path")
                        {
                            let _ = ui.send(AppEvent::SetLastBuildApk {
                                apk_path: apk.value.clone(),
                            });
                        }
                    }
                    if page == "targets" {
                        if let Some(adb_serial) =
                            completed.outputs.iter().find(|kv| kv.key == "adb_serial")
                        {
                            let trimmed = adb_serial.value.trim();
                            if !trimmed.is_empty() {
                                let _ = ui.send(AppEvent::UpdateActiveContext {
                                    project_id: None,
                                    project_path: None,
                                    toolchain_set_id: None,
                                    target_id: Some(trimmed.to_string()),
                                    run_id: None,
                                });
                            }
                        }
                    }
                }
                for line in stream_job_event_lines(&job_id, &evt) {
                    ui.send(AppEvent::Log { page, line }).ok();
                }
            }
            Err(err) => {
                ui.send(AppEvent::Log {
                    page,
                    line: format!("job {job_id} stream error: {err}\n"),
                })
                .ok();
                break;
            }
        }
    }

    Ok(())
}

async fn stream_run_events(
    addr: String,
    run_id: String,
    correlation_id: String,
    include_history: bool,
    page: &'static str,
    ui: UiEventSender,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = JobServiceClient::new(connect(&addr).await?);
    let mut stream = client
        .stream_run_events(StreamRunEventsRequest {
            run_id: run_id_from_optional(&run_id),
            correlation_id,
            include_history,
            buffer_max_events: 0,
            max_delay_ms: 0,
            discovery_interval_ms: 0,
        })
        .await?
        .into_inner();

    while let Some(item) = stream.next().await {
        match item {
            Ok(evt) => {
                for line in run_stream_lines(&evt) {
                    ui.send(AppEvent::Log { page, line }).ok();
                }
            }
            Err(err) => {
                ui.send(AppEvent::Log {
                    page,
                    line: format!("run stream error: {err}\n"),
                })
                .ok();
                break;
            }
        }
    }

    Ok(())
}

async fn connect(addr: &str) -> Result<Channel, Box<dyn std::error::Error>> {
    let endpoint = format!("http://{addr}");
    Ok(Channel::from_shared(endpoint)?.connect().await?)
}
