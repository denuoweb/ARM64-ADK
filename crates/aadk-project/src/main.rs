use std::{net::SocketAddr, sync::Arc};

use aadk_proto::aadk::v1::{
    project_service_server::{ProjectService, ProjectServiceServer},
    CreateProjectRequest, CreateProjectResponse, Id, ListRecentProjectsRequest, ListRecentProjectsResponse,
    ListTemplatesRequest, ListTemplatesResponse, OpenProjectRequest, OpenProjectResponse,
    PageInfo, Project, SetProjectConfigRequest, SetProjectConfigResponse, Template, Timestamp, KeyValue,
};
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::info;
use uuid::Uuid;

#[derive(Default)]
struct State {
    recent: Vec<Project>,
}

#[derive(Clone, Default)]
struct Svc {
    state: Arc<Mutex<State>>,
}

fn now_ts() -> Timestamp {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    Timestamp { unix_millis: ms }
}

fn compose_counter_template() -> Template {
    Template {
        template_id: Some(Id { value: "tmpl-compose-counter".into() }),
        name: "Compose Counter".into(),
        description: "Jetpack Compose + Kotlin counter that updates UI state".into(),
        defaults: vec![
            KeyValue { key: "minSdk".into(), value: "24".into() },
            KeyValue { key: "compileSdk".into(), value: "35".into() },
        ],
    }
}

#[tonic::async_trait]
impl ProjectService for Svc {
    async fn list_templates(
        &self,
        _request: Request<ListTemplatesRequest>,
    ) -> Result<Response<ListTemplatesResponse>, Status> {
        Ok(Response::new(ListTemplatesResponse {
            templates: vec![compose_counter_template()],
        }))
    }

    async fn create_project(
        &self,
        request: Request<CreateProjectRequest>,
    ) -> Result<Response<CreateProjectResponse>, Status> {
        let req = request.into_inner();
        let project_id = format!("proj-{}", Uuid::new_v4());
        let job_id = format!("job-{}", Uuid::new_v4());

        let proj = Project {
            project_id: Some(Id { value: project_id.clone() }),
            name: req.name.clone(),
            path: req.path.clone(),
            created_at: Some(now_ts()),
            last_opened_at: Some(now_ts()),
            toolchain_set_id: req.toolchain_set_id,
            default_target_id: None,
        };

        let mut st = self.state.lock().await;
        st.recent.insert(0, proj.clone());

        Ok(Response::new(CreateProjectResponse {
            job_id: Some(Id { value: job_id }),
            project_id: Some(Id { value: project_id }),
        }))
    }

    async fn open_project(
        &self,
        request: Request<OpenProjectRequest>,
    ) -> Result<Response<OpenProjectResponse>, Status> {
        let req = request.into_inner();
        // Stub: treat the path as a project, invent ID/name from folder.
        let name = std::path::Path::new(&req.path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("Project")
            .to_string();

        let proj = Project {
            project_id: Some(Id { value: format!("proj-{}", Uuid::new_v4()) }),
            name,
            path: req.path,
            created_at: Some(now_ts()),
            last_opened_at: Some(now_ts()),
            toolchain_set_id: None,
            default_target_id: None,
        };

        let mut st = self.state.lock().await;
        st.recent.insert(0, proj.clone());

        Ok(Response::new(OpenProjectResponse { project: Some(proj) }))
    }

    async fn list_recent_projects(
        &self,
        _request: Request<ListRecentProjectsRequest>,
    ) -> Result<Response<ListRecentProjectsResponse>, Status> {
        let st = self.state.lock().await;
        Ok(Response::new(ListRecentProjectsResponse {
            projects: st.recent.clone(),
            page_info: Some(PageInfo { next_page_token: "".into() }),
        }))
    }

    async fn set_project_config(
        &self,
        _request: Request<SetProjectConfigRequest>,
    ) -> Result<Response<SetProjectConfigResponse>, Status> {
        Ok(Response::new(SetProjectConfigResponse { ok: true }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let addr_str = std::env::var("AADK_PROJECT_ADDR").unwrap_or_else(|_| "127.0.0.1:50053".to_string());
    let addr: SocketAddr = addr_str.parse()?;

    let svc = Svc::default();
    info!("aadk-project listening on {}", addr);

    tonic::transport::Server::builder()
        .add_service(ProjectServiceServer::new(svc))
        .serve(addr)
        .await?;

    Ok(())
}
