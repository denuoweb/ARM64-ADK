use aadk_proto::aadk::v1::{
    job_service_client::JobServiceClient,
    toolchain_service_client::ToolchainServiceClient,
    target_service_client::TargetServiceClient,
    CancelJobRequest, GetCuttlefishStatusRequest, Id, InstallCuttlefishRequest,
    ListProvidersRequest, ListTargetsRequest, StartCuttlefishRequest, StartJobRequest,
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

fn default_job_addr() -> String {
    std::env::var("AADK_JOB_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".into())
}
fn default_toolchain_addr() -> String {
    std::env::var("AADK_TOOLCHAIN_ADDR").unwrap_or_else(|_| "127.0.0.1:50052".into())
}
fn default_targets_addr() -> String {
    std::env::var("AADK_TARGETS_ADDR").unwrap_or_else(|_| "127.0.0.1:50055".into())
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

                let mut stream = client.stream_job_events(StreamJobEventsRequest {
                    job_id: Some(Id { value: job_id }),
                    include_history: true,
                }).await?.into_inner();

                while let Some(item) = stream.next().await {
                    match item {
                        Ok(evt) => println!("{:?}", evt.payload),
                        Err(e) => { eprintln!("stream error: {e}"); break; }
                    }
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
    }

    Ok(())
}

async fn connect(addr: &str) -> Result<Channel, Box<dyn std::error::Error>> {
    let endpoint = format!("http://{addr}");
    Ok(Channel::from_shared(endpoint)?.connect().await?)
}
