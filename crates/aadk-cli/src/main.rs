use std::{
    collections::BTreeMap,
    fs,
    io,
    io::Write,
    path::{Path, PathBuf},
    time::SystemTime,
};

use aadk_proto::aadk::v1::{
    build_service_client::BuildServiceClient,
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    observe_service_client::ObserveServiceClient,
    project_service_client::ProjectServiceClient,
    toolchain_service_client::ToolchainServiceClient,
    target_service_client::TargetServiceClient,
    workflow_service_client::WorkflowServiceClient,
    Artifact, ArtifactFilter, ArtifactType, BuildRequest, BuildVariant, CancelJobRequest,
    CleanupToolchainCacheRequest, CreateProjectRequest, CreateToolchainSetRequest,
    ExportEvidenceBundleRequest, ExportSupportBundleRequest, GetActiveToolchainSetRequest,
    GetCuttlefishStatusRequest, GetDefaultTargetRequest, GetJobRequest, Id,
    InstallCuttlefishRequest, Job, JobEvent, JobEventKind, JobFilter, JobHistoryFilter, JobState,
    KeyValue, ListArtifactsRequest, ListJobHistoryRequest, ListJobsRequest, ListProvidersRequest,
    ListRecentProjectsRequest, ListRunOutputsRequest, ListRunsRequest, ListTargetsRequest,
    ListTemplatesRequest, ListToolchainSetsRequest, OpenProjectRequest, Pagination, RunFilter,
    RunId, RunOutputFilter, RunOutputKind,
    SetActiveToolchainSetRequest, SetDefaultTargetRequest, SetProjectConfigRequest,
    StartCuttlefishRequest, StartJobRequest, StopCuttlefishRequest, StreamJobEventsRequest,
    StreamRunEventsRequest, UninstallToolchainRequest, UpdateToolchainRequest,
    WorkflowPipelineOptions, WorkflowPipelineRequest,
};
use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tonic::transport::Channel;

#[derive(Parser)]
#[command(name = "aadk-cli", version, about = "AADK scaffold CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Job-related commands (JobService)
    Job {
        #[command(subcommand)]
        cmd: JobCmd,
    },
    /// Toolchain-related commands (ToolchainService)
    Toolchain {
        #[command(subcommand)]
        cmd: ToolchainCmd,
    },
    /// Target-related commands (TargetService)
    Targets {
        #[command(subcommand)]
        cmd: TargetsCmd,
    },
    /// Project-related commands (ProjectService)
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Observe-related commands (ObserveService)
    Observe {
        #[command(subcommand)]
        cmd: ObserveCmd,
    },
    /// Build-related commands (BuildService)
    Build {
        #[command(subcommand)]
        cmd: BuildCmd,
    },
    /// Workflow-related commands (WorkflowService)
    Workflow {
        #[command(subcommand)]
        cmd: WorkflowCmd,
    },
}

#[derive(Subcommand)]
enum JobCmd {
    /// Run a job by type and optionally stream events
    Run {
        #[arg(long, default_value_t = default_job_addr())]
        addr: String,
        job_type: String,
        #[arg(long)]
        param: Vec<String>,
        #[arg(long)]
        project_id: Option<String>,
        #[arg(long)]
        target_id: Option<String>,
        #[arg(long)]
        toolchain_set_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        no_stream: bool,
    },
    /// List jobs with optional filters
    List {
        #[arg(long, default_value_t = default_job_addr())]
        addr: String,
        #[arg(long)]
        job_type: Vec<String>,
        #[arg(long)]
        state: Vec<String>,
        #[arg(long)]
        created_after: Option<i64>,
        #[arg(long)]
        created_before: Option<i64>,
        #[arg(long)]
        finished_after: Option<i64>,
        #[arg(long)]
        finished_before: Option<i64>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long, default_value_t = 50)]
        page_size: u32,
        #[arg(long, default_value = "")]
        page_token: String,
    },
    /// Watch a job stream by id
    Watch {
        #[arg(long, default_value_t = default_job_addr())]
        addr: String,
        job_id: String,
        #[arg(long)]
        include_history: bool,
    },
    /// Watch job events by run or correlation id (aggregated stream)
    WatchRun {
        #[arg(long, default_value_t = default_job_addr())]
        addr: String,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        include_history: bool,
        #[arg(long)]
        buffer_max_events: Option<u32>,
        #[arg(long)]
        max_delay_ms: Option<u64>,
        #[arg(long)]
        discovery_interval_ms: Option<u64>,
    },
    /// List job event history by id
    History {
        #[arg(long, default_value_t = default_job_addr())]
        addr: String,
        job_id: String,
        #[arg(long)]
        kind: Vec<String>,
        #[arg(long)]
        after: Option<i64>,
        #[arg(long)]
        before: Option<i64>,
        #[arg(long, default_value_t = 200)]
        page_size: u32,
        #[arg(long, default_value = "")]
        page_token: String,
    },
    /// Export job logs with config snapshot
    Export {
        #[arg(long, default_value_t = default_job_addr())]
        addr: String,
        job_id: String,
        #[arg(long)]
        output: Option<String>,
    },
    /// Cancel a job by id
    Cancel {
        #[arg(long, default_value_t = default_job_addr())]
        addr: String,
        job_id: String,
    },
}

#[derive(Subcommand)]
enum ToolchainCmd {
    /// List toolchain providers
    ListProviders {
        #[arg(long, default_value_t = default_toolchain_addr())]
        addr: String,
    },
    /// List toolchain sets
    ListSets {
        #[arg(long, default_value_t = default_toolchain_addr())]
        addr: String,
        #[arg(long, default_value_t = 25)]
        page_size: u32,
        #[arg(long, default_value = "")]
        page_token: String,
    },
    /// Create a toolchain set from installed SDK/NDK ids
    CreateSet {
        #[arg(long, default_value_t = default_toolchain_addr())]
        addr: String,
        #[arg(long)]
        sdk_toolchain_id: Option<String>,
        #[arg(long)]
        ndk_toolchain_id: Option<String>,
        #[arg(long)]
        display_name: Option<String>,
    },
    /// Set the active toolchain set
    SetActive {
        #[arg(long, default_value_t = default_toolchain_addr())]
        addr: String,
        toolchain_set_id: String,
    },
    /// Get the active toolchain set
    GetActive {
        #[arg(long, default_value_t = default_toolchain_addr())]
        addr: String,
    },
    /// Update an installed toolchain to a new version
    Update {
        #[arg(long, default_value_t = default_toolchain_addr())]
        addr: String,
        #[arg(long, default_value_t = default_job_addr())]
        job_addr: String,
        toolchain_id: String,
        #[arg(long)]
        version: String,
        #[arg(long, default_value_t = true)]
        verify_hash: bool,
        #[arg(long)]
        remove_cached: bool,
        #[arg(long)]
        job_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        no_stream: bool,
    },
    /// Uninstall a toolchain
    Uninstall {
        #[arg(long, default_value_t = default_toolchain_addr())]
        addr: String,
        #[arg(long, default_value_t = default_job_addr())]
        job_addr: String,
        toolchain_id: String,
        #[arg(long)]
        remove_cached: bool,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        job_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        no_stream: bool,
    },
    /// Cleanup cached toolchain artifacts
    CleanupCache {
        #[arg(long, default_value_t = default_toolchain_addr())]
        addr: String,
        #[arg(long, default_value_t = default_job_addr())]
        job_addr: String,
        #[arg(long, default_value_t = true)]
        dry_run: bool,
        #[arg(long)]
        remove_all: bool,
        #[arg(long)]
        job_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        no_stream: bool,
    },
}

#[derive(Subcommand)]
enum TargetsCmd {
    /// List targets
    List {
        #[arg(long, default_value_t = default_targets_addr())]
        addr: String,
    },
    /// Set the default target id
    SetDefault {
        #[arg(long, default_value_t = default_targets_addr())]
        addr: String,
        target_id: String,
    },
    /// Get the configured default target
    GetDefault {
        #[arg(long, default_value_t = default_targets_addr())]
        addr: String,
    },
    /// Start Cuttlefish and return a job id
    StartCuttlefish {
        #[arg(long, default_value_t = default_targets_addr())]
        addr: String,
        #[arg(long)]
        show_full_ui: bool,
        #[arg(long)]
        job_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        run_id: Option<String>,
    },
    /// Stop Cuttlefish and return a job id
    StopCuttlefish {
        #[arg(long, default_value_t = default_targets_addr())]
        addr: String,
        #[arg(long)]
        job_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        run_id: Option<String>,
    },
    /// Get Cuttlefish status
    CuttlefishStatus {
        #[arg(long, default_value_t = default_targets_addr())]
        addr: String,
    },
    /// Install Cuttlefish using the configured installer
    InstallCuttlefish {
        #[arg(long, default_value_t = default_targets_addr())]
        addr: String,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        job_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        run_id: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// List available project templates
    ListTemplates {
        #[arg(long, default_value_t = default_project_addr())]
        addr: String,
    },
    /// List recent projects
    ListRecent {
        #[arg(long, default_value_t = default_project_addr())]
        addr: String,
        #[arg(long, default_value_t = 25)]
        page_size: u32,
        #[arg(long, default_value = "")]
        page_token: String,
    },
    /// Create a new project from a template and stream job events
    Create {
        #[arg(long, default_value_t = default_project_addr())]
        addr: String,
        #[arg(long, default_value_t = default_job_addr())]
        job_addr: String,
        name: String,
        path: String,
        #[arg(long)]
        template_id: String,
        #[arg(long)]
        job_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        run_id: Option<String>,
    },
    /// Open an existing project
    Open {
        #[arg(long, default_value_t = default_project_addr())]
        addr: String,
        path: String,
    },
    /// Update project toolchain/target defaults
    SetConfig {
        #[arg(long, default_value_t = default_project_addr())]
        addr: String,
        project_id: String,
        #[arg(long)]
        toolchain_set_id: Option<String>,
        #[arg(long)]
        default_target_id: Option<String>,
    },
    /// Apply active toolchain set + default target to a project
    UseActiveDefaults {
        #[arg(long, default_value_t = default_project_addr())]
        addr: String,
        #[arg(long, default_value_t = default_toolchain_addr())]
        toolchain_addr: String,
        #[arg(long, default_value_t = default_targets_addr())]
        targets_addr: String,
        project_id: String,
    },
}

#[derive(Subcommand)]
enum ObserveCmd {
    /// List recorded runs
    ListRuns {
        #[arg(long, default_value_t = default_observe_addr())]
        addr: String,
        #[arg(long, default_value_t = 25)]
        page_size: u32,
        #[arg(long, default_value = "")]
        page_token: String,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        project_id: Option<String>,
        #[arg(long)]
        target_id: Option<String>,
        #[arg(long)]
        toolchain_set_id: Option<String>,
        #[arg(long)]
        result: Option<String>,
    },
    /// List outputs for a run
    ListOutputs {
        #[arg(long, default_value_t = default_observe_addr())]
        addr: String,
        run_id: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        output_type: Option<String>,
        #[arg(long)]
        path_contains: Option<String>,
        #[arg(long)]
        label_contains: Option<String>,
        #[arg(long, default_value_t = 50)]
        page_size: u32,
        #[arg(long, default_value = "")]
        page_token: String,
    },
    /// Export a support bundle and stream job events
    ExportSupport {
        #[arg(long, default_value_t = default_observe_addr())]
        addr: String,
        #[arg(long, default_value_t = default_job_addr())]
        job_addr: String,
        #[arg(long, default_value_t = true)]
        include_logs: bool,
        #[arg(long, default_value_t = true)]
        include_config: bool,
        #[arg(long, default_value_t = true)]
        include_toolchain_provenance: bool,
        #[arg(long, default_value_t = true)]
        include_recent_runs: bool,
        #[arg(long, default_value_t = 10)]
        recent_runs_limit: u32,
        #[arg(long)]
        job_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        project_id: Option<String>,
        #[arg(long)]
        target_id: Option<String>,
        #[arg(long)]
        toolchain_set_id: Option<String>,
    },
    /// Export an evidence bundle for a run id
    ExportEvidence {
        #[arg(long, default_value_t = default_observe_addr())]
        addr: String,
        #[arg(long, default_value_t = default_job_addr())]
        job_addr: String,
        run_id: Option<String>,
        #[arg(long)]
        job_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
    },
}

const CLI_CONFIG_FILE: &str = "cli-config.json";

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct CliConfig {
    job_addr: String,
    toolchain_addr: String,
    targets_addr: String,
    project_addr: String,
    build_addr: String,
    observe_addr: String,
    workflow_addr: String,
    last_job_id: String,
    last_job_type: String,
}

#[derive(Serialize)]
struct CliLogExport {
    exported_at_unix_millis: i64,
    job_id: String,
    config: CliConfig,
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

fn data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/aadk")
    } else {
        PathBuf::from("/tmp/aadk")
    }
}

fn config_path() -> PathBuf {
    data_dir().join("state").join(CLI_CONFIG_FILE)
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

fn load_cli_config() -> CliConfig {
    let path = config_path();
    match fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str::<CliConfig>(&data) {
            Ok(cfg) => cfg,
            Err(err) => {
                eprintln!("Failed to parse {}: {err}", path.display());
                CliConfig::default()
            }
        },
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                eprintln!("Failed to read {}: {err}", path.display());
            }
            CliConfig::default()
        }
    }
}

fn save_cli_config(cfg: &CliConfig) -> io::Result<()> {
    write_json_atomic(&config_path(), cfg)
}

fn update_cli_config(update: impl FnOnce(&mut CliConfig)) {
    let mut cfg = load_cli_config();
    update(&mut cfg);
    if let Err(err) = save_cli_config(&cfg) {
        eprintln!("Failed to persist CLI config: {err}");
    }
}

#[derive(Subcommand)]
enum BuildCmd {
    /// Run a Gradle build
    Run {
        #[arg(long, default_value_t = default_build_addr())]
        addr: String,
        #[arg(long, default_value_t = default_job_addr())]
        job_addr: String,
        project_ref: String,
        #[arg(long, default_value = "debug")]
        variant: String,
        #[arg(long)]
        variant_name: Option<String>,
        #[arg(long)]
        module: Option<String>,
        #[arg(long, action = clap::ArgAction::Append)]
        task: Vec<String>,
        #[arg(long, action = clap::ArgAction::Append)]
        gradle_arg: Vec<String>,
        #[arg(long)]
        clean_first: bool,
        #[arg(long)]
        job_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        no_stream: bool,
    },
    /// List build artifacts
    ListArtifacts {
        #[arg(long, default_value_t = default_build_addr())]
        addr: String,
        project_ref: String,
        #[arg(long, default_value = "unspecified")]
        variant: String,
        #[arg(long, value_delimiter = ',')]
        module: Vec<String>,
        #[arg(long)]
        variant_name: Option<String>,
        #[arg(long, value_delimiter = ',')]
        artifact_type: Vec<String>,
        #[arg(long, default_value = "")]
        name_contains: String,
        #[arg(long, default_value = "")]
        path_contains: String,
    },
}

#[derive(Subcommand)]
enum WorkflowCmd {
    /// Run the workflow pipeline
    RunPipeline {
        #[arg(long, default_value_t = default_workflow_addr())]
        addr: String,
        #[arg(long, default_value_t = default_job_addr())]
        job_addr: String,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        correlation_id: Option<String>,
        #[arg(long)]
        job_id: Option<String>,
        #[arg(long)]
        project_id: Option<String>,
        #[arg(long)]
        project_path: Option<String>,
        #[arg(long)]
        project_name: Option<String>,
        #[arg(long)]
        template_id: Option<String>,
        #[arg(long)]
        toolchain_id: Option<String>,
        #[arg(long)]
        toolchain_set_id: Option<String>,
        #[arg(long)]
        target_id: Option<String>,
        #[arg(long, default_value = "debug")]
        variant: String,
        #[arg(long)]
        module: Option<String>,
        #[arg(long)]
        variant_name: Option<String>,
        #[arg(long, action = clap::ArgAction::Append)]
        task: Vec<String>,
        #[arg(long)]
        apk_path: Option<String>,
        #[arg(long)]
        application_id: Option<String>,
        #[arg(long)]
        activity: Option<String>,
        #[arg(long, value_delimiter = ',')]
        step: Vec<String>,
        #[arg(long)]
        stream_run: bool,
        #[arg(long)]
        no_stream: bool,
    },
}

fn default_job_addr() -> String {
    if let Ok(val) = std::env::var("AADK_JOB_ADDR") {
        return val;
    }
    let cfg = load_cli_config();
    if !cfg.job_addr.trim().is_empty() {
        return cfg.job_addr;
    }
    "127.0.0.1:50051".into()
}
fn default_toolchain_addr() -> String {
    if let Ok(val) = std::env::var("AADK_TOOLCHAIN_ADDR") {
        return val;
    }
    let cfg = load_cli_config();
    if !cfg.toolchain_addr.trim().is_empty() {
        return cfg.toolchain_addr;
    }
    "127.0.0.1:50052".into()
}
fn default_targets_addr() -> String {
    if let Ok(val) = std::env::var("AADK_TARGETS_ADDR") {
        return val;
    }
    let cfg = load_cli_config();
    if !cfg.targets_addr.trim().is_empty() {
        return cfg.targets_addr;
    }
    "127.0.0.1:50055".into()
}
fn default_project_addr() -> String {
    if let Ok(val) = std::env::var("AADK_PROJECT_ADDR") {
        return val;
    }
    let cfg = load_cli_config();
    if !cfg.project_addr.trim().is_empty() {
        return cfg.project_addr;
    }
    "127.0.0.1:50053".into()
}
fn default_build_addr() -> String {
    if let Ok(val) = std::env::var("AADK_BUILD_ADDR") {
        return val;
    }
    let cfg = load_cli_config();
    if !cfg.build_addr.trim().is_empty() {
        return cfg.build_addr;
    }
    "127.0.0.1:50054".into()
}
fn default_observe_addr() -> String {
    if let Ok(val) = std::env::var("AADK_OBSERVE_ADDR") {
        return val;
    }
    let cfg = load_cli_config();
    if !cfg.observe_addr.trim().is_empty() {
        return cfg.observe_addr;
    }
    "127.0.0.1:50056".into()
}
fn default_workflow_addr() -> String {
    if let Ok(val) = std::env::var("AADK_WORKFLOW_ADDR") {
        return val;
    }
    let cfg = load_cli_config();
    if !cfg.workflow_addr.trim().is_empty() {
        return cfg.workflow_addr;
    }
    "127.0.0.1:50057".into()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Job { cmd } => match cmd {
            JobCmd::Run {
                addr,
                job_type,
                param,
                project_id,
                target_id,
                toolchain_set_id,
                correlation_id,
                run_id,
                no_stream,
            } => {
                if job_type.trim().is_empty() {
                    eprintln!("job_type is required");
                    return Ok(());
                }
                update_cli_config(|cfg| {
                    cfg.job_addr = addr.clone();
                    cfg.last_job_type = job_type.clone();
                });
                let params = parse_kv_params(&param);
                let mut client = JobServiceClient::new(connect(&addr).await?);
                let resp = client.start_job(StartJobRequest {
                    job_type: job_type.clone(),
                    params,
                    project_id: project_id.as_ref().map(|value| Id { value: value.clone() }),
                    target_id: target_id.as_ref().map(|value| Id { value: value.clone() }),
                    toolchain_set_id: toolchain_set_id
                        .as_ref()
                        .map(|value| Id { value: value.clone() }),
                    correlation_id: correlation_id.unwrap_or_default(),
                    run_id: run_id.map(|value| RunId { value }),
                }).await?.into_inner();
                let job_id = resp.job.and_then(|r| r.job_id).map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}");
                if !job_id.is_empty() {
                    update_cli_config(|cfg| cfg.last_job_id = job_id.clone());
                }
                if !job_id.is_empty() && !no_stream {
                    stream_job_events_until_done(&addr, &job_id).await?;
                }
            }
            JobCmd::List {
                addr,
                job_type,
                state,
                created_after,
                created_before,
                finished_after,
                finished_before,
                correlation_id,
                run_id,
                page_size,
                page_token,
            } => {
                update_cli_config(|cfg| cfg.job_addr = addr.clone());
                let (states, unknown_states) = parse_job_states(&state);
                if !unknown_states.is_empty() {
                    eprintln!("Unknown states: {}", unknown_states.join(", "));
                }
                let filter = JobFilter {
                    job_types: split_tokens(&job_type),
                    states,
                    created_after: created_after.map(|ms| aadk_proto::aadk::v1::Timestamp { unix_millis: ms }),
                    created_before: created_before.map(|ms| aadk_proto::aadk::v1::Timestamp { unix_millis: ms }),
                    finished_after: finished_after.map(|ms| aadk_proto::aadk::v1::Timestamp { unix_millis: ms }),
                    finished_before: finished_before.map(|ms| aadk_proto::aadk::v1::Timestamp { unix_millis: ms }),
                    correlation_id: correlation_id.unwrap_or_default(),
                    run_id: run_id.map(|value| RunId { value }),
                };
                let mut client = JobServiceClient::new(connect(&addr).await?);
                let resp = client
                    .list_jobs(ListJobsRequest {
                        page: Some(Pagination { page_size: page_size.max(1), page_token }),
                        filter: Some(filter),
                    })
                    .await?
                    .into_inner();
                if resp.jobs.is_empty() {
                    println!("no jobs found");
                } else {
                    println!("jobs={}", resp.jobs.len());
                    for job in &resp.jobs {
                        render_job_summary(job);
                    }
                }
                if let Some(page_info) = resp.page_info {
                    if !page_info.next_page_token.is_empty() {
                        println!("next_page_token={}", page_info.next_page_token);
                    }
                }
            }
            JobCmd::Watch { addr, job_id, include_history } => {
                update_cli_config(|cfg| {
                    cfg.job_addr = addr.clone();
                    cfg.last_job_id = job_id.clone();
                });
                stream_job_events(&addr, &job_id, include_history, true).await?;
            }
            JobCmd::WatchRun {
                addr,
                run_id,
                correlation_id,
                include_history,
                buffer_max_events,
                max_delay_ms,
                discovery_interval_ms,
            } => {
                update_cli_config(|cfg| cfg.job_addr = addr.clone());
                stream_run_events(
                    &addr,
                    run_id,
                    correlation_id,
                    include_history,
                    buffer_max_events,
                    max_delay_ms,
                    discovery_interval_ms,
                )
                .await?;
            }
            JobCmd::History {
                addr,
                job_id,
                kind,
                after,
                before,
                page_size,
                page_token,
            } => {
                update_cli_config(|cfg| {
                    cfg.job_addr = addr.clone();
                    cfg.last_job_id = job_id.clone();
                });
                let (kinds, unknown_kinds) = parse_job_event_kinds(&kind);
                if !unknown_kinds.is_empty() {
                    eprintln!("Unknown kinds: {}", unknown_kinds.join(", "));
                }
                let filter = JobHistoryFilter {
                    kinds,
                    after: after.map(|ms| aadk_proto::aadk::v1::Timestamp { unix_millis: ms }),
                    before: before.map(|ms| aadk_proto::aadk::v1::Timestamp { unix_millis: ms }),
                };
                let mut client = JobServiceClient::new(connect(&addr).await?);
                let resp = client
                    .list_job_history(ListJobHistoryRequest {
                        job_id: Some(Id { value: job_id.clone() }),
                        page: Some(Pagination { page_size: page_size.max(1), page_token }),
                        filter: Some(filter),
                    })
                    .await?
                    .into_inner();
                if resp.events.is_empty() {
                    println!("no events found");
                } else {
                    println!("events={}", resp.events.len());
                    for event in &resp.events {
                        render_job_event(event);
                    }
                }
                if let Some(page_info) = resp.page_info {
                    if !page_info.next_page_token.is_empty() {
                        println!("next_page_token={}", page_info.next_page_token);
                    }
                }
            }
            JobCmd::Export { addr, job_id, output } => {
                update_cli_config(|cfg| {
                    cfg.job_addr = addr.clone();
                    cfg.last_job_id = job_id.clone();
                });
                let path = output
                    .as_ref()
                    .map(|value| PathBuf::from(value))
                    .unwrap_or_else(|| default_export_path("cli-job-export", &job_id));
                let mut client = JobServiceClient::new(connect(&addr).await?);
                let job = match client
                    .get_job(GetJobRequest {
                        job_id: Some(Id { value: job_id.clone() }),
                    })
                    .await
                {
                    Ok(resp) => resp.into_inner().job,
                    Err(err) => {
                        eprintln!("GetJob failed: {err}");
                        None
                    }
                };
                let events = collect_job_history(&mut client, &job_id).await?;
                let export = CliLogExport {
                    exported_at_unix_millis: now_millis(),
                    job_id: job_id.clone(),
                    config: load_cli_config(),
                    job: job.as_ref().map(JobSummary::from_proto),
                    events: events.iter().map(LogExportEvent::from_proto).collect(),
                };
                write_json_atomic(&path, &export)?;
                println!("exported={}", path.display());
            }
            JobCmd::Cancel { addr, job_id } => {
                update_cli_config(|cfg| {
                    cfg.job_addr = addr.clone();
                    cfg.last_job_id = job_id.clone();
                });
                let mut client = JobServiceClient::new(connect(&addr).await?);
                let resp = client.cancel_job(CancelJobRequest {
                    job_id: Some(Id { value: job_id }),
                }).await?.into_inner();
                println!("accepted={}", resp.accepted);
            }
        },

        Cmd::Toolchain { cmd } => match cmd {
            ToolchainCmd::ListProviders { addr } => {
                update_cli_config(|cfg| cfg.toolchain_addr = addr.clone());
                let mut client = ToolchainServiceClient::new(connect(&addr).await?);
                let resp = client.list_providers(ListProvidersRequest {}).await?.into_inner();
                for p in resp.providers {
                    let id = p.provider_id.map(|i| i.value).unwrap_or_default();
                    println!("{}\t{}\tkind={}", id, p.name, p.kind);
                }
            }
            ToolchainCmd::ListSets { addr, page_size, page_token } => {
                update_cli_config(|cfg| cfg.toolchain_addr = addr.clone());
                let mut client = ToolchainServiceClient::new(connect(&addr).await?);
                let resp = client.list_toolchain_sets(ListToolchainSetsRequest {
                    page: Some(Pagination { page_size, page_token }),
                }).await?.into_inner();
                for set in resp.sets {
                    let set_id = set.toolchain_set_id.map(|i| i.value).unwrap_or_default();
                    let sdk = set.sdk_toolchain_id.map(|i| i.value).unwrap_or_default();
                    let ndk = set.ndk_toolchain_id.map(|i| i.value).unwrap_or_default();
                    println!("{}\t{}\tsdk={}\tndk={}", set_id, set.display_name, sdk, ndk);
                }
                if let Some(page_info) = resp.page_info {
                    if !page_info.next_page_token.is_empty() {
                        println!("next_page_token={}", page_info.next_page_token);
                    }
                }
            }
            ToolchainCmd::CreateSet {
                addr,
                sdk_toolchain_id,
                ndk_toolchain_id,
                display_name,
            } => {
                update_cli_config(|cfg| cfg.toolchain_addr = addr.clone());
                let sdk_id = sdk_toolchain_id.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty());
                let ndk_id = ndk_toolchain_id.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty());
                if sdk_id.is_none() && ndk_id.is_none() {
                    eprintln!("Provide --sdk-toolchain-id and/or --ndk-toolchain-id");
                    return Ok(());
                }
                let mut client = ToolchainServiceClient::new(connect(&addr).await?);
                let resp = client.create_toolchain_set(CreateToolchainSetRequest {
                    sdk_toolchain_id: sdk_id.map(|value| Id { value: value.to_string() }),
                    ndk_toolchain_id: ndk_id.map(|value| Id { value: value.to_string() }),
                    display_name: display_name.unwrap_or_default(),
                }).await?.into_inner();
                if let Some(set) = resp.set {
                    let set_id = set.toolchain_set_id.map(|i| i.value).unwrap_or_default();
                    let sdk = set.sdk_toolchain_id.map(|i| i.value).unwrap_or_default();
                    let ndk = set.ndk_toolchain_id.map(|i| i.value).unwrap_or_default();
                    println!("set_id={set_id}\tdisplay_name={}\tsdk={sdk}\tndk={ndk}", set.display_name);
                } else {
                    println!("no toolchain set returned");
                }
            }
            ToolchainCmd::SetActive { addr, toolchain_set_id } => {
                update_cli_config(|cfg| cfg.toolchain_addr = addr.clone());
                let mut client = ToolchainServiceClient::new(connect(&addr).await?);
                let resp = client.set_active_toolchain_set(SetActiveToolchainSetRequest {
                    toolchain_set_id: Some(Id { value: toolchain_set_id.clone() }),
                }).await?.into_inner();
                println!("ok={}", resp.ok);
            }
            ToolchainCmd::GetActive { addr } => {
                update_cli_config(|cfg| cfg.toolchain_addr = addr.clone());
                let mut client = ToolchainServiceClient::new(connect(&addr).await?);
                let resp = client.get_active_toolchain_set(GetActiveToolchainSetRequest {}).await?.into_inner();
                if let Some(set) = resp.set {
                    let set_id = set.toolchain_set_id.map(|i| i.value).unwrap_or_default();
                    let sdk = set.sdk_toolchain_id.map(|i| i.value).unwrap_or_default();
                    let ndk = set.ndk_toolchain_id.map(|i| i.value).unwrap_or_default();
                    println!("set_id={set_id}\tdisplay_name={}\tsdk={sdk}\tndk={ndk}", set.display_name);
                } else {
                    println!("no active toolchain set");
                }
            }
            ToolchainCmd::Update {
                addr,
                job_addr,
                toolchain_id,
                version,
                verify_hash,
                remove_cached,
                job_id,
                correlation_id,
                run_id,
                no_stream,
            } => {
                update_cli_config(|cfg| {
                    cfg.toolchain_addr = addr.clone();
                    cfg.job_addr = job_addr.clone();
                });
                if toolchain_id.trim().is_empty() || version.trim().is_empty() {
                    eprintln!("toolchain_id and --version are required");
                    return Ok(());
                }
                let mut client = ToolchainServiceClient::new(connect(&addr).await?);
                let resp = client
                    .update_toolchain(UpdateToolchainRequest {
                        toolchain_id: Some(Id { value: toolchain_id.clone() }),
                        version,
                        verify_hash,
                        remove_cached_artifact: remove_cached,
                        job_id: job_id
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| Id { value: value.clone() }),
                        correlation_id: correlation_id.unwrap_or_default(),
                        run_id: run_id.map(|value| RunId { value }),
                    })
                    .await?
                    .into_inner();
                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}");
                if !job_id.is_empty() && !no_stream {
                    stream_job_events_until_done(&job_addr, &job_id).await?;
                }
            }
            ToolchainCmd::Uninstall {
                addr,
                job_addr,
                toolchain_id,
                remove_cached,
                force,
                job_id,
                correlation_id,
                run_id,
                no_stream,
            } => {
                update_cli_config(|cfg| {
                    cfg.toolchain_addr = addr.clone();
                    cfg.job_addr = job_addr.clone();
                });
                if toolchain_id.trim().is_empty() {
                    eprintln!("toolchain_id is required");
                    return Ok(());
                }
                let mut client = ToolchainServiceClient::new(connect(&addr).await?);
                let resp = client
                    .uninstall_toolchain(UninstallToolchainRequest {
                        toolchain_id: Some(Id { value: toolchain_id.clone() }),
                        remove_cached_artifact: remove_cached,
                        force,
                        job_id: job_id
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| Id { value: value.clone() }),
                        correlation_id: correlation_id.unwrap_or_default(),
                        run_id: run_id.map(|value| RunId { value }),
                    })
                    .await?
                    .into_inner();
                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}");
                if !job_id.is_empty() && !no_stream {
                    stream_job_events_until_done(&job_addr, &job_id).await?;
                }
            }
            ToolchainCmd::CleanupCache {
                addr,
                job_addr,
                dry_run,
                remove_all,
                job_id,
                correlation_id,
                run_id,
                no_stream,
            } => {
                update_cli_config(|cfg| {
                    cfg.toolchain_addr = addr.clone();
                    cfg.job_addr = job_addr.clone();
                });
                let mut client = ToolchainServiceClient::new(connect(&addr).await?);
                let resp = client
                    .cleanup_toolchain_cache(CleanupToolchainCacheRequest {
                        dry_run,
                        remove_all,
                        job_id: job_id
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| Id { value: value.clone() }),
                        correlation_id: correlation_id.unwrap_or_default(),
                        run_id: run_id.map(|value| RunId { value }),
                    })
                    .await?
                    .into_inner();
                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}");
                if !job_id.is_empty() && !no_stream {
                    stream_job_events_until_done(&job_addr, &job_id).await?;
                }
            }
        },

        Cmd::Targets { cmd } => match cmd {
            TargetsCmd::List { addr } => {
                update_cli_config(|cfg| cfg.targets_addr = addr.clone());
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client.list_targets(ListTargetsRequest { include_offline: true }).await?.into_inner();
                for t in resp.targets {
                    let id = t.target_id.map(|i| i.value).unwrap_or_default();
                    println!("{}\t{}\t{}\t{}", id, t.display_name, t.state, t.provider);
                }
            }
            TargetsCmd::SetDefault { addr, target_id } => {
                update_cli_config(|cfg| cfg.targets_addr = addr.clone());
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client.set_default_target(SetDefaultTargetRequest {
                    target_id: Some(Id { value: target_id.clone() }),
                }).await?.into_inner();
                println!("ok={}", resp.ok);
            }
            TargetsCmd::GetDefault { addr } => {
                update_cli_config(|cfg| cfg.targets_addr = addr.clone());
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client.get_default_target(GetDefaultTargetRequest {}).await?.into_inner();
                if let Some(target) = resp.target {
                    let id = target.target_id.map(|i| i.value).unwrap_or_default();
                    println!("{}\t{}\t{}\t{}", id, target.display_name, target.state, target.provider);
                } else {
                    println!("no default target configured");
                }
            }
            TargetsCmd::StartCuttlefish {
                addr,
                show_full_ui,
                job_id,
                correlation_id,
                run_id,
            } => {
                update_cli_config(|cfg| cfg.targets_addr = addr.clone());
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client
                    .start_cuttlefish(StartCuttlefishRequest {
                        show_full_ui,
                        job_id: job_id
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| Id { value: value.clone() }),
                        correlation_id: correlation_id.unwrap_or_default(),
                        run_id: run_id.map(|value| RunId { value }),
                    })
                    .await?
                    .into_inner();
                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}");
            }
            TargetsCmd::StopCuttlefish {
                addr,
                job_id,
                correlation_id,
                run_id,
            } => {
                update_cli_config(|cfg| cfg.targets_addr = addr.clone());
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client
                    .stop_cuttlefish(StopCuttlefishRequest {
                        job_id: job_id
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| Id { value: value.clone() }),
                        correlation_id: correlation_id.unwrap_or_default(),
                        run_id: run_id.map(|value| RunId { value }),
                    })
                    .await?
                    .into_inner();
                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}");
            }
            TargetsCmd::CuttlefishStatus { addr } => {
                update_cli_config(|cfg| cfg.targets_addr = addr.clone());
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client.get_cuttlefish_status(GetCuttlefishStatusRequest {}).await?.into_inner();
                println!("state={}\tadb={}", resp.state, resp.adb_serial);
                for kv in resp.details {
                    println!("{}\t{}", kv.key, kv.value);
                }
            }
            TargetsCmd::InstallCuttlefish {
                addr,
                force,
                job_id,
                correlation_id,
                run_id,
            } => {
                update_cli_config(|cfg| cfg.targets_addr = addr.clone());
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client
                    .install_cuttlefish(InstallCuttlefishRequest {
                        force,
                        branch: "".into(),
                        target: "".into(),
                        build_id: "".into(),
                        job_id: job_id
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| Id { value: value.clone() }),
                        correlation_id: correlation_id.unwrap_or_default(),
                        run_id: run_id.map(|value| RunId { value }),
                    })
                    .await?
                    .into_inner();
                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}");
            }
        },

        Cmd::Project { cmd } => match cmd {
            ProjectCmd::ListTemplates { addr } => {
                update_cli_config(|cfg| cfg.project_addr = addr.clone());
                let mut client = ProjectServiceClient::new(connect(&addr).await?);
                let resp = client.list_templates(ListTemplatesRequest {}).await?.into_inner();
                for t in resp.templates {
                    let id = t.template_id.map(|i| i.value).unwrap_or_default();
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
                    println!("{}\t{}\n{}\n{}\n", id, t.name, t.description, defaults);
                }
            }
            ProjectCmd::ListRecent { addr, page_size, page_token } => {
                update_cli_config(|cfg| cfg.project_addr = addr.clone());
                let mut client = ProjectServiceClient::new(connect(&addr).await?);
                let resp = client.list_recent_projects(ListRecentProjectsRequest {
                    page: Some(Pagination {
                        page_size,
                        page_token,
                    }),
                }).await?.into_inner();
                for project in resp.projects {
                    let id = project.project_id.map(|i| i.value).unwrap_or_default();
                    println!("{}\t{}\t{}", id, project.name, project.path);
                }
                if let Some(page_info) = resp.page_info {
                    if !page_info.next_page_token.is_empty() {
                        println!("next_page_token={}", page_info.next_page_token);
                    }
                }
            }
            ProjectCmd::Create {
                addr,
                job_addr,
                name,
                path,
                template_id,
                job_id,
                correlation_id,
                run_id,
            } => {
                update_cli_config(|cfg| {
                    cfg.project_addr = addr.clone();
                    cfg.job_addr = job_addr.clone();
                });
                let mut client = ProjectServiceClient::new(connect(&addr).await?);
                let resp = client.create_project(CreateProjectRequest {
                    name,
                    path,
                    template_id: Some(Id { value: template_id }),
                    params: vec![],
                    toolchain_set_id: None,
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id { value: value.clone() }),
                    correlation_id: correlation_id.unwrap_or_default(),
                    run_id: run_id.map(|value| RunId { value }),
                }).await?.into_inner();

                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                let project_id = resp.project_id.map(|i| i.value).unwrap_or_default();
                println!("project_id={project_id}\njob_id={job_id}");

                if !job_id.is_empty() {
                    stream_job_events_until_done(&job_addr, &job_id).await?;
                }
            }
            ProjectCmd::Open { addr, path } => {
                update_cli_config(|cfg| cfg.project_addr = addr.clone());
                let mut client = ProjectServiceClient::new(connect(&addr).await?);
                let resp = client.open_project(OpenProjectRequest { path }).await?.into_inner();
                if let Some(project) = resp.project {
                    let id = project.project_id.map(|i| i.value).unwrap_or_default();
                    println!("{}\t{}\t{}", id, project.name, project.path);
                } else {
                    println!("no project returned");
                }
            }
            ProjectCmd::SetConfig {
                addr,
                project_id,
                toolchain_set_id,
                default_target_id,
            } => {
                update_cli_config(|cfg| cfg.project_addr = addr.clone());
                if project_id.trim().is_empty() {
                    eprintln!("project_id is required");
                    return Ok(());
                }
                let toolchain_set_id = toolchain_set_id
                    .as_ref()
                    .map(|value| value.trim())
                    .filter(|value| !value.is_empty());
                let default_target_id = default_target_id
                    .as_ref()
                    .map(|value| value.trim())
                    .filter(|value| !value.is_empty());

                if toolchain_set_id.is_none() && default_target_id.is_none() {
                    eprintln!("Provide --toolchain-set-id and/or --default-target-id");
                    return Ok(());
                }

                let mut client = ProjectServiceClient::new(connect(&addr).await?);
                let resp = client.set_project_config(SetProjectConfigRequest {
                    project_id: Some(Id { value: project_id.clone() }),
                    toolchain_set_id: toolchain_set_id.map(|value| Id { value: value.to_string() }),
                    default_target_id: default_target_id.map(|value| Id { value: value.to_string() }),
                }).await?.into_inner();
                println!("ok={}", resp.ok);
            }
            ProjectCmd::UseActiveDefaults {
                addr,
                toolchain_addr,
                targets_addr,
                project_id,
            } => {
                update_cli_config(|cfg| {
                    cfg.project_addr = addr.clone();
                    cfg.toolchain_addr = toolchain_addr.clone();
                    cfg.targets_addr = targets_addr.clone();
                });
                if project_id.trim().is_empty() {
                    eprintln!("project_id is required");
                    return Ok(());
                }

                let mut toolchain_set_id = None;
                match connect(&toolchain_addr).await {
                    Ok(channel) => {
                        let mut client = ToolchainServiceClient::new(channel);
                        match client.get_active_toolchain_set(GetActiveToolchainSetRequest {}).await {
                            Ok(resp) => {
                                toolchain_set_id = resp
                                    .into_inner()
                                    .set
                                    .and_then(|set| set.toolchain_set_id)
                                    .map(|id| id.value)
                                    .filter(|value| !value.trim().is_empty());
                            }
                            Err(err) => {
                                eprintln!("active toolchain set lookup failed: {err}");
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!("toolchain service connection failed: {err}");
                    }
                }

                let mut default_target_id = None;
                match connect(&targets_addr).await {
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
                                eprintln!("default target lookup failed: {err}");
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!("targets service connection failed: {err}");
                    }
                }

                if toolchain_set_id.is_none() && default_target_id.is_none() {
                    eprintln!("no active toolchain set or default target found");
                    return Ok(());
                }

                let mut client = ProjectServiceClient::new(connect(&addr).await?);
                let resp = client
                    .set_project_config(SetProjectConfigRequest {
                        project_id: Some(Id { value: project_id.clone() }),
                        toolchain_set_id: toolchain_set_id.clone().map(|value| Id { value }),
                        default_target_id: default_target_id.clone().map(|value| Id { value }),
                    })
                    .await?
                    .into_inner();

                if resp.ok {
                    println!(
                        "ok=true\ntoolchain_set_id={}\ndefault_target_id={}",
                        toolchain_set_id.unwrap_or_default(),
                        default_target_id.unwrap_or_default()
                    );
                } else {
                    println!("ok=false");
                }
            }
        },

        Cmd::Observe { cmd } => match cmd {
            ObserveCmd::ListRuns {
                addr,
                page_size,
                page_token,
                run_id,
                correlation_id,
                project_id,
                target_id,
                toolchain_set_id,
                result,
            } => {
                update_cli_config(|cfg| cfg.observe_addr = addr.clone());
                let mut client = ObserveServiceClient::new(connect(&addr).await?);
                let resp = client.list_runs(ListRunsRequest {
                    page: Some(Pagination {
                        page_size,
                        page_token,
                    }),
                    filter: Some(RunFilter {
                        run_id: run_id.map(|value| RunId { value }),
                        correlation_id: correlation_id.unwrap_or_default(),
                        project_id: project_id.map(|value| Id { value }),
                        target_id: target_id.map(|value| Id { value }),
                        toolchain_set_id: toolchain_set_id.map(|value| Id { value }),
                        result: result.unwrap_or_default(),
                    }),
                }).await?.into_inner();
                for run in resp.runs {
                    let run_id = run.run_id.map(|i| i.value).unwrap_or_default();
                    let started = run.started_at.map(|t| t.unix_millis).unwrap_or(0);
                    let finished = run.finished_at.map(|t| t.unix_millis).unwrap_or(0);
                    let output_summary = run.output_summary.as_ref();
                    let bundle_count = output_summary.map(|s| s.bundle_count).unwrap_or(0);
                    let artifact_count = output_summary.map(|s| s.artifact_count).unwrap_or(0);
                    let output_updated = output_summary
                        .and_then(|s| s.updated_at.as_ref())
                        .map(|t| t.unix_millis)
                        .unwrap_or(0);
                    let last_bundle_id = output_summary
                        .map(|s| s.last_bundle_id.as_str())
                        .unwrap_or("");
                    println!(
                        "{}\tresult={}\tstarted={}\tfinished={}\tjobs={}\tbundles={}\tartifacts={}\toutputs_updated={}\tlast_bundle_id={}",
                        run_id,
                        run.result,
                        started,
                        finished,
                        run.job_ids.len(),
                        bundle_count,
                        artifact_count,
                        output_updated,
                        last_bundle_id
                    );
                }
                if let Some(page_info) = resp.page_info {
                    if !page_info.next_page_token.is_empty() {
                        println!("next_page_token={}", page_info.next_page_token);
                    }
                }
            }
            ObserveCmd::ListOutputs {
                addr,
                run_id,
                kind,
                output_type,
                path_contains,
                label_contains,
                page_size,
                page_token,
            } => {
                update_cli_config(|cfg| cfg.observe_addr = addr.clone());
                let kind_value = kind
                    .as_deref()
                    .and_then(parse_run_output_kind)
                    .unwrap_or(RunOutputKind::RunOutputKindUnspecified);
                let mut client = ObserveServiceClient::new(connect(&addr).await?);
                let resp = client
                    .list_run_outputs(ListRunOutputsRequest {
                        run_id: Some(RunId { value: run_id }),
                        page: Some(Pagination {
                            page_size,
                            page_token,
                        }),
                        filter: Some(RunOutputFilter {
                            kind: kind_value as i32,
                            output_type: output_type.unwrap_or_default(),
                            path_contains: path_contains.unwrap_or_default(),
                            label_contains: label_contains.unwrap_or_default(),
                        }),
                    })
                    .await?
                    .into_inner();
                if let Some(summary) = resp.summary.as_ref() {
                    let updated = summary
                        .updated_at
                        .as_ref()
                        .map(|t| t.unix_millis)
                        .unwrap_or(0);
                    println!(
                        "summary: bundles={} artifacts={} updated_at={} last_bundle_id={}",
                        summary.bundle_count,
                        summary.artifact_count,
                        updated,
                        summary.last_bundle_id
                    );
                }
                for output in resp.outputs {
                    let job_id = output.job_id.map(|id| id.value).unwrap_or_default();
                    let created_at = output
                        .created_at
                        .map(|ts| ts.unix_millis)
                        .unwrap_or(0);
                    println!(
                        "{}\tkind={}\ttype={}\tpath={}\tlabel={}\tjob_id={}\tcreated_at={}",
                        output.output_id,
                        run_output_kind_label(output.kind),
                        output.output_type,
                        output.path,
                        output.label,
                        job_id,
                        created_at
                    );
                    if !output.metadata.is_empty() {
                        let metadata = output
                            .metadata
                            .iter()
                            .map(|item| format!("{}={}", item.key, item.value))
                            .collect::<Vec<_>>()
                            .join(", ");
                        println!("  metadata: {metadata}");
                    }
                }
                if let Some(page_info) = resp.page_info {
                    if !page_info.next_page_token.is_empty() {
                        println!("next_page_token={}", page_info.next_page_token);
                    }
                }
            }
            ObserveCmd::ExportSupport {
                addr,
                job_addr,
                include_logs,
                include_config,
                include_toolchain_provenance,
                include_recent_runs,
                recent_runs_limit,
                job_id,
                correlation_id,
                run_id,
                project_id,
                target_id,
                toolchain_set_id,
            } => {
                update_cli_config(|cfg| {
                    cfg.observe_addr = addr.clone();
                    cfg.job_addr = job_addr.clone();
                });
                let mut client = ObserveServiceClient::new(connect(&addr).await?);
                let resp = client.export_support_bundle(ExportSupportBundleRequest {
                    include_logs,
                    include_config,
                    include_toolchain_provenance,
                    include_recent_runs,
                    recent_runs_limit,
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id { value: value.clone() }),
                    project_id: project_id.map(|value| Id { value }),
                    target_id: target_id.map(|value| Id { value }),
                    toolchain_set_id: toolchain_set_id.map(|value| Id { value }),
                    correlation_id: correlation_id.unwrap_or_default(),
                    run_id: run_id.map(|value| RunId { value }),
                }).await?.into_inner();
                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}\noutput_path={}", resp.output_path);
                if !job_id.is_empty() {
                    stream_job_events_until_done(&job_addr, &job_id).await?;
                }
            }
            ObserveCmd::ExportEvidence {
                addr,
                job_addr,
                run_id,
                job_id,
                correlation_id,
            } => {
                update_cli_config(|cfg| {
                    cfg.observe_addr = addr.clone();
                    cfg.job_addr = job_addr.clone();
                });
                let mut client = ObserveServiceClient::new(connect(&addr).await?);
                let resp = client.export_evidence_bundle(ExportEvidenceBundleRequest {
                    run_id: run_id.map(|value| RunId { value }),
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id { value: value.clone() }),
                    correlation_id: correlation_id.unwrap_or_default(),
                }).await?.into_inner();
                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}\noutput_path={}", resp.output_path);
                if !job_id.is_empty() {
                    stream_job_events_until_done(&job_addr, &job_id).await?;
                }
            }
        },

        Cmd::Build { cmd } => match cmd {
            BuildCmd::Run {
                addr,
                job_addr,
                project_ref,
                variant,
                variant_name,
                module,
                task,
                gradle_arg,
                clean_first,
                job_id,
                correlation_id,
                run_id,
                no_stream,
            } => {
                update_cli_config(|cfg| {
                    cfg.build_addr = addr.clone();
                    cfg.job_addr = job_addr.clone();
                });
                if project_ref.trim().is_empty() {
                    eprintln!("project_ref is required");
                    return Ok(());
                }
                let Some(variant) = parse_build_variant(&variant) else {
                    eprintln!("unsupported variant: {variant}");
                    return Ok(());
                };

                let tasks = task
                    .into_iter()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>();
                let gradle_args = gradle_arg
                    .into_iter()
                    .map(|arg| arg.trim().to_string())
                    .filter(|arg| !arg.is_empty())
                    .map(|arg| KeyValue {
                        key: arg,
                        value: String::new(),
                    })
                    .collect::<Vec<_>>();

                let mut client = BuildServiceClient::new(connect(&addr).await?);
                let resp = client.build(BuildRequest {
                    project_id: Some(Id { value: project_ref.trim().to_string() }),
                    variant: variant as i32,
                    clean_first,
                    gradle_args,
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id { value: value.clone() }),
                    module: module.unwrap_or_default().trim().to_string(),
                    variant_name: variant_name.unwrap_or_default().trim().to_string(),
                    tasks,
                    correlation_id: correlation_id.unwrap_or_default(),
                    run_id: run_id.map(|value| RunId { value }),
                }).await?.into_inner();

                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}");
                if !job_id.is_empty() && !no_stream {
                    stream_job_events_until_done(&job_addr, &job_id).await?;
                }
            }
            BuildCmd::ListArtifacts {
                addr,
                project_ref,
                variant,
                module,
                variant_name,
                artifact_type,
                name_contains,
                path_contains,
            } => {
                update_cli_config(|cfg| cfg.build_addr = addr.clone());
                if project_ref.trim().is_empty() {
                    eprintln!("project_ref is required");
                    return Ok(());
                }
                let Some(variant) = parse_build_variant(&variant) else {
                    eprintln!("unsupported variant: {variant}");
                    return Ok(());
                };

                let modules = module
                    .into_iter()
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty())
                    .collect::<Vec<_>>();

                let (types, unknown) = parse_artifact_types(&artifact_type);
                if !unknown.is_empty() {
                    eprintln!("unknown artifact types: {}", unknown.join(", "));
                }

                let filter = ArtifactFilter {
                    modules,
                    variant: variant_name.unwrap_or_default().trim().to_string(),
                    types: types.into_iter().map(|t| t as i32).collect(),
                    name_contains: name_contains.trim().to_string(),
                    path_contains: path_contains.trim().to_string(),
                };

                let mut client = BuildServiceClient::new(connect(&addr).await?);
                let resp = client.list_artifacts(ListArtifactsRequest {
                    project_id: Some(Id { value: project_ref.trim().to_string() }),
                    variant: variant as i32,
                    filter: Some(filter),
                }).await?.into_inner();

                if resp.artifacts.is_empty() {
                    println!("no artifacts found");
                } else {
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
                    println!("artifacts={total}");
                    for (module, artifacts) in by_module {
                        println!("module={module} count={}", artifacts.len());
                        for artifact in artifacts {
                            render_artifact(&artifact);
                        }
                    }
                }
            }
        },

        Cmd::Workflow { cmd } => match cmd {
            WorkflowCmd::RunPipeline {
                addr,
                job_addr,
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
                variant,
                module,
                variant_name,
                task,
                apk_path,
                application_id,
                activity,
                step,
                stream_run,
                no_stream,
            } => {
                update_cli_config(|cfg| {
                    cfg.workflow_addr = addr.clone();
                    cfg.job_addr = job_addr.clone();
                    cfg.last_job_type = "workflow.pipeline".into();
                });

                let (options, unknown_steps) = parse_workflow_steps(&step);
                if !unknown_steps.is_empty() {
                    eprintln!("Unknown workflow steps: {}", unknown_steps.join(", "));
                }
                if let Some(opts) = options.as_ref() {
                    let any = opts.verify_toolchain
                        || opts.create_project
                        || opts.open_project
                        || opts.build
                        || opts.install_apk
                        || opts.launch_app
                        || opts.export_support_bundle
                        || opts.export_evidence_bundle;
                    if !any {
                        eprintln!("No workflow steps enabled; use --step all or omit --step for inference.");
                        return Ok(());
                    }
                }

                let Some(build_variant) = parse_build_variant(&variant) else {
                    eprintln!("unsupported variant: {variant}");
                    return Ok(());
                };

                let tasks = task
                    .into_iter()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>();

                let mut client = WorkflowServiceClient::new(connect(&addr).await?);
                let correlation_id = correlation_id.unwrap_or_default();
                let resp = client
                    .run_pipeline(WorkflowPipelineRequest {
                        run_id: run_id.map(|value| RunId { value }),
                        correlation_id: correlation_id.clone(),
                        job_id: job_id
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| Id { value: value.clone() }),
                        project_id: project_id
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| Id { value: value.clone() }),
                        project_path: project_path.unwrap_or_default().trim().to_string(),
                        project_name: project_name.unwrap_or_default().trim().to_string(),
                        template_id: template_id
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| Id { value: value.clone() }),
                        toolchain_id: toolchain_id
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| Id { value: value.clone() }),
                        toolchain_set_id: toolchain_set_id
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| Id { value: value.clone() }),
                        target_id: target_id
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| Id { value: value.clone() }),
                        build_variant: build_variant as i32,
                        module: module.unwrap_or_default().trim().to_string(),
                        variant_name: variant_name.unwrap_or_default().trim().to_string(),
                        tasks,
                        apk_path: apk_path.unwrap_or_default().trim().to_string(),
                        application_id: application_id.unwrap_or_default().trim().to_string(),
                        activity: activity.unwrap_or_default().trim().to_string(),
                        options,
                    })
                    .await?
                    .into_inner();

                let run_id = resp.run_id.as_ref().map(|id| id.value.clone()).unwrap_or_default();
                let job_id = resp.job_id.as_ref().map(|id| id.value.clone()).unwrap_or_default();
                let project_id = resp
                    .project_id
                    .as_ref()
                    .map(|id| id.value.clone())
                    .unwrap_or_default();
                println!("run_id={run_id}\njob_id={job_id}\nproject_id={project_id}");

                if !job_id.is_empty() {
                    update_cli_config(|cfg| cfg.last_job_id = job_id.clone());
                }

                if no_stream || job_id.is_empty() {
                    return Ok(());
                }

                if stream_run {
                    let run_id_opt = if !run_id.trim().is_empty() {
                        Some(run_id)
                    } else {
                        None
                    };
                    let correlation_opt = if !correlation_id.trim().is_empty() {
                        Some(correlation_id)
                    } else {
                        None
                    };
                    stream_run_events(
                        &job_addr,
                        run_id_opt,
                        correlation_opt,
                        true,
                        None,
                        None,
                        None,
                    )
                    .await?;
                } else {
                    stream_job_events(&job_addr, &job_id, true, true).await?;
                }
            }
        },
    }

    Ok(())
}

async fn stream_job_events_until_done(
    addr: &str,
    job_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    stream_job_events(addr, job_id, true, true).await
}

async fn stream_job_events(
    addr: &str,
    job_id: &str,
    include_history: bool,
    stop_on_done: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = JobServiceClient::new(connect(addr).await?);
    let mut stream = client
        .stream_job_events(StreamJobEventsRequest {
            job_id: Some(Id {
                value: job_id.to_string(),
            }),
            include_history,
        })
        .await?
        .into_inner();

    while let Some(item) = stream.next().await {
        match item {
            Ok(evt) => {
                if render_job_event(&evt) && stop_on_done {
                    break;
                }
            }
            Err(err) => {
                eprintln!("stream error: {err}");
                break;
            }
        }
    }

    Ok(())
}

async fn stream_run_events(
    addr: &str,
    run_id: Option<String>,
    correlation_id: Option<String>,
    include_history: bool,
    buffer_max_events: Option<u32>,
    max_delay_ms: Option<u64>,
    discovery_interval_ms: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = JobServiceClient::new(connect(addr).await?);
    let mut stream = client
        .stream_run_events(StreamRunEventsRequest {
            run_id: run_id.map(|value| RunId { value }),
            correlation_id: correlation_id.unwrap_or_default(),
            include_history,
            buffer_max_events: buffer_max_events.unwrap_or(0),
            max_delay_ms: max_delay_ms.unwrap_or(0),
            discovery_interval_ms: discovery_interval_ms.unwrap_or(0),
        })
        .await?
        .into_inner();

    while let Some(item) = stream.next().await {
        match item {
            Ok(evt) => {
                render_job_event(&evt);
            }
            Err(err) => {
                eprintln!("stream error: {err}");
                break;
            }
        }
    }

    Ok(())
}

fn render_job_event(event: &JobEvent) -> bool {
    let ts = event
        .at
        .as_ref()
        .map(|ts| ts.unix_millis)
        .unwrap_or_default();
    match event.payload.as_ref() {
        Some(JobPayload::StateChanged(state)) => {
            let new_state = JobState::try_from(state.new_state).unwrap_or(JobState::Unspecified);
            println!("{ts} state={new_state:?}");
            matches!(
                new_state,
                JobState::Success | JobState::Failed | JobState::Cancelled
            )
        }
        Some(JobPayload::Progress(progress)) => {
            if let Some(p) = progress.progress.as_ref() {
                println!("{ts} progress {}% {}", p.percent, p.phase);
                for kv in &p.metrics {
                    println!("  {}={}", kv.key, kv.value);
                }
            }
            false
        }
        Some(JobPayload::Log(log)) => {
            if let Some(chunk) = log.chunk.as_ref() {
                print!("{}", String::from_utf8_lossy(&chunk.data));
                let _ = std::io::stdout().flush();
            }
            false
        }
        Some(JobPayload::Completed(completed)) => {
            println!("{ts} completed: {}", completed.summary);
            for kv in &completed.outputs {
                println!("  {}={}", kv.key, kv.value);
            }
            true
        }
        Some(JobPayload::Failed(failed)) => {
            if let Some(err) = failed.error.as_ref() {
                println!("{ts} failed: {} ({})", err.message, err.code);
                if !err.technical_details.is_empty() {
                    println!("details: {}", err.technical_details);
                }
            } else {
                println!("{ts} failed");
            }
            true
        }
        None => {
            println!("{ts} event=unknown");
            false
        }
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn default_export_path(prefix: &str, job_id: &str) -> PathBuf {
    let ts = now_millis();
    data_dir()
        .join("state")
        .join(format!("{prefix}-{job_id}-{ts}.json"))
}

async fn collect_job_history(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
) -> Result<Vec<JobEvent>, Box<dyn std::error::Error>> {
    let mut events = Vec::new();
    let mut page_token = String::new();
    loop {
        let resp = client
            .list_job_history(ListJobHistoryRequest {
                job_id: Some(Id {
                    value: job_id.to_string(),
                }),
                page: Some(Pagination {
                    page_size: 200,
                    page_token: page_token.clone(),
                }),
                filter: None,
            })
            .await?
            .into_inner();
        events.extend(resp.events);
        let next_token = resp
            .page_info
            .map(|page_info| page_info.next_page_token)
            .unwrap_or_default();
        if next_token.is_empty() {
            break;
        }
        page_token = next_token;
    }
    Ok(events)
}

fn split_tokens(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| value.split(|c: char| c == ',' || c.is_whitespace()))
        .map(|token| token.trim())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_string())
        .collect()
}

fn parse_kv_params(params: &[String]) -> Vec<KeyValue> {
    params
        .iter()
        .filter_map(|entry| {
            let trimmed = entry.trim();
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

fn parse_job_states(values: &[String]) -> (Vec<i32>, Vec<String>) {
    let mut states = Vec::new();
    let mut unknown = Vec::new();
    for token in split_tokens(values) {
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

fn parse_job_event_kinds(values: &[String]) -> (Vec<i32>, Vec<String>) {
    let mut kinds = Vec::new();
    let mut unknown = Vec::new();
    for token in split_tokens(values) {
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

fn parse_workflow_steps(
    values: &[String],
) -> (Option<WorkflowPipelineOptions>, Vec<String>) {
    if values.is_empty() {
        return (None, Vec::new());
    }
    let mut opts = WorkflowPipelineOptions::default();
    let mut unknown = Vec::new();
    for token in split_tokens(values) {
        match token.to_ascii_lowercase().as_str() {
            "all" => {
                opts.verify_toolchain = true;
                opts.create_project = true;
                opts.open_project = true;
                opts.build = true;
                opts.install_apk = true;
                opts.launch_app = true;
                opts.export_support_bundle = true;
                opts.export_evidence_bundle = true;
            }
            "verify" | "verify_toolchain" | "toolchain.verify" => opts.verify_toolchain = true,
            "create" | "create_project" | "project.create" => opts.create_project = true,
            "open" | "open_project" | "project.open" => opts.open_project = true,
            "build" | "build.run" => opts.build = true,
            "install" | "install_apk" | "targets.install" => opts.install_apk = true,
            "launch" | "launch_app" | "targets.launch" => opts.launch_app = true,
            "support" | "support_bundle" | "observe.support_bundle" => {
                opts.export_support_bundle = true
            }
            "evidence" | "evidence_bundle" | "observe.evidence_bundle" => {
                opts.export_evidence_bundle = true
            }
            _ => unknown.push(token),
        }
    }
    (Some(opts), unknown)
}

fn render_job_summary(job: &Job) {
    let job_id = job.job_id.as_ref().map(|id| id.value.as_str()).unwrap_or("-");
    let run_id = job.run_id.as_ref().map(|id| id.value.as_str()).unwrap_or("-");
    let state = JobState::try_from(job.state).unwrap_or(JobState::Unspecified);
    let created = job.created_at.as_ref().map(|ts| ts.unix_millis).unwrap_or_default();
    let finished = job.finished_at.as_ref().map(|ts| ts.unix_millis).unwrap_or_default();
    println!(
        "{}\t{}\tstate={state:?}\trun_id={run_id}\tcreated={created}\tfinished={finished}",
        job_id, job.job_type
    );
}

fn kv_pairs(items: &[KeyValue]) -> String {
    items
        .iter()
        .map(|kv| format!("{}={}", kv.key, kv.value))
        .collect::<Vec<_>>()
        .join(", ")
}

impl JobSummary {
    fn from_proto(job: &Job) -> Self {
        let state = JobState::try_from(job.state).unwrap_or(JobState::Unspecified);
        Self {
            job_id: job.job_id.as_ref().map(|id| id.value.clone()).unwrap_or_default(),
            job_type: job.job_type.clone(),
            state: format!("{state:?}"),
            created_at_unix_millis: job.created_at.as_ref().map(|ts| ts.unix_millis).unwrap_or_default(),
            started_at_unix_millis: job.started_at.as_ref().map(|ts| ts.unix_millis),
            finished_at_unix_millis: job.finished_at.as_ref().map(|ts| ts.unix_millis),
            display_name: job.display_name.clone(),
            correlation_id: job.correlation_id.clone(),
            run_id: job.run_id.as_ref().map(|id| id.value.clone()).unwrap_or_default(),
            project_id: job.project_id.as_ref().map(|id| id.value.clone()).unwrap_or_default(),
            target_id: job.target_id.as_ref().map(|id| id.value.clone()).unwrap_or_default(),
            toolchain_set_id: job.toolchain_set_id.as_ref().map(|id| id.value.clone()).unwrap_or_default(),
        }
    }
}

impl LogExportEvent {
    fn from_proto(event: &JobEvent) -> Self {
        let at_unix_millis = event.at.as_ref().map(|ts| ts.unix_millis).unwrap_or_default();
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

fn parse_build_variant(value: &str) -> Option<BuildVariant> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "unspecified" => Some(BuildVariant::Unspecified),
        "debug" => Some(BuildVariant::Debug),
        "release" => Some(BuildVariant::Release),
        _ => None,
    }
}

fn parse_run_output_kind(value: &str) -> Option<RunOutputKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "unspecified" => Some(RunOutputKind::RunOutputKindUnspecified),
        "bundle" | "bundles" => Some(RunOutputKind::RunOutputKindBundle),
        "artifact" | "artifacts" => Some(RunOutputKind::RunOutputKindArtifact),
        _ => None,
    }
}

fn run_output_kind_label(kind: i32) -> &'static str {
    match RunOutputKind::try_from(kind).unwrap_or(RunOutputKind::RunOutputKindUnspecified) {
        RunOutputKind::RunOutputKindBundle => "bundle",
        RunOutputKind::RunOutputKindArtifact => "artifact",
        RunOutputKind::RunOutputKindUnspecified => "unspecified",
    }
}

fn parse_artifact_type(value: &str) -> Option<ArtifactType> {
    match value.trim().to_ascii_lowercase().as_str() {
        "apk" => Some(ArtifactType::Apk),
        "aab" | "bundle" => Some(ArtifactType::Aab),
        "aar" => Some(ArtifactType::Aar),
        "mapping" | "mapping.txt" => Some(ArtifactType::Mapping),
        "test" | "tests" | "test_result" | "test-results" => Some(ArtifactType::TestResult),
        _ => None,
    }
}

fn parse_artifact_types(values: &[String]) -> (Vec<ArtifactType>, Vec<String>) {
    let mut types = Vec::new();
    let mut unknown = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(artifact_type) = parse_artifact_type(trimmed) {
            types.push(artifact_type);
        } else {
            unknown.push(trimmed.to_string());
        }
    }
    (types, unknown)
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

fn metadata_value<'a>(items: &'a [KeyValue], key: &str) -> Option<&'a str> {
    items
        .iter()
        .find(|item| item.key == key)
        .map(|item| item.value.as_str())
}

fn render_artifact(artifact: &Artifact) {
    let kind = ArtifactType::try_from(artifact.r#type).unwrap_or(ArtifactType::Unspecified);
    println!(
        "- [{}] {} size={} path={}",
        artifact_type_label(kind),
        artifact.name,
        artifact.size_bytes,
        artifact.path
    );

    let module = metadata_value(&artifact.metadata, "module").unwrap_or("");
    let variant = metadata_value(&artifact.metadata, "variant").unwrap_or("");
    let task = metadata_value(&artifact.metadata, "task").unwrap_or("");
    if !module.is_empty() || !variant.is_empty() || !task.is_empty() {
        let mut parts = Vec::new();
        if !module.is_empty() {
            parts.push(format!("module={module}"));
        }
        if !variant.is_empty() {
            parts.push(format!("variant={variant}"));
        }
        if !task.is_empty() {
            parts.push(format!("task={task}"));
        }
        println!("  {}", parts.join(" "));
    }
    if !artifact.sha256.is_empty() {
        println!("  sha256={}", artifact.sha256);
    }
}

async fn connect(addr: &str) -> Result<Channel, Box<dyn std::error::Error>> {
    let endpoint = format!("http://{addr}");
    Ok(Channel::from_shared(endpoint)?.connect().await?)
}
