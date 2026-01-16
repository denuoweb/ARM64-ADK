use std::path::Path;

use aadk_proto::aadk::v1::{
    build_service_client::BuildServiceClient,
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    observe_service_client::ObserveServiceClient,
    project_service_client::ProjectServiceClient,
    target_service_client::TargetServiceClient,
    toolchain_service_client::ToolchainServiceClient,
    workflow_service_server::{WorkflowService, WorkflowServiceServer},
    Artifact, ArtifactFilter, ArtifactType, BuildRequest, BuildVariant, CreateProjectRequest,
    ErrorCode, ErrorDetail, ExportEvidenceBundleRequest, ExportSupportBundleRequest, GetJobRequest,
    Id, InstallApkRequest, JobCompleted, JobEvent, JobFailed, JobLogAppended, JobProgress,
    JobProgressUpdated, JobState, JobStateChanged, KeyValue, LaunchRequest, ListArtifactsRequest,
    OpenProjectRequest, PublishJobEventRequest, RunId, RunOutput, RunOutputKind, StartJobRequest,
    StreamJobEventsRequest, Timestamp, UpsertRunOutputsRequest, UpsertRunRequest,
    WorkflowPipelineRequest, WorkflowPipelineResponse,
};
use aadk_util::{
    build_addr, job_addr, now_millis, observe_addr, project_addr, serve_grpc_with_telemetry,
    targets_addr, toolchain_addr,
};
use futures_util::StreamExt;
use tonic::{transport::Channel, Request, Response, Status};
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Clone)]
struct WorkflowConfig {
    job_addr: String,
    toolchain_addr: String,
    project_addr: String,
    build_addr: String,
    targets_addr: String,
    observe_addr: String,
}

#[derive(Clone)]
struct Svc {
    config: WorkflowConfig,
}

fn metric(key: &str, value: impl ToString) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: value.to_string(),
    }
}

async fn connect_job(addr: &str) -> Result<JobServiceClient<Channel>, Status> {
    let endpoint = format!("http://{addr}");
    let channel = Channel::from_shared(endpoint)
        .map_err(|e| Status::internal(format!("invalid job endpoint: {e}")))?
        .connect()
        .await
        .map_err(|e| Status::unavailable(format!("job service unavailable: {e}")))?;
    Ok(JobServiceClient::new(channel))
}

async fn connect_project(addr: &str) -> Result<ProjectServiceClient<Channel>, Status> {
    let endpoint = format!("http://{addr}");
    let channel = Channel::from_shared(endpoint)
        .map_err(|e| Status::internal(format!("invalid project endpoint: {e}")))?
        .connect()
        .await
        .map_err(|e| Status::unavailable(format!("project service unavailable: {e}")))?;
    Ok(ProjectServiceClient::new(channel))
}

async fn connect_build(addr: &str) -> Result<BuildServiceClient<Channel>, Status> {
    let endpoint = format!("http://{addr}");
    let channel = Channel::from_shared(endpoint)
        .map_err(|e| Status::internal(format!("invalid build endpoint: {e}")))?
        .connect()
        .await
        .map_err(|e| Status::unavailable(format!("build service unavailable: {e}")))?;
    Ok(BuildServiceClient::new(channel))
}

async fn connect_toolchain(addr: &str) -> Result<ToolchainServiceClient<Channel>, Status> {
    let endpoint = format!("http://{addr}");
    let channel = Channel::from_shared(endpoint)
        .map_err(|e| Status::internal(format!("invalid toolchain endpoint: {e}")))?
        .connect()
        .await
        .map_err(|e| Status::unavailable(format!("toolchain service unavailable: {e}")))?;
    Ok(ToolchainServiceClient::new(channel))
}

async fn connect_targets(addr: &str) -> Result<TargetServiceClient<Channel>, Status> {
    let endpoint = format!("http://{addr}");
    let channel = Channel::from_shared(endpoint)
        .map_err(|e| Status::internal(format!("invalid targets endpoint: {e}")))?
        .connect()
        .await
        .map_err(|e| Status::unavailable(format!("targets service unavailable: {e}")))?;
    Ok(TargetServiceClient::new(channel))
}

async fn connect_observe(addr: &str) -> Result<ObserveServiceClient<Channel>, Status> {
    let endpoint = format!("http://{addr}");
    let channel = Channel::from_shared(endpoint)
        .map_err(|e| Status::internal(format!("invalid observe endpoint: {e}")))?
        .connect()
        .await
        .map_err(|e| Status::unavailable(format!("observe service unavailable: {e}")))?;
    Ok(ObserveServiceClient::new(channel))
}

async fn publish_job_event(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    payload: JobPayload,
) -> Result<(), Status> {
    client
        .publish_job_event(PublishJobEventRequest {
            event: Some(JobEvent {
                at: None,
                job_id: Some(Id {
                    value: job_id.to_string(),
                }),
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

async fn publish_log(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    line: &str,
) -> Result<(), Status> {
    publish_job_event(
        client,
        job_id,
        JobPayload::Log(JobLogAppended {
            chunk: Some(aadk_proto::aadk::v1::LogChunk {
                stream: "stdout".into(),
                data: line.as_bytes().to_vec(),
                truncated: false,
            }),
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

async fn publish_completed(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    summary: &str,
    outputs: Vec<KeyValue>,
) -> Result<(), Status> {
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
    detail: ErrorDetail,
) -> Result<(), Status> {
    publish_job_event(
        client,
        job_id,
        JobPayload::Failed(JobFailed {
            error: Some(detail),
        }),
    )
    .await
}

fn job_error_detail(
    code: ErrorCode,
    message: &str,
    technical: &str,
    correlation_id: &str,
) -> ErrorDetail {
    ErrorDetail {
        code: code as i32,
        message: message.into(),
        technical_details: technical.into(),
        remedies: vec![],
        correlation_id: correlation_id.into(),
    }
}

async fn fail_pipeline(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    correlation_id: &str,
    message: &str,
    technical: &str,
) {
    let detail = job_error_detail(ErrorCode::Internal, message, technical, correlation_id);
    let _ = publish_failed(client, job_id, detail).await;
}

#[allow(clippy::too_many_arguments)]
async fn fail_pipeline_and_record(
    config: &WorkflowConfig,
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    run_id: &str,
    correlation_id: &str,
    project_id: &str,
    target_id: &str,
    toolchain_set_id: &str,
    job_ids: &[String],
    started_at: i64,
    message: &str,
    technical: &str,
) {
    fail_pipeline(client, job_id, correlation_id, message, technical).await;
    let summary = vec![metric("error", message), metric("detail", technical)];
    let _ = upsert_run_best_effort(
        &config.observe_addr,
        run_id,
        correlation_id,
        project_id,
        target_id,
        toolchain_set_id,
        job_ids,
        "failed",
        summary,
        Some(started_at),
        Some(now_millis()),
    )
    .await;
}

fn resolve_project_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "aadk-project".to_string())
}

fn resolve_variant(variant: i32) -> BuildVariant {
    match BuildVariant::try_from(variant).unwrap_or(BuildVariant::Unspecified) {
        BuildVariant::Unspecified => BuildVariant::Debug,
        other => other,
    }
}

fn select_artifact_path(artifacts: &[Artifact]) -> Option<String> {
    let mut apk = None;
    let mut fallback = None;
    for artifact in artifacts {
        if fallback.is_none() {
            fallback = Some(artifact.path.clone());
        }
        let kind = ArtifactType::try_from(artifact.r#type).unwrap_or(ArtifactType::Unspecified);
        if matches!(kind, ArtifactType::Apk) {
            apk = Some(artifact.path.clone());
            break;
        }
    }
    apk.or(fallback)
}

fn artifact_type_label(artifact_type: ArtifactType) -> &'static str {
    match artifact_type {
        ArtifactType::Apk => "apk",
        ArtifactType::Aab => "aab",
        ArtifactType::Aar => "aar",
        ArtifactType::Mapping => "mapping",
        ArtifactType::TestResult => "test_result",
        ArtifactType::Unspecified => "unspecified",
    }
}

async fn wait_for_job(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
) -> Result<JobState, Status> {
    let mut stream = client
        .stream_job_events(StreamJobEventsRequest {
            job_id: Some(Id {
                value: job_id.to_string(),
            }),
            include_history: true,
        })
        .await?
        .into_inner();

    while let Some(evt) = stream.next().await {
        let evt = evt?;
        match evt.payload {
            Some(JobPayload::Completed(_)) => return Ok(JobState::Success),
            Some(JobPayload::Failed(_)) => return Ok(JobState::Failed),
            Some(JobPayload::StateChanged(state)) => {
                let new_state =
                    JobState::try_from(state.new_state).unwrap_or(JobState::Unspecified);
                if matches!(
                    new_state,
                    JobState::Success | JobState::Failed | JobState::Cancelled
                ) {
                    return Ok(new_state);
                }
            }
            _ => {}
        }
    }

    let resp = client
        .get_job(GetJobRequest {
            job_id: Some(Id {
                value: job_id.to_string(),
            }),
        })
        .await?
        .into_inner();
    let state = resp
        .job
        .map(|job| JobState::try_from(job.state).unwrap_or(JobState::Unspecified))
        .unwrap_or(JobState::Unspecified);
    Ok(state)
}

#[allow(clippy::too_many_arguments)]
async fn upsert_run_best_effort(
    observe_addr: &str,
    run_id: &str,
    correlation_id: &str,
    project_id: &str,
    target_id: &str,
    toolchain_set_id: &str,
    job_ids: &[String],
    result: &str,
    summary: Vec<KeyValue>,
    started_at: Option<i64>,
    finished_at: Option<i64>,
) -> Result<(), Status> {
    let mut client = connect_observe(observe_addr).await?;
    client
        .upsert_run(UpsertRunRequest {
            run_id: Some(RunId {
                value: run_id.to_string(),
            }),
            correlation_id: correlation_id.to_string(),
            project_id: if project_id.is_empty() {
                None
            } else {
                Some(Id {
                    value: project_id.to_string(),
                })
            },
            target_id: if target_id.is_empty() {
                None
            } else {
                Some(Id {
                    value: target_id.to_string(),
                })
            },
            toolchain_set_id: if toolchain_set_id.is_empty() {
                None
            } else {
                Some(Id {
                    value: toolchain_set_id.to_string(),
                })
            },
            job_ids: job_ids.iter().map(|id| Id { value: id.clone() }).collect(),
            result: result.to_string(),
            summary,
            started_at: started_at.map(|ms| Timestamp { unix_millis: ms }),
            finished_at: finished_at.map(|ms| Timestamp { unix_millis: ms }),
        })
        .await?;
    Ok(())
}

async fn upsert_run_outputs_best_effort(
    observe_addr: &str,
    run_id: &str,
    job_id: &str,
    artifacts: &[Artifact],
) {
    if run_id.trim().is_empty() || artifacts.is_empty() {
        return;
    }

    let mut client = match connect_observe(observe_addr).await {
        Ok(client) => client,
        Err(err) => {
            warn!("run outputs: observe unavailable: {err}");
            return;
        }
    };

    let mut outputs = Vec::new();
    for artifact in artifacts {
        let path = artifact.path.trim();
        if path.is_empty() {
            continue;
        }
        let artifact_type =
            ArtifactType::try_from(artifact.r#type).unwrap_or(ArtifactType::Unspecified);
        let output_type = artifact_type_label(artifact_type).to_string();
        let label = if artifact.name.trim().is_empty() {
            path.to_string()
        } else {
            artifact.name.trim().to_string()
        };
        let mut metadata = artifact.metadata.clone();
        metadata.push(KeyValue {
            key: "artifact_type".into(),
            value: output_type.clone(),
        });
        if !artifact.name.trim().is_empty() {
            metadata.push(KeyValue {
                key: "artifact_name".into(),
                value: artifact.name.clone(),
            });
        }
        outputs.push(RunOutput {
            output_id: format!("artifact:{job_id}:{path}"),
            run_id: Some(RunId {
                value: run_id.to_string(),
            }),
            kind: RunOutputKind::Artifact as i32,
            output_type,
            path: path.to_string(),
            label,
            job_id: Some(Id {
                value: job_id.to_string(),
            }),
            created_at: Some(Timestamp {
                unix_millis: now_millis(),
            }),
            metadata,
        });
    }

    if outputs.is_empty() {
        return;
    }

    if let Err(err) = client
        .upsert_run_outputs(UpsertRunOutputsRequest {
            run_id: Some(RunId {
                value: run_id.to_string(),
            }),
            outputs,
        })
        .await
    {
        warn!("run outputs: upsert failed for {run_id}: {err}");
    }
}

async fn run_pipeline_task(
    config: WorkflowConfig,
    pipeline_job_id: String,
    run_id: String,
    correlation_id: String,
    req: WorkflowPipelineRequest,
) {
    let mut job_client = match connect_job(&config.job_addr).await {
        Ok(client) => client,
        Err(err) => {
            warn!("pipeline job client error: {err}");
            return;
        }
    };

    let started_at = now_millis();
    let options = req.options;
    let inferred = options.is_none();
    let options = options.unwrap_or_default();

    let mut project_id = req
        .project_id
        .as_ref()
        .map(|id| id.value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let project_path = req.project_path.trim().to_string();
    let project_name = if !req.project_name.trim().is_empty() {
        req.project_name.trim().to_string()
    } else if !project_path.is_empty() {
        resolve_project_name(&project_path)
    } else {
        "aadk-project".to_string()
    };
    let template_id = req
        .template_id
        .as_ref()
        .map(|id| id.value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let toolchain_id = req
        .toolchain_id
        .as_ref()
        .map(|id| id.value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let toolchain_set_id = req
        .toolchain_set_id
        .as_ref()
        .map(|id| id.value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let target_id = req
        .target_id
        .as_ref()
        .map(|id| id.value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let application_id = req.application_id.trim().to_string();
    let activity = req.activity.trim().to_string();
    let mut apk_path = req.apk_path.trim().to_string();

    let wants_create = if inferred {
        !template_id.is_empty()
    } else {
        options.create_project
    };
    let wants_open = if inferred {
        project_id.is_empty() && !project_path.is_empty()
    } else {
        options.open_project
    };
    let wants_verify = if inferred {
        !toolchain_id.is_empty()
    } else {
        options.verify_toolchain
    };
    let wants_build = if inferred {
        !project_id.is_empty() || !project_path.is_empty()
    } else {
        options.build
    };
    let wants_install = if inferred {
        !apk_path.is_empty()
    } else {
        options.install_apk
    };
    let wants_launch = if inferred {
        !application_id.is_empty()
    } else {
        options.launch_app
    };
    let wants_support = if inferred {
        false
    } else {
        options.export_support_bundle
    };
    let wants_evidence = if inferred {
        false
    } else {
        options.export_evidence_bundle
    };

    let mut total_steps = 0u32;
    for step in [
        wants_create,
        wants_open,
        wants_verify,
        wants_build,
        wants_install,
        wants_launch,
        wants_support,
        wants_evidence,
    ] {
        if step {
            total_steps += 1;
        }
    }
    if total_steps == 0 {
        total_steps = 1;
    }

    let mut step_index = 0u32;
    let mut job_ids = Vec::new();
    let mut outputs = vec![
        metric("run_id", &run_id),
        metric("correlation_id", &correlation_id),
    ];

    let mut summary = Vec::new();
    let _ = publish_log(
        &mut job_client,
        &pipeline_job_id,
        &format!("pipeline run_id={run_id} correlation_id={correlation_id}\n"),
    )
    .await;
    let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Running).await;

    if let Err(err) = upsert_run_best_effort(
        &config.observe_addr,
        &run_id,
        &correlation_id,
        &project_id,
        &target_id,
        &toolchain_set_id,
        &job_ids,
        "running",
        vec![],
        Some(started_at),
        None,
    )
    .await
    {
        let _ = publish_log(
            &mut job_client,
            &pipeline_job_id,
            &format!("WARN: failed to upsert run start: {err}\n"),
        )
        .await;
    }

    if wants_create {
        step_index += 1;
        let percent = step_index * 100 / total_steps;
        let mut metrics = vec![
            metric("pipeline_step", "project.create"),
            metric("step_index", step_index),
            metric("total_steps", total_steps),
            metric("run_id", &run_id),
            metric("correlation_id", &correlation_id),
        ];
        if !template_id.is_empty() {
            metrics.push(metric("template_id", &template_id));
        }
        if !project_path.is_empty() {
            metrics.push(metric("project_path", &project_path));
        }
        let _ = publish_progress(
            &mut job_client,
            &pipeline_job_id,
            percent,
            "project.create",
            metrics,
        )
        .await;

        if template_id.is_empty() || project_path.is_empty() {
            let msg = "project.create requires template_id and project_path";
            if !inferred && options.create_project {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "pipeline failed",
                    msg,
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
            let _ = publish_log(
                &mut job_client,
                &pipeline_job_id,
                &format!("WARN: {msg}, skipping\n"),
            )
            .await;
        } else {
            let mut project_client = match connect_project(&config.project_addr).await {
                Ok(client) => client,
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "project service unavailable",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            let resp = project_client
                .create_project(CreateProjectRequest {
                    name: project_name.clone(),
                    path: project_path.clone(),
                    template_id: Some(Id {
                        value: template_id.clone(),
                    }),
                    params: vec![],
                    toolchain_set_id: if toolchain_set_id.is_empty() {
                        None
                    } else {
                        Some(Id {
                            value: toolchain_set_id.clone(),
                        })
                    },
                    job_id: None,
                    correlation_id: correlation_id.clone(),
                    run_id: Some(RunId {
                        value: run_id.clone(),
                    }),
                })
                .await;
            let resp = match resp {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "project.create failed",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            if let Some(job_id) = resp
                .job_id
                .as_ref()
                .map(|id| id.value.clone())
                .filter(|value| !value.is_empty())
            {
                job_ids.push(job_id.clone());
                if let Err(err) = wait_for_job(&mut job_client, &job_id).await {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "project.create job failed",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            }
            if let Some(pid) = resp
                .project_id
                .as_ref()
                .map(|id| id.value.clone())
                .filter(|value| !value.is_empty())
            {
                project_id = pid;
                outputs.push(metric("project_id", &project_id));
            }
        }
    }

    if wants_open {
        step_index += 1;
        let percent = step_index * 100 / total_steps;
        let metrics = vec![
            metric("pipeline_step", "project.open"),
            metric("step_index", step_index),
            metric("total_steps", total_steps),
            metric("run_id", &run_id),
            metric("correlation_id", &correlation_id),
            metric("project_path", &project_path),
        ];
        let _ = publish_progress(
            &mut job_client,
            &pipeline_job_id,
            percent,
            "project.open",
            metrics,
        )
        .await;

        if project_path.is_empty() {
            let msg = "project.open requires project_path";
            if !inferred && options.open_project {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "pipeline failed",
                    msg,
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
            let _ = publish_log(
                &mut job_client,
                &pipeline_job_id,
                &format!("WARN: {msg}, skipping\n"),
            )
            .await;
        } else {
            let mut project_client = match connect_project(&config.project_addr).await {
                Ok(client) => client,
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "project service unavailable",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            match project_client
                .open_project(OpenProjectRequest {
                    path: project_path.clone(),
                })
                .await
            {
                Ok(resp) => {
                    if let Some(project) = resp.into_inner().project {
                        if let Some(pid) = project
                            .project_id
                            .as_ref()
                            .map(|id| id.value.clone())
                            .filter(|value| !value.is_empty())
                        {
                            project_id = pid;
                            outputs.push(metric("project_id", &project_id));
                        }
                    }
                }
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "project.open failed",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            }
        }
    }

    if wants_verify {
        step_index += 1;
        let percent = step_index * 100 / total_steps;
        let metrics = vec![
            metric("pipeline_step", "toolchain.verify"),
            metric("step_index", step_index),
            metric("total_steps", total_steps),
            metric("run_id", &run_id),
            metric("correlation_id", &correlation_id),
            metric("toolchain_id", &toolchain_id),
        ];
        let _ = publish_progress(
            &mut job_client,
            &pipeline_job_id,
            percent,
            "toolchain.verify",
            metrics,
        )
        .await;

        if toolchain_id.is_empty() {
            let msg = "toolchain.verify requires toolchain_id";
            if !inferred && options.verify_toolchain {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "pipeline failed",
                    msg,
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
            let _ = publish_log(
                &mut job_client,
                &pipeline_job_id,
                &format!("WARN: {msg}, skipping\n"),
            )
            .await;
        } else {
            let mut toolchain_client = match connect_toolchain(&config.toolchain_addr).await {
                Ok(client) => client,
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "toolchain service unavailable",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            let resp = toolchain_client
                .verify_toolchain(aadk_proto::aadk::v1::VerifyToolchainRequest {
                    toolchain_id: Some(Id {
                        value: toolchain_id.clone(),
                    }),
                    job_id: None,
                    correlation_id: correlation_id.clone(),
                    run_id: Some(RunId {
                        value: run_id.clone(),
                    }),
                })
                .await;
            let resp = match resp {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "toolchain.verify failed",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            if let Some(job_id) = resp
                .job_id
                .as_ref()
                .map(|id| id.value.clone())
                .filter(|value| !value.is_empty())
            {
                job_ids.push(job_id.clone());
                let state = match wait_for_job(&mut job_client, &job_id).await {
                    Ok(state) => state,
                    Err(err) => {
                        fail_pipeline_and_record(
                            &config,
                            &mut job_client,
                            &pipeline_job_id,
                            &run_id,
                            &correlation_id,
                            &project_id,
                            &target_id,
                            &toolchain_set_id,
                            &job_ids,
                            started_at,
                            "toolchain.verify job failed",
                            &err.to_string(),
                        )
                        .await;
                        let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed)
                            .await;
                        return;
                    }
                };
                if !matches!(state, JobState::Success) || !resp.verified {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "toolchain.verify failed",
                        "verification failed",
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            }
        }
    }

    if wants_build {
        step_index += 1;
        let percent = step_index * 100 / total_steps;
        let project_ref = if !project_id.is_empty() {
            project_id.clone()
        } else {
            project_path.clone()
        };
        let metrics = vec![
            metric("pipeline_step", "build.run"),
            metric("step_index", step_index),
            metric("total_steps", total_steps),
            metric("run_id", &run_id),
            metric("correlation_id", &correlation_id),
            metric("project_ref", &project_ref),
        ];
        let _ = publish_progress(
            &mut job_client,
            &pipeline_job_id,
            percent,
            "build.run",
            metrics,
        )
        .await;

        if project_ref.trim().is_empty() {
            let msg = "build.run requires project_id or project_path";
            if !inferred && options.build {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "pipeline failed",
                    msg,
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
            let _ = publish_log(
                &mut job_client,
                &pipeline_job_id,
                &format!("WARN: {msg}, skipping\n"),
            )
            .await;
        } else {
            let mut build_client = match connect_build(&config.build_addr).await {
                Ok(client) => client,
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "build service unavailable",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            let variant = resolve_variant(req.build_variant);
            let resp = build_client
                .build(BuildRequest {
                    project_id: Some(Id {
                        value: project_ref.clone(),
                    }),
                    variant: variant as i32,
                    clean_first: false,
                    gradle_args: vec![],
                    job_id: None,
                    module: req.module.clone(),
                    variant_name: req.variant_name.clone(),
                    tasks: req.tasks.clone(),
                    correlation_id: correlation_id.clone(),
                    run_id: Some(RunId {
                        value: run_id.clone(),
                    }),
                })
                .await;
            let resp = match resp {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "build.run failed",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            let build_job_id = resp
                .job_id
                .as_ref()
                .map(|id| id.value.clone())
                .filter(|value| !value.is_empty())
                .unwrap_or_default();
            if build_job_id.is_empty() {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "build.run failed",
                    "empty job_id",
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
            job_ids.push(build_job_id.clone());
            let state = match wait_for_job(&mut job_client, &build_job_id).await {
                Ok(state) => state,
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "build.run job failed",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            if !matches!(state, JobState::Success) {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "build.run failed",
                    "build job failed",
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }

            let artifacts = build_client
                .list_artifacts(ListArtifactsRequest {
                    project_id: Some(Id {
                        value: project_ref.clone(),
                    }),
                    variant: variant as i32,
                    filter: Some(ArtifactFilter {
                        modules: if req.module.trim().is_empty() {
                            vec![]
                        } else {
                            vec![req.module.trim().to_string()]
                        },
                        variant: req.variant_name.trim().to_string(),
                        types: vec![],
                        name_contains: "".into(),
                        path_contains: "".into(),
                    }),
                })
                .await
                .map(|resp| resp.into_inner().artifacts)
                .unwrap_or_default();
            upsert_run_outputs_best_effort(
                &config.observe_addr,
                &run_id,
                &build_job_id,
                &artifacts,
            )
            .await;
            if let Some(path) = select_artifact_path(&artifacts) {
                if apk_path.is_empty() {
                    apk_path = path.clone();
                }
                outputs.push(metric("artifact_path", path));
            }
        }
    }

    if wants_install {
        step_index += 1;
        let percent = step_index * 100 / total_steps;
        let metrics = vec![
            metric("pipeline_step", "targets.install"),
            metric("step_index", step_index),
            metric("total_steps", total_steps),
            metric("run_id", &run_id),
            metric("correlation_id", &correlation_id),
            metric("target_id", &target_id),
            metric("apk_path", &apk_path),
        ];
        let _ = publish_progress(
            &mut job_client,
            &pipeline_job_id,
            percent,
            "targets.install",
            metrics,
        )
        .await;

        if target_id.is_empty() || apk_path.is_empty() {
            let msg = "targets.install requires target_id and apk_path";
            if !inferred && options.install_apk {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "pipeline failed",
                    msg,
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
            let _ = publish_log(
                &mut job_client,
                &pipeline_job_id,
                &format!("WARN: {msg}, skipping\n"),
            )
            .await;
        } else {
            let mut targets_client = match connect_targets(&config.targets_addr).await {
                Ok(client) => client,
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "targets service unavailable",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            let resp = targets_client
                .install_apk(InstallApkRequest {
                    target_id: Some(Id {
                        value: target_id.clone(),
                    }),
                    project_id: if project_id.is_empty() {
                        None
                    } else {
                        Some(Id {
                            value: project_id.clone(),
                        })
                    },
                    apk_path: apk_path.clone(),
                    job_id: None,
                    correlation_id: correlation_id.clone(),
                    run_id: Some(RunId {
                        value: run_id.clone(),
                    }),
                })
                .await;
            let resp = match resp {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "targets.install failed",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            let install_job_id = resp
                .job_id
                .as_ref()
                .map(|id| id.value.clone())
                .filter(|value| !value.is_empty())
                .unwrap_or_default();
            if install_job_id.is_empty() {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "targets.install failed",
                    "empty job_id",
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
            job_ids.push(install_job_id.clone());
            let state = match wait_for_job(&mut job_client, &install_job_id).await {
                Ok(state) => state,
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "targets.install job failed",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            if !matches!(state, JobState::Success) {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "targets.install failed",
                    "install job failed",
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
        }
    }

    if wants_launch {
        step_index += 1;
        let percent = step_index * 100 / total_steps;
        let metrics = vec![
            metric("pipeline_step", "targets.launch"),
            metric("step_index", step_index),
            metric("total_steps", total_steps),
            metric("run_id", &run_id),
            metric("correlation_id", &correlation_id),
            metric("target_id", &target_id),
            metric("application_id", &application_id),
        ];
        let _ = publish_progress(
            &mut job_client,
            &pipeline_job_id,
            percent,
            "targets.launch",
            metrics,
        )
        .await;

        if target_id.is_empty() || application_id.is_empty() {
            let msg = "targets.launch requires target_id and application_id";
            if !inferred && options.launch_app {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "pipeline failed",
                    msg,
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
            let _ = publish_log(
                &mut job_client,
                &pipeline_job_id,
                &format!("WARN: {msg}, skipping\n"),
            )
            .await;
        } else {
            let mut targets_client = match connect_targets(&config.targets_addr).await {
                Ok(client) => client,
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "targets service unavailable",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            let resp = targets_client
                .launch(LaunchRequest {
                    target_id: Some(Id {
                        value: target_id.clone(),
                    }),
                    application_id: application_id.clone(),
                    activity: activity.clone(),
                    job_id: None,
                    correlation_id: correlation_id.clone(),
                    run_id: Some(RunId {
                        value: run_id.clone(),
                    }),
                })
                .await;
            let resp = match resp {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "targets.launch failed",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            let launch_job_id = resp
                .job_id
                .as_ref()
                .map(|id| id.value.clone())
                .filter(|value| !value.is_empty())
                .unwrap_or_default();
            if launch_job_id.is_empty() {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "targets.launch failed",
                    "empty job_id",
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
            job_ids.push(launch_job_id.clone());
            let state = match wait_for_job(&mut job_client, &launch_job_id).await {
                Ok(state) => state,
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "targets.launch job failed",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            if !matches!(state, JobState::Success) {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "targets.launch failed",
                    "launch job failed",
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
        }
    }

    if wants_support {
        step_index += 1;
        let percent = step_index * 100 / total_steps;
        let metrics = vec![
            metric("pipeline_step", "observe.support_bundle"),
            metric("step_index", step_index),
            metric("total_steps", total_steps),
            metric("run_id", &run_id),
            metric("correlation_id", &correlation_id),
        ];
        let _ = publish_progress(
            &mut job_client,
            &pipeline_job_id,
            percent,
            "observe.support_bundle",
            metrics,
        )
        .await;

        let mut observe_client = match connect_observe(&config.observe_addr).await {
            Ok(client) => client,
            Err(err) => {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "observe service unavailable",
                    &err.to_string(),
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
        };
        let resp = observe_client
            .export_support_bundle(ExportSupportBundleRequest {
                include_logs: true,
                include_config: true,
                include_toolchain_provenance: true,
                include_recent_runs: false,
                recent_runs_limit: 10,
                job_id: None,
                project_id: if project_id.is_empty() {
                    None
                } else {
                    Some(Id {
                        value: project_id.clone(),
                    })
                },
                target_id: if target_id.is_empty() {
                    None
                } else {
                    Some(Id {
                        value: target_id.clone(),
                    })
                },
                toolchain_set_id: if toolchain_set_id.is_empty() {
                    None
                } else {
                    Some(Id {
                        value: toolchain_set_id.clone(),
                    })
                },
                correlation_id: correlation_id.clone(),
                run_id: Some(RunId {
                    value: run_id.clone(),
                }),
            })
            .await;
        let resp = match resp {
            Ok(resp) => resp.into_inner(),
            Err(err) => {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "observe.support_bundle failed",
                    &err.to_string(),
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
        };
        if let Some(job_id) = resp
            .job_id
            .as_ref()
            .map(|id| id.value.clone())
            .filter(|value| !value.is_empty())
        {
            job_ids.push(job_id.clone());
            let state = match wait_for_job(&mut job_client, &job_id).await {
                Ok(state) => state,
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "observe.support_bundle job failed",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            if !matches!(state, JobState::Success) {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "observe.support_bundle failed",
                    "support bundle failed",
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
        }
    }

    if wants_evidence {
        step_index += 1;
        let percent = step_index * 100 / total_steps;
        let metrics = vec![
            metric("pipeline_step", "observe.evidence_bundle"),
            metric("step_index", step_index),
            metric("total_steps", total_steps),
            metric("run_id", &run_id),
            metric("correlation_id", &correlation_id),
        ];
        let _ = publish_progress(
            &mut job_client,
            &pipeline_job_id,
            percent,
            "observe.evidence_bundle",
            metrics,
        )
        .await;

        let mut observe_client = match connect_observe(&config.observe_addr).await {
            Ok(client) => client,
            Err(err) => {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "observe service unavailable",
                    &err.to_string(),
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
        };
        let resp = observe_client
            .export_evidence_bundle(ExportEvidenceBundleRequest {
                run_id: Some(RunId {
                    value: run_id.clone(),
                }),
                job_id: None,
                correlation_id: correlation_id.clone(),
            })
            .await;
        let resp = match resp {
            Ok(resp) => resp.into_inner(),
            Err(err) => {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "observe.evidence_bundle failed",
                    &err.to_string(),
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
        };
        if let Some(job_id) = resp
            .job_id
            .as_ref()
            .map(|id| id.value.clone())
            .filter(|value| !value.is_empty())
        {
            job_ids.push(job_id.clone());
            let state = match wait_for_job(&mut job_client, &job_id).await {
                Ok(state) => state,
                Err(err) => {
                    fail_pipeline_and_record(
                        &config,
                        &mut job_client,
                        &pipeline_job_id,
                        &run_id,
                        &correlation_id,
                        &project_id,
                        &target_id,
                        &toolchain_set_id,
                        &job_ids,
                        started_at,
                        "observe.evidence_bundle job failed",
                        &err.to_string(),
                    )
                    .await;
                    let _ =
                        publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                    return;
                }
            };
            if !matches!(state, JobState::Success) {
                fail_pipeline_and_record(
                    &config,
                    &mut job_client,
                    &pipeline_job_id,
                    &run_id,
                    &correlation_id,
                    &project_id,
                    &target_id,
                    &toolchain_set_id,
                    &job_ids,
                    started_at,
                    "observe.evidence_bundle failed",
                    "evidence bundle failed",
                )
                .await;
                let _ = publish_state(&mut job_client, &pipeline_job_id, JobState::Failed).await;
                return;
            }
        }
    }

    summary.push(metric("pipeline", "complete"));
    if let Err(err) = upsert_run_best_effort(
        &config.observe_addr,
        &run_id,
        &correlation_id,
        &project_id,
        &target_id,
        &toolchain_set_id,
        &job_ids,
        "success",
        summary,
        Some(started_at),
        Some(now_millis()),
    )
    .await
    {
        let _ = publish_log(
            &mut job_client,
            &pipeline_job_id,
            &format!("WARN: failed to upsert run completion: {err}\n"),
        )
        .await;
    }

    let _ = publish_completed(
        &mut job_client,
        &pipeline_job_id,
        "Workflow pipeline completed",
        outputs,
    )
    .await;
}

#[tonic::async_trait]
impl WorkflowService for Svc {
    async fn run_pipeline(
        &self,
        request: Request<WorkflowPipelineRequest>,
    ) -> Result<Response<WorkflowPipelineResponse>, Status> {
        let req = request.into_inner();
        let run_id = req
            .run_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("run-{}", Uuid::new_v4()));
        let correlation_id = if req.correlation_id.trim().is_empty() {
            run_id.clone()
        } else {
            req.correlation_id.trim().to_string()
        };

        let mut job_client = connect_job(&self.config.job_addr).await?;
        let job_id = req
            .job_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let job_id = if job_id.is_empty() {
            let mut params = vec![
                metric("run_id", &run_id),
                metric("correlation_id", &correlation_id),
            ];
            if let Some(project_id) = req.project_id.as_ref().map(|id| id.value.clone()) {
                params.push(metric("project_id", project_id));
            }
            if let Some(target_id) = req.target_id.as_ref().map(|id| id.value.clone()) {
                params.push(metric("target_id", target_id));
            }
            if let Some(toolchain_set_id) = req.toolchain_set_id.as_ref().map(|id| id.value.clone())
            {
                params.push(metric("toolchain_set_id", toolchain_set_id));
            }
            let resp = job_client
                .start_job(StartJobRequest {
                    job_type: "workflow.pipeline".into(),
                    params,
                    project_id: req.project_id.clone(),
                    target_id: req.target_id.clone(),
                    toolchain_set_id: req.toolchain_set_id.clone(),
                    correlation_id: correlation_id.clone(),
                    run_id: Some(RunId {
                        value: run_id.clone(),
                    }),
                })
                .await?
                .into_inner();
            resp.job
                .and_then(|job| job.job_id)
                .map(|id| id.value)
                .unwrap_or_default()
        } else {
            job_id
        };

        if job_id.is_empty() {
            return Err(Status::internal("pipeline job_id is empty"));
        }

        let config = self.config.clone();
        let job_id_for_task = job_id.clone();
        let run_id_for_task = run_id.clone();
        let correlation_id_for_task = correlation_id.clone();
        let project_id_for_response = req.project_id.clone();
        tokio::spawn(async move {
            run_pipeline_task(
                config,
                job_id_for_task,
                run_id_for_task,
                correlation_id_for_task,
                req,
            )
            .await;
        });

        Ok(Response::new(WorkflowPipelineResponse {
            run_id: Some(RunId { value: run_id }),
            job_id: Some(Id { value: job_id }),
            project_id: project_id_for_response,
            outputs: vec![metric("correlation_id", correlation_id)],
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let svc = Svc {
        config: WorkflowConfig {
            job_addr: job_addr(),
            toolchain_addr: toolchain_addr(),
            project_addr: project_addr(),
            build_addr: build_addr(),
            targets_addr: targets_addr(),
            observe_addr: observe_addr(),
        },
    };

    serve_grpc_with_telemetry(
        "aadk-workflow",
        env!("CARGO_PKG_VERSION"),
        "workflow",
        "AADK_WORKFLOW_ADDR",
        aadk_util::DEFAULT_WORKFLOW_ADDR,
        |server| server.add_service(WorkflowServiceServer::new(svc)),
    )
    .await
}
