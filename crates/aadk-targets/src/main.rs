mod adb;
mod cuttlefish;
mod ids;
mod jobs;
mod service;
mod state;

use aadk_proto::aadk::v1::target_service_server::TargetServiceServer;
use aadk_util::serve_grpc_with_telemetry;
use service::Svc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    serve_grpc_with_telemetry(
        "aadk-targets",
        env!("CARGO_PKG_VERSION"),
        "targets",
        "AADK_TARGETS_ADDR",
        aadk_util::DEFAULT_TARGETS_ADDR,
        |server| server.add_service(TargetServiceServer::new(Svc::default())),
    )
    .await
}
