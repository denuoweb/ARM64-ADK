mod artifacts;
mod cancel;
mod catalog;
mod hashing;
mod jobs;
mod provenance;
mod service;
mod state;
mod verify;

use aadk_proto::aadk::v1::toolchain_service_server::ToolchainServiceServer;
use aadk_util::serve_grpc_with_telemetry;
use service::Svc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    serve_grpc_with_telemetry(
        "aadk-toolchain",
        env!("CARGO_PKG_VERSION"),
        "toolchain",
        "AADK_TOOLCHAIN_ADDR",
        aadk_util::DEFAULT_TOOLCHAIN_ADDR,
        |server| server.add_service(ToolchainServiceServer::new(Svc::default())),
    )
    .await
}
