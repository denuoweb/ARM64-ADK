use std::net::SocketAddr;

use aadk_proto::aadk::v1::{
    observe_service_server::{ObserveService, ObserveServiceServer},
    ExportEvidenceBundleRequest, ExportEvidenceBundleResponse, ExportSupportBundleRequest, ExportSupportBundleResponse,
    Id, ListRunsRequest, ListRunsResponse, PageInfo,
};
use tonic::{Request, Response, Status};
use tracing::info;
use uuid::Uuid;

#[derive(Clone, Default)]
struct Svc;

#[tonic::async_trait]
impl ObserveService for Svc {
    async fn list_runs(&self, _request: Request<ListRunsRequest>) -> Result<Response<ListRunsResponse>, Status> {
        Ok(Response::new(ListRunsResponse {
            runs: vec![],
            page_info: Some(PageInfo { next_page_token: "".into() }),
        }))
    }

    async fn export_support_bundle(
        &self,
        request: Request<ExportSupportBundleRequest>,
    ) -> Result<Response<ExportSupportBundleResponse>, Status> {
        let _req = request.into_inner();
        Ok(Response::new(ExportSupportBundleResponse {
            job_id: Some(Id { value: format!("job-{}", Uuid::new_v4()) }),
            output_path: "/tmp/aadk-support-bundle.zip".into(),
        }))
    }

    async fn export_evidence_bundle(
        &self,
        request: Request<ExportEvidenceBundleRequest>,
    ) -> Result<Response<ExportEvidenceBundleResponse>, Status> {
        let _req = request.into_inner();
        Ok(Response::new(ExportEvidenceBundleResponse {
            job_id: Some(Id { value: format!("job-{}", Uuid::new_v4()) }),
            output_path: "/tmp/aadk-evidence-bundle.zip".into(),
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let addr_str = std::env::var("AADK_OBSERVE_ADDR").unwrap_or_else(|_| "127.0.0.1:50056".to_string());
    let addr: SocketAddr = addr_str.parse()?;

    let svc = Svc::default();
    info!("aadk-observe listening on {}", addr);

    tonic::transport::Server::builder()
        .add_service(ObserveServiceServer::new(svc))
        .serve(addr)
        .await?;

    Ok(())
}
