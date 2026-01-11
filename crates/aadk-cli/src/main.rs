use std::io::Write;

use aadk_proto::aadk::v1::{
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    project_service_client::ProjectServiceClient,
    toolchain_service_client::ToolchainServiceClient,
    target_service_client::TargetServiceClient,
    CancelJobRequest, CreateProjectRequest, GetCuttlefishStatusRequest, Id,
    InstallCuttlefishRequest, JobState, ListProvidersRequest, ListRecentProjectsRequest,
    ListTargetsRequest, ListTemplatesRequest, OpenProjectRequest, Pagination,
    StartCuttlefishRequest, StartJobRequest, StopCuttlefishRequest, StreamJobEventsRequest,
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
}

#[derive(Subcommand)]
enum TargetsCmd {
    /// List targets
    List {
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
                let resp = client.install_cuttlefish(InstallCuttlefishRequest { force }).await?.into_inner();
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
