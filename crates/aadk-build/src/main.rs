use std::{
    collections::VecDeque,
    fs,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    time::Instant,
};

use aadk_proto::aadk::v1::{
    build_service_server::{BuildService, BuildServiceServer},
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    project_service_client::ProjectServiceClient,
    Artifact, BuildRequest, BuildResponse, BuildVariant, ErrorCode, ErrorDetail, Id, JobCompleted,
    JobEvent, JobFailed, JobLogAppended, JobProgress, JobProgressUpdated, JobState, JobStateChanged,
    KeyValue, ListArtifactsRequest, ListArtifactsResponse, ListRecentProjectsRequest, LogChunk,
    PublishJobEventRequest, StartJobRequest,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
};
use tonic::{transport::Channel, Request, Response, Status};
use tracing::{info, warn};

const LOG_CHANNEL_CAPACITY: usize = 1024;
const RECENT_LOG_LIMIT: usize = 200;

#[derive(Clone, Default)]
struct Svc;

#[derive(Debug)]
struct LogLine {
    stream: &'static str,
    line: String,
}

fn now_ts() -> aadk_proto::aadk::v1::Timestamp {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    aadk_proto::aadk::v1::Timestamp { unix_millis: ms }
}

fn job_addr() -> String {
    std::env::var("AADK_JOB_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".into())
}

fn project_addr() -> String {
    std::env::var("AADK_PROJECT_ADDR").unwrap_or_else(|_| "127.0.0.1:50053".into())
}

async fn connect_job() -> Result<JobServiceClient<Channel>, Status> {
    let addr = job_addr();
    let endpoint = format!("http://{addr}");
    let channel = Channel::from_shared(endpoint)
        .map_err(|e| Status::internal(format!("invalid job endpoint: {e}")))?
        .connect()
        .await
        .map_err(|e| Status::unavailable(format!("job service unavailable: {e}")))?;
    Ok(JobServiceClient::new(channel))
}

async fn connect_project() -> Result<ProjectServiceClient<Channel>, Status> {
    let addr = project_addr();
    let endpoint = format!("http://{addr}");
    let channel = Channel::from_shared(endpoint)
        .map_err(|e| Status::internal(format!("invalid project endpoint: {e}")))?
        .connect()
        .await
        .map_err(|e| Status::unavailable(format!("project service unavailable: {e}")))?;
    Ok(ProjectServiceClient::new(channel))
}

async fn start_job(
    client: &mut JobServiceClient<Channel>,
    job_type: &str,
    params: Vec<KeyValue>,
    project_id: Option<Id>,
) -> Result<String, Status> {
    let resp = client
        .start_job(StartJobRequest {
            job_type: job_type.into(),
            params,
            project_id,
            target_id: None,
            toolchain_set_id: None,
        })
        .await
        .map_err(|e| Status::unavailable(format!("job start failed: {e}")))?
        .into_inner();

    let job_id = resp
        .job
        .and_then(|r| r.job_id)
        .map(|i| i.value)
        .unwrap_or_default();

    if job_id.is_empty() {
        return Err(Status::internal("job service returned empty job_id"));
    }
    Ok(job_id)
}

async fn publish_job_event(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    payload: JobPayload,
) -> Result<(), Status> {
    client
        .publish_job_event(PublishJobEventRequest {
            event: Some(JobEvent {
                at: Some(now_ts()),
                job_id: Some(Id { value: job_id.to_string() }),
                payload: Some(payload),
            }),
        })
        .await
        .map_err(|e| Status::unavailable(format!("publish job event failed: {e}")))?;
    Ok(())
}

async fn publish_state(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    state: JobState,
) -> Result<(), Status> {
    publish_job_event(
        client,
        job_id,
        JobPayload::StateChanged(JobStateChanged {
            new_state: state as i32,
        }),
    )
    .await
}

async fn publish_progress(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    percent: u32,
    phase: &str,
    metrics: Vec<KeyValue>,
) -> Result<(), Status> {
    publish_job_event(
        client,
        job_id,
        JobPayload::Progress(JobProgressUpdated {
            progress: Some(JobProgress {
                percent,
                phase: phase.into(),
                metrics,
            }),
        }),
    )
    .await
}

async fn publish_log(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    message: &str,
) -> Result<(), Status> {
    publish_job_event(
        client,
        job_id,
        JobPayload::Log(JobLogAppended {
            chunk: Some(LogChunk {
                stream: "build".into(),
                data: message.as_bytes().to_vec(),
                truncated: false,
            }),
        }),
    )
    .await
}

async fn publish_completed(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    summary: &str,
    outputs: Vec<KeyValue>,
) -> Result<(), Status> {
    publish_state(client, job_id, JobState::Success).await?;
    publish_job_event(
        client,
        job_id,
        JobPayload::Completed(JobCompleted {
            summary: summary.into(),
            outputs,
        }),
    )
    .await
}

async fn publish_failed(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    error: ErrorDetail,
) -> Result<(), Status> {
    publish_state(client, job_id, JobState::Failed).await?;
    publish_job_event(
        client,
        job_id,
        JobPayload::Failed(JobFailed { error: Some(error) }),
    )
    .await
}

fn job_error_detail(code: ErrorCode, message: &str, technical: String, correlation_id: &str) -> ErrorDetail {
    ErrorDetail {
        code: code as i32,
        message: message.into(),
        technical_details: technical,
        remedies: vec![],
        correlation_id: correlation_id.into(),
    }
}

fn expand_user(path: &str) -> PathBuf {
    if path == "~" || path.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            let rest = path.strip_prefix("~/").unwrap_or("");
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

async fn lookup_project_path(project_id: &str) -> Result<Option<PathBuf>, Status> {
    let mut client = connect_project().await?;
    let resp = client
        .list_recent_projects(ListRecentProjectsRequest { page: None })
        .await
        .map_err(|e| Status::unavailable(format!("list recent projects failed: {e}")))?
        .into_inner();

    for project in resp.projects {
        let matches = project
            .project_id
            .as_ref()
            .map(|id| id.value.as_str())
            == Some(project_id);
        if matches {
            return Ok(Some(PathBuf::from(project.path)));
        }
    }

    Ok(None)
}

async fn resolve_project_path(project_id: &str) -> Result<PathBuf, Status> {
    let trimmed = project_id.trim();
    if trimmed.is_empty() {
        return Err(Status::invalid_argument("project_id is required"));
    }

    let direct = expand_user(trimmed);
    if direct.is_dir() {
        return Ok(direct);
    }

    if let Ok(root) = std::env::var("AADK_PROJECT_ROOT") {
        let candidate = PathBuf::from(root).join(trimmed);
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }

    match lookup_project_path(trimmed).await? {
        Some(path) => Ok(path),
        None => Err(Status::not_found(format!(
            "project not found: {trimmed}"
        ))),
    }
}

fn variant_label(variant: BuildVariant) -> &'static str {
    match variant {
        BuildVariant::Debug => "debug",
        BuildVariant::Release => "release",
        BuildVariant::Unspecified => "unspecified",
    }
}

fn tasks_for_variant(variant: BuildVariant, clean_first: bool) -> Vec<String> {
    let task = match variant {
        BuildVariant::Release => "assembleRelease",
        _ => "assembleDebug",
    };

    if clean_first {
        vec!["clean".into(), task.into()]
    } else {
        vec![task.into()]
    }
}

fn arg_is_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn gradle_daemon_enabled() -> bool {
    matches!(
        std::env::var("AADK_GRADLE_DAEMON"),
        Ok(val) if val == "1" || val.eq_ignore_ascii_case("true")
    )
}

fn gradle_stacktrace_enabled() -> bool {
    matches!(
        std::env::var("AADK_GRADLE_STACKTRACE"),
        Ok(val) if val == "1" || val.eq_ignore_ascii_case("true")
    )
}

fn expand_gradle_args(items: Vec<KeyValue>) -> Vec<String> {
    let mut args = Vec::new();
    for item in items {
        let key = item.key.trim();
        let value = item.value.trim();
        if key.is_empty() {
            continue;
        }

        if value.is_empty() {
            args.push(key.to_string());
            continue;
        }

        if key == "-P" || key == "-D" || key.ends_with('=') {
            args.push(format!("{key}{value}"));
            continue;
        }

        if key.starts_with('-') {
            args.push(key.to_string());
            args.push(value.to_string());
            continue;
        }

        args.push(format!("{key}={value}"));
    }
    args
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

fn spawn_error(err: io::Error) -> Status {
    if err.kind() == io::ErrorKind::NotFound {
        Status::failed_precondition("gradle not found (missing gradlew or gradle in PATH)")
    } else {
        Status::internal(format!("failed to spawn gradle: {err}"))
    }
}

fn spawn_gradle(project_dir: &Path, args: &[String]) -> Result<Child, Status> {
    let wrapper = project_dir.join("gradlew");
    let mut cmd = if wrapper.is_file() {
        if is_executable(&wrapper) {
            Command::new(wrapper)
        } else {
            let mut cmd = Command::new("sh");
            cmd.arg(wrapper);
            cmd
        }
    } else {
        Command::new("gradle")
    };

    cmd.current_dir(project_dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    cmd.spawn().map_err(spawn_error)
}

async fn read_lines<R>(reader: R, stream: &'static str, tx: mpsc::Sender<LogLine>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let payload = LogLine { stream, line };
        if tx.send(payload).await.is_err() {
            break;
        }
    }
}

fn append_recent(recent: &mut VecDeque<String>, line: String) {
    if recent.len() >= RECENT_LOG_LIMIT {
        recent.pop_front();
    }
    recent.push_back(line);
}

fn collect_recent(recent: &VecDeque<String>) -> String {
    let mut combined = String::new();
    for line in recent {
        combined.push_str(line);
        if !line.ends_with('\n') {
            combined.push('\n');
        }
    }
    combined
}

fn list_apk_roots(project_path: &Path) -> Vec<(Option<String>, PathBuf)> {
    let mut roots = Vec::new();
    let root_build = project_path.join("build").join("outputs").join("apk");
    if root_build.is_dir() {
        roots.push((None, root_build));
    }

    if let Ok(entries) = fs::read_dir(project_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let module_name = entry.file_name().to_string_lossy().to_string();
            let module_root = path.join("build").join("outputs").join("apk");
            if module_root.is_dir() {
                roots.push((Some(module_name), module_root));
            }
        }
    }

    roots
}

fn collect_apk_paths(root: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_apk_paths(&path, out)?;
        } else if path.extension().map(|e| e == "apk").unwrap_or(false) {
            out.push(path);
        }
    }
    Ok(())
}

fn variant_dir(variant: BuildVariant) -> Option<&'static str> {
    match variant {
        BuildVariant::Debug => Some("debug"),
        BuildVariant::Release => Some("release"),
        BuildVariant::Unspecified => None,
    }
}

fn collect_artifacts(project_path: &Path, variant: BuildVariant) -> Vec<Artifact> {
    let mut artifacts = Vec::new();
    let roots = list_apk_roots(project_path);
    let variant_filter = variant_dir(variant);

    for (module, root) in roots {
        let search_root = if let Some(dir) = variant_filter {
            let candidate = root.join(dir);
            if candidate.is_dir() {
                candidate
            } else {
                root.clone()
            }
        } else {
            root.clone()
        };

        let mut paths = Vec::new();
        if collect_apk_paths(&search_root, &mut paths).is_err() {
            continue;
        }

        for path in paths {
            if !path.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("app.apk")
                .to_string();
            let size_bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let mut metadata = Vec::new();
            if let Some(module) = module.clone() {
                metadata.push(KeyValue {
                    key: "module".into(),
                    value: module,
                });
            }
            if let Some(dir) = variant_filter {
                metadata.push(KeyValue {
                    key: "variant".into(),
                    value: dir.into(),
                });
            }

            artifacts.push(Artifact {
                name,
                path: path.to_string_lossy().to_string(),
                size_bytes,
                sha256: "".into(),
                metadata,
            });
        }
    }

    artifacts
}

async fn run_build_job(job_id: String, req: BuildRequest) {
    let mut job_client = match connect_job().await {
        Ok(client) => client,
        Err(err) => {
            warn!("build job {job_id}: failed to connect job service: {err}");
            return;
        }
    };

    let _ = publish_state(&mut job_client, &job_id, JobState::Running).await;
    let _ = publish_log(&mut job_client, &job_id, "Starting Gradle build\n").await;

    let project_id = match req.project_id.as_ref() {
        Some(id) if !id.value.trim().is_empty() => id.value.clone(),
        _ => {
            let detail = job_error_detail(
                ErrorCode::InvalidArgument,
                "project_id is required",
                "missing project_id in BuildRequest".into(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    let project_path = match resolve_project_path(&project_id).await {
        Ok(path) => path,
        Err(err) => {
            let code = match err.code() {
                tonic::Code::NotFound => ErrorCode::NotFound,
                tonic::Code::Unavailable => ErrorCode::Unavailable,
                _ => ErrorCode::InvalidArgument,
            };
            let detail = job_error_detail(
                code,
                "project resolution failed",
                err.message().to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    let variant = BuildVariant::try_from(req.variant).unwrap_or(BuildVariant::Debug);
    let mut args = tasks_for_variant(variant, req.clean_first);
    let extra_args = expand_gradle_args(req.gradle_args);
    args.extend(extra_args);

    if !gradle_daemon_enabled() && !arg_is_flag(&args, "--no-daemon") {
        args.push("--no-daemon".into());
    }
    if gradle_stacktrace_enabled() && !arg_is_flag(&args, "--stacktrace") {
        args.push("--stacktrace".into());
    }

    let _ = publish_progress(
        &mut job_client,
        &job_id,
        10,
        "preflight",
        vec![
            KeyValue {
                key: "variant".into(),
                value: variant_label(variant).into(),
            },
            KeyValue {
                key: "project".into(),
                value: project_path.to_string_lossy().to_string(),
            },
        ],
    )
    .await;

    let _ = publish_log(
        &mut job_client,
        &job_id,
        &format!("Working dir: {}\n", project_path.display()),
    )
    .await;

    let _ = publish_log(
        &mut job_client,
        &job_id,
        &format!("Gradle args: {}\n", args.join(" ")),
    )
    .await;

    let mut child = match spawn_gradle(&project_path, &args) {
        Ok(child) => child,
        Err(err) => {
            let detail = job_error_detail(
                ErrorCode::BuildFailed,
                "failed to start Gradle",
                err.message().to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    let stdout = match child.stdout.take() {
        Some(out) => out,
        None => {
            let detail = job_error_detail(
                ErrorCode::BuildFailed,
                "failed to capture gradle stdout",
                "stdout pipe missing".into(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    let stderr = match child.stderr.take() {
        Some(err) => err,
        None => {
            let detail = job_error_detail(
                ErrorCode::BuildFailed,
                "failed to capture gradle stderr",
                "stderr pipe missing".into(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    let (line_tx, mut line_rx) = mpsc::channel::<LogLine>(LOG_CHANNEL_CAPACITY);
    tokio::spawn(read_lines(stdout, "stdout", line_tx.clone()));
    tokio::spawn(read_lines(stderr, "stderr", line_tx));

    let _ = publish_progress(&mut job_client, &job_id, 25, "gradle running", vec![]).await;

    let mut recent = VecDeque::with_capacity(RECENT_LOG_LIMIT);
    let start = Instant::now();
    let mut status = None;
    let wait = child.wait();
    tokio::pin!(wait);

    loop {
        tokio::select! {
            line = line_rx.recv() => {
                match line {
                    Some(line) => {
                        let mut text = format!("[{}] {}", line.stream, line.line);
                        if !text.ends_with('\n') {
                            text.push('\n');
                        }
                        append_recent(&mut recent, text.clone());
                        let _ = publish_log(&mut job_client, &job_id, &text).await;
                    }
                    None => {
                        if status.is_some() {
                            break;
                        }
                    }
                }
            }
            result = &mut wait, if status.is_none() => {
                status = Some(result);
            }
        }
    }

    let status = match status {
        Some(Ok(status)) => status,
        Some(Err(err)) => {
            let detail = job_error_detail(
                ErrorCode::BuildFailed,
                "gradle process failed",
                err.to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
        None => {
            let detail = job_error_detail(
                ErrorCode::BuildFailed,
                "gradle process did not return status",
                "missing exit status".into(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    let duration_ms = start.elapsed().as_millis();

    if status.success() {
        let artifacts = collect_artifacts(&project_path, variant);
        let mut outputs = vec![
            KeyValue {
                key: "duration_ms".into(),
                value: duration_ms.to_string(),
            },
            KeyValue {
                key: "artifact_count".into(),
                value: artifacts.len().to_string(),
            },
        ];

        for artifact in artifacts.iter().take(10) {
            outputs.push(KeyValue {
                key: "apk_path".into(),
                value: artifact.path.clone(),
            });
        }

        let _ = publish_progress(&mut job_client, &job_id, 95, "finalizing", vec![]).await;
        let _ = publish_completed(
            &mut job_client,
            &job_id,
            "Gradle build finished",
            outputs,
        )
        .await;
    } else {
        let code = status.code().unwrap_or(-1);
        let mut detail = format!("exit_code={code}\n");
        detail.push_str(&collect_recent(&recent));
        let detail = job_error_detail(
            ErrorCode::BuildFailed,
            "Gradle build failed",
            detail,
            &job_id,
        );
        let _ = publish_failed(&mut job_client, &job_id, detail).await;
    }
}

#[tonic::async_trait]
impl BuildService for Svc {
    async fn build(&self, request: Request<BuildRequest>) -> Result<Response<BuildResponse>, Status> {
        let req = request.into_inner();
        let project_id = req
            .project_id
            .clone()
            .ok_or_else(|| Status::invalid_argument("project_id is required"))?;

        let variant = BuildVariant::try_from(req.variant).unwrap_or(BuildVariant::Debug);
        let mut job_client = connect_job().await?;
        let job_id = start_job(
            &mut job_client,
            "build.run",
            vec![
                KeyValue {
                    key: "variant".into(),
                    value: variant_label(variant).into(),
                },
                KeyValue {
                    key: "clean_first".into(),
                    value: req.clean_first.to_string(),
                },
            ],
            Some(project_id),
        )
        .await?;

        tokio::spawn(run_build_job(job_id.clone(), req));

        Ok(Response::new(BuildResponse {
            job_id: Some(Id { value: job_id }),
        }))
    }

    async fn list_artifacts(
        &self,
        request: Request<ListArtifactsRequest>,
    ) -> Result<Response<ListArtifactsResponse>, Status> {
        let req = request.into_inner();
        let project_id = req
            .project_id
            .as_ref()
            .map(|id| id.value.clone())
            .unwrap_or_default();

        let project_path = resolve_project_path(&project_id).await?;
        let variant = BuildVariant::try_from(req.variant).unwrap_or(BuildVariant::Unspecified);
        let artifacts = collect_artifacts(&project_path, variant);

        Ok(Response::new(ListArtifactsResponse { artifacts }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let addr_str = std::env::var("AADK_BUILD_ADDR").unwrap_or_else(|_| "127.0.0.1:50054".to_string());
    let addr: SocketAddr = addr_str.parse()?;

    let svc = Svc::default();
    info!("aadk-build listening on {}", addr);

    tonic::transport::Server::builder()
        .add_service(BuildServiceServer::new(svc))
        .serve(addr)
        .await?;

    Ok(())
}
