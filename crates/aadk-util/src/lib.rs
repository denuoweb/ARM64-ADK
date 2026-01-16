use std::{
    fs, io,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use aadk_proto::aadk::v1::{
    job_service_client::JobServiceClient, Id, JobEvent, ListJobHistoryRequest, Pagination,
    Timestamp,
};
use aadk_telemetry as telemetry;
use serde::Serialize;
use tonic::transport::{server::Router, Channel, Server};
use tracing::info;

pub const DEFAULT_JOB_ADDR: &str = "127.0.0.1:50051";
pub const DEFAULT_TOOLCHAIN_ADDR: &str = "127.0.0.1:50052";
pub const DEFAULT_PROJECT_ADDR: &str = "127.0.0.1:50053";
pub const DEFAULT_BUILD_ADDR: &str = "127.0.0.1:50054";
pub const DEFAULT_TARGETS_ADDR: &str = "127.0.0.1:50055";
pub const DEFAULT_OBSERVE_ADDR: &str = "127.0.0.1:50056";
pub const DEFAULT_WORKFLOW_ADDR: &str = "127.0.0.1:50057";

pub fn env_addr(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

pub fn job_addr() -> String {
    env_addr("AADK_JOB_ADDR", DEFAULT_JOB_ADDR)
}

pub fn toolchain_addr() -> String {
    env_addr("AADK_TOOLCHAIN_ADDR", DEFAULT_TOOLCHAIN_ADDR)
}

pub fn project_addr() -> String {
    env_addr("AADK_PROJECT_ADDR", DEFAULT_PROJECT_ADDR)
}

pub fn build_addr() -> String {
    env_addr("AADK_BUILD_ADDR", DEFAULT_BUILD_ADDR)
}

pub fn targets_addr() -> String {
    env_addr("AADK_TARGETS_ADDR", DEFAULT_TARGETS_ADDR)
}

pub fn observe_addr() -> String {
    env_addr("AADK_OBSERVE_ADDR", DEFAULT_OBSERVE_ADDR)
}

pub fn workflow_addr() -> String {
    env_addr("AADK_WORKFLOW_ADDR", DEFAULT_WORKFLOW_ADDR)
}

pub fn data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/aadk")
    } else {
        PathBuf::from("/tmp/aadk")
    }
}

pub fn state_dir() -> PathBuf {
    data_dir().join("state")
}

pub fn state_file_path(file_name: &str) -> PathBuf {
    state_dir().join(file_name)
}

pub fn expand_user(path: &str) -> PathBuf {
    if path == "~" || path.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            let rest = path.strip_prefix("~/").unwrap_or("");
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn now_ts() -> Timestamp {
    Timestamp {
        unix_millis: now_millis(),
    }
}

pub fn default_export_path(prefix: &str, job_id: &str) -> PathBuf {
    let ts = now_millis();
    state_dir().join(format!("{prefix}-{job_id}-{ts}.json"))
}

pub async fn collect_job_history(
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

pub fn init_tracing() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();
    Ok(())
}

pub fn init_service_telemetry(app_name: &'static str, app_version: &'static str, service_name: &str) {
    telemetry::init_with_env(app_name, app_version);
    telemetry::event("service.start", &[("service", service_name)]);
}

pub async fn serve_grpc<F>(
    app_name: &str,
    addr_env: &str,
    default_addr: &str,
    add_service: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnOnce(&mut Server) -> Router,
{
    let addr_str = env_addr(addr_env, default_addr);
    let addr: SocketAddr = addr_str.parse()?;
    info!("{app_name} listening on {addr}");

    let mut server = Server::builder();
    add_service(&mut server).serve(addr).await?;
    Ok(())
}

pub async fn serve_grpc_with_telemetry<F>(
    app_name: &'static str,
    app_version: &'static str,
    service_name: &str,
    addr_env: &str,
    default_addr: &str,
    add_service: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnOnce(&mut Server) -> Router,
{
    init_tracing()?;
    init_service_telemetry(app_name, app_version, service_name);
    serve_grpc(app_name, addr_env, default_addr, add_service).await
}
