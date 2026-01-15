mod artifacts;
mod cancel;
mod catalog;
mod hashing;
mod jobs;
mod provenance;
mod service;
mod state;
mod verify;

use std::net::SocketAddr;

use aadk_proto::aadk::v1::toolchain_service_server::ToolchainServiceServer;
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
        std::env::var("AADK_TOOLCHAIN_ADDR").unwrap_or_else(|_| "127.0.0.1:50052".to_string());
    let addr: SocketAddr = addr_str.parse()?;

    let svc = Svc::default();
    info!("aadk-toolchain listening on {}", addr);

    tonic::transport::Server::builder()
        .add_service(ToolchainServiceServer::new(svc))
        .serve(addr)
        .await?;

    Ok(())
}
