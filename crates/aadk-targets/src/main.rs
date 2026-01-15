mod adb;
mod cuttlefish;
mod ids;
mod jobs;
mod service;
mod state;

use std::net::SocketAddr;

use aadk_proto::aadk::v1::target_service_server::TargetServiceServer;
use service::Svc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();

    let addr_str =
        std::env::var("AADK_TARGETS_ADDR").unwrap_or_else(|_| "127.0.0.1:50055".to_string());
    let addr: SocketAddr = addr_str.parse()?;

    let svc = Svc::default();
    info!("aadk-targets listening on {}", addr);

    tonic::transport::Server::builder()
        .add_service(TargetServiceServer::new(svc))
        .serve(addr)
        .await?;

    Ok(())
}
