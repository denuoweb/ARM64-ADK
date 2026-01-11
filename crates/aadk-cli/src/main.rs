use std::io::Write;

use aadk_proto::aadk::v1::{
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    observe_service_client::ObserveServiceClient,
    project_service_client::ProjectServiceClient,
    toolchain_service_client::ToolchainServiceClient,
    target_service_client::TargetServiceClient,
    CancelJobRequest, CreateProjectRequest, CreateToolchainSetRequest, ExportEvidenceBundleRequest,
    ExportSupportBundleRequest, GetActiveToolchainSetRequest, GetCuttlefishStatusRequest,
    GetDefaultTargetRequest, Id, InstallCuttlefishRequest, JobState, ListProvidersRequest,
    ListRecentProjectsRequest, ListRunsRequest, ListTargetsRequest, ListTemplatesRequest,
    ListToolchainSetsRequest, OpenProjectRequest, Pagination, SetActiveToolchainSetRequest,
    SetDefaultTargetRequest, SetProjectConfigRequest, StartCuttlefishRequest, StartJobRequest,
    StopCuttlefishRequest, StreamJobEventsRequest,
};
use clap::{Parser, Subcommand};
use futures_util::StreamExt;
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
}

#[derive(Subcommand)]
enum JobCmd {
    /// Start a demo job and stream events
    StartDemo {
        #[arg(long, default_value_t = default_job_addr())]
        addr: String,
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
    },
    /// Stop Cuttlefish and return a job id
    StopCuttlefish {
        #[arg(long, default_value_t = default_targets_addr())]
        addr: String,
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
    },
    /// Export an evidence bundle for a run id
    ExportEvidence {
        #[arg(long, default_value_t = default_observe_addr())]
        addr: String,
        #[arg(long, default_value_t = default_job_addr())]
        job_addr: String,
        run_id: String,
    },
}

fn default_job_addr() -> String {
    std::env::var("AADK_JOB_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".into())
}
fn default_toolchain_addr() -> String {
    std::env::var("AADK_TOOLCHAIN_ADDR").unwrap_or_else(|_| "127.0.0.1:50052".into())
}
fn default_targets_addr() -> String {
    std::env::var("AADK_TARGETS_ADDR").unwrap_or_else(|_| "127.0.0.1:50055".into())
}
fn default_project_addr() -> String {
    std::env::var("AADK_PROJECT_ADDR").unwrap_or_else(|_| "127.0.0.1:50053".into())
}
fn default_observe_addr() -> String {
    std::env::var("AADK_OBSERVE_ADDR").unwrap_or_else(|_| "127.0.0.1:50056".into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Job { cmd } => match cmd {
            JobCmd::StartDemo { addr } => {
                let mut client = JobServiceClient::new(connect(&addr).await?);
                let resp = client.start_job(StartJobRequest {
                    job_type: "demo.job".into(),
                    params: vec![],
                    project_id: None,
                    target_id: None,
                    toolchain_set_id: None,
                }).await?.into_inner();

                let job_id = resp.job.and_then(|r| r.job_id).map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}");
                if !job_id.is_empty() {
                    stream_job_events_until_done(&addr, &job_id).await?;
                }
            }
            JobCmd::Cancel { addr, job_id } => {
                let mut client = JobServiceClient::new(connect(&addr).await?);
                let resp = client.cancel_job(CancelJobRequest {
                    job_id: Some(Id { value: job_id }),
                }).await?.into_inner();
                println!("accepted={}", resp.accepted);
            }
        },

        Cmd::Toolchain { cmd } => match cmd {
            ToolchainCmd::ListProviders { addr } => {
                let mut client = ToolchainServiceClient::new(connect(&addr).await?);
                let resp = client.list_providers(ListProvidersRequest {}).await?.into_inner();
                for p in resp.providers {
                    let id = p.provider_id.map(|i| i.value).unwrap_or_default();
                    println!("{}\t{}\tkind={}", id, p.name, p.kind);
                }
            }
            ToolchainCmd::ListSets { addr, page_size, page_token } => {
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
                let mut client = ToolchainServiceClient::new(connect(&addr).await?);
                let resp = client.set_active_toolchain_set(SetActiveToolchainSetRequest {
                    toolchain_set_id: Some(Id { value: toolchain_set_id.clone() }),
                }).await?.into_inner();
                println!("ok={}", resp.ok);
            }
            ToolchainCmd::GetActive { addr } => {
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
        },

        Cmd::Targets { cmd } => match cmd {
            TargetsCmd::List { addr } => {
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client.list_targets(ListTargetsRequest { include_offline: true }).await?.into_inner();
                for t in resp.targets {
                    let id = t.target_id.map(|i| i.value).unwrap_or_default();
                    println!("{}\t{}\t{}\t{}", id, t.display_name, t.state, t.provider);
                }
            }
            TargetsCmd::SetDefault { addr, target_id } => {
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client.set_default_target(SetDefaultTargetRequest {
                    target_id: Some(Id { value: target_id.clone() }),
                }).await?.into_inner();
                println!("ok={}", resp.ok);
            }
            TargetsCmd::GetDefault { addr } => {
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client.get_default_target(GetDefaultTargetRequest {}).await?.into_inner();
                if let Some(target) = resp.target {
                    let id = target.target_id.map(|i| i.value).unwrap_or_default();
                    println!("{}\t{}\t{}\t{}", id, target.display_name, target.state, target.provider);
                } else {
                    println!("no default target configured");
                }
            }
            TargetsCmd::StartCuttlefish { addr, show_full_ui } => {
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client.start_cuttlefish(StartCuttlefishRequest { show_full_ui }).await?.into_inner();
                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}");
            }
            TargetsCmd::StopCuttlefish { addr } => {
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client.stop_cuttlefish(StopCuttlefishRequest {}).await?.into_inner();
                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}");
            }
            TargetsCmd::CuttlefishStatus { addr } => {
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client.get_cuttlefish_status(GetCuttlefishStatusRequest {}).await?.into_inner();
                println!("state={}\tadb={}", resp.state, resp.adb_serial);
                for kv in resp.details {
                    println!("{}\t{}", kv.key, kv.value);
                }
            }
            TargetsCmd::InstallCuttlefish { addr, force } => {
                let mut client = TargetServiceClient::new(connect(&addr).await?);
                let resp = client
                    .install_cuttlefish(InstallCuttlefishRequest {
                        force,
                        branch: "".into(),
                        target: "".into(),
                        build_id: "".into(),
                    })
                    .await?
                    .into_inner();
                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}");
            }
        },

        Cmd::Project { cmd } => match cmd {
            ProjectCmd::ListTemplates { addr } => {
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
            ProjectCmd::Create { addr, job_addr, name, path, template_id } => {
                let mut client = ProjectServiceClient::new(connect(&addr).await?);
                let resp = client.create_project(CreateProjectRequest {
                    name,
                    path,
                    template_id: Some(Id { value: template_id }),
                    params: vec![],
                    toolchain_set_id: None,
                }).await?.into_inner();

                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                let project_id = resp.project_id.map(|i| i.value).unwrap_or_default();
                println!("project_id={project_id}\njob_id={job_id}");

                if !job_id.is_empty() {
                    stream_job_events_until_done(&job_addr, &job_id).await?;
                }
            }
            ProjectCmd::Open { addr, path } => {
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
            ObserveCmd::ListRuns { addr, page_size, page_token } => {
                let mut client = ObserveServiceClient::new(connect(&addr).await?);
                let resp = client.list_runs(ListRunsRequest {
                    page: Some(Pagination {
                        page_size,
                        page_token,
                    }),
                }).await?.into_inner();
                for run in resp.runs {
                    let run_id = run.run_id.map(|i| i.value).unwrap_or_default();
                    let started = run.started_at.map(|t| t.unix_millis).unwrap_or(0);
                    let finished = run.finished_at.map(|t| t.unix_millis).unwrap_or(0);
                    println!(
                        "{}\tresult={}\tstarted={}\tfinished={}\tjobs={}",
                        run_id,
                        run.result,
                        started,
                        finished,
                        run.job_ids.len()
                    );
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
            } => {
                let mut client = ObserveServiceClient::new(connect(&addr).await?);
                let resp = client.export_support_bundle(ExportSupportBundleRequest {
                    include_logs,
                    include_config,
                    include_toolchain_provenance,
                    include_recent_runs,
                    recent_runs_limit,
                }).await?.into_inner();
                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}\noutput_path={}", resp.output_path);
                if !job_id.is_empty() {
                    stream_job_events_until_done(&job_addr, &job_id).await?;
                }
            }
            ObserveCmd::ExportEvidence { addr, job_addr, run_id } => {
                let mut client = ObserveServiceClient::new(connect(&addr).await?);
                let resp = client.export_evidence_bundle(ExportEvidenceBundleRequest {
                    run_id: Some(Id { value: run_id }),
                }).await?.into_inner();
                let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
                println!("job_id={job_id}\noutput_path={}", resp.output_path);
                if !job_id.is_empty() {
                    stream_job_events_until_done(&job_addr, &job_id).await?;
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

    while let Some(item) = stream.next().await {
        match item {
            Ok(evt) => {
                if let Some(payload) = evt.payload.as_ref() {
                    if render_job_payload(payload) {
                        break;
                    }
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

fn render_job_payload(payload: &JobPayload) -> bool {
    match payload {
        JobPayload::StateChanged(state) => {
            let new_state = JobState::try_from(state.new_state).unwrap_or(JobState::Unspecified);
            println!("state={new_state:?}");
            matches!(
                new_state,
                JobState::Success | JobState::Failed | JobState::Cancelled
            )
        }
        JobPayload::Progress(progress) => {
            if let Some(p) = progress.progress.as_ref() {
                println!("progress {}% {}", p.percent, p.phase);
                for kv in &p.metrics {
                    println!("  {}={}", kv.key, kv.value);
                }
            }
            false
        }
        JobPayload::Log(log) => {
            if let Some(chunk) = log.chunk.as_ref() {
                print!("{}", String::from_utf8_lossy(&chunk.data));
                let _ = std::io::stdout().flush();
            }
            false
        }
        JobPayload::Completed(completed) => {
            println!("completed: {}", completed.summary);
            for kv in &completed.outputs {
                println!("  {}={}", kv.key, kv.value);
            }
            true
        }
        JobPayload::Failed(failed) => {
            if let Some(err) = failed.error.as_ref() {
                println!("failed: {} ({})", err.message, err.code);
                if !err.technical_details.is_empty() {
                    println!("details: {}", err.technical_details);
                }
            } else {
                println!("failed");
            }
            true
        }
    }
}

async fn connect(addr: &str) -> Result<Channel, Box<dyn std::error::Error>> {
    let endpoint = format!("http://{addr}");
    Ok(Channel::from_shared(endpoint)?.connect().await?)
}
