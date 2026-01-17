use std::{path::Path, sync::Arc};

use aadk_proto::aadk::v1::{
    job_service_client::JobServiceClient, target_service_server::TargetService, ErrorCode,
    GetCuttlefishStatusRequest, GetCuttlefishStatusResponse, GetDefaultTargetRequest,
    GetDefaultTargetResponse, Id, InstallApkRequest, InstallApkResponse, InstallCuttlefishRequest,
    InstallCuttlefishResponse, JobState, KeyValue, LaunchRequest, LaunchResponse,
    ListTargetsRequest, ListTargetsResponse, LogcatEvent, ReloadStateRequest, ReloadStateResponse,
    ResolveCuttlefishBuildRequest, ResolveCuttlefishBuildResponse, SetDefaultTargetRequest,
    SetDefaultTargetResponse, StartCuttlefishRequest, StartCuttlefishResponse, StopAppRequest,
    StopAppResponse, StopCuttlefishRequest, StopCuttlefishResponse, StreamLogcatRequest, Target,
};
use aadk_util::{now_millis, now_ts};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::{mpsc, Mutex},
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{transport::Channel, Request, Response, Status};
use tracing::warn;

use crate::adb::{
    adb_collect_props, adb_connect, adb_failure_message, adb_failure_status, adb_get_state,
    adb_output, adb_path, format_adb_output, AdbFailure,
};
use crate::cuttlefish::{
    cuttlefish_adb_serial, cuttlefish_status, host_page_size, maybe_cuttlefish_target,
    resolve_build_info, resolve_cuttlefish_request_config, run_cuttlefish_install_job,
    run_cuttlefish_start_job, run_cuttlefish_stop_job, CuttlefishInstallOptions,
    CuttlefishStatusError,
};
use crate::ids::{canonicalize_adb_serial, normalize_target_id, normalize_target_id_for_compare};
use crate::jobs::{
    cancel_requested, connect_job, job_error_detail, job_is_cancelled, metric, publish_completed,
    publish_failed, publish_log, publish_progress, publish_state, spawn_cancel_watcher, start_job,
};
use crate::state::{
    load_state, merge_inventory_targets, save_state, save_state_best_effort,
    upsert_inventory_entries, State,
};

#[derive(Clone)]
pub(crate) struct Svc {
    state: Arc<Mutex<State>>,
}

impl Default for Svc {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(load_state())),
        }
    }
}

enum TargetProvider {
    Adb,
    Cuttlefish,
}

impl TargetProvider {
    async fn list_targets(&self, include_offline: bool) -> Result<Vec<Target>, Status> {
        match self {
            TargetProvider::Adb => crate::adb::list_adb_targets(include_offline).await,
            TargetProvider::Cuttlefish => Ok(vec![]),
        }
    }

    async fn augment_targets(
        &self,
        targets: &mut Vec<Target>,
        include_offline: bool,
    ) -> Result<(), Status> {
        match self {
            TargetProvider::Adb => Ok(()),
            TargetProvider::Cuttlefish => {
                if let Some(cuttlefish) = maybe_cuttlefish_target(targets, include_offline).await? {
                    targets.push(cuttlefish);
                }
                Ok(())
            }
        }
    }
}

fn target_providers() -> Vec<TargetProvider> {
    vec![TargetProvider::Adb, TargetProvider::Cuttlefish]
}

async fn fetch_targets(include_offline: bool) -> Result<Vec<Target>, Status> {
    let providers = target_providers();
    let mut targets = Vec::new();

    for provider in &providers {
        let mut items = provider.list_targets(include_offline).await?;
        targets.append(&mut items);
    }

    for provider in &providers {
        provider
            .augment_targets(&mut targets, include_offline)
            .await?;
    }

    Ok(targets)
}

#[allow(clippy::result_large_err)]
fn require_id(id: Option<Id>, field: &str) -> Result<String, Status> {
    let value = id.map(|i| i.value).unwrap_or_default();
    if value.trim().is_empty() {
        Err(Status::invalid_argument(format!("{field} is required")))
    } else {
        Ok(value)
    }
}

async fn ensure_target_ready(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
    target_id: &str,
) -> Option<String> {
    let normalized = normalize_target_id(target_id);
    let canonical = canonicalize_adb_serial(&normalized);
    if canonical.contains(':') {
        let _ = adb_connect(&canonical).await;
    }

    match adb_get_state(&canonical).await {
        Ok(state) if state == "device" => {
            let props = adb_collect_props(&canonical).await;
            let mut metrics = vec![
                metric("target_id", &normalized),
                metric("adb_serial", &canonical),
                metric("state", &state),
                metric("health_state", "online"),
            ];
            if let Some(api_level) = props.api_level.as_ref() {
                metrics.push(metric("api_level", api_level));
            }
            if let Some(release) = props.release.as_ref() {
                metrics.push(metric("android_release", release));
            }
            if let Some(abi) = props.abi.as_ref() {
                metrics.push(metric("abi", abi));
            }
            let _ = publish_progress(client, job_id, 20, "target online", metrics).await;
            Some(canonical)
        }
        Ok(state) => {
            let detail = job_error_detail(
                ErrorCode::TargetNotReachable,
                "target is not online",
                format!("state={state}"),
                job_id,
            );
            let _ = publish_failed(client, job_id, detail).await;
            None
        }
        Err(err) => {
            let code = if matches!(err, AdbFailure::NotFound) {
                ErrorCode::AdbNotAvailable
            } else {
                ErrorCode::TargetNotReachable
            };
            let detail = job_error_detail(
                code,
                "failed to query target state",
                adb_failure_message(&err),
                job_id,
            );
            let _ = publish_failed(client, job_id, detail).await;
            None
        }
    }
}

async fn run_install_job(job_id: String, target_id: String, apk_path: String) {
    let mut job_client = match connect_job().await {
        Ok(client) => client,
        Err(err) => {
            warn!("install job {job_id}: failed to connect job service: {err}");
            return;
        }
    };

    let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;
    if job_is_cancelled(&mut job_client, &job_id).await {
        let _ = publish_log(&mut job_client, &job_id, "Install cancelled before start\n").await;
        return;
    }

    let _ = publish_state(&mut job_client, &job_id, JobState::Running).await;
    let _ = publish_log(
        &mut job_client,
        &job_id,
        &format!("Installing {apk_path} on {target_id}\n"),
    )
    .await;
    let _ = publish_progress(
        &mut job_client,
        &job_id,
        10,
        "checking target",
        vec![
            metric("target_id", &target_id),
            metric("apk_path", &apk_path),
        ],
    )
    .await;

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Install cancelled\n").await;
        return;
    }

    let target_id = match ensure_target_ready(&mut job_client, &job_id, &target_id).await {
        Some(serial) => serial,
        None => return,
    };

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Install cancelled\n").await;
        return;
    }

    let _ = publish_progress(
        &mut job_client,
        &job_id,
        55,
        "adb install",
        vec![
            metric("adb_serial", &target_id),
            metric("apk_path", &apk_path),
            metric("replace", true),
        ],
    )
    .await;

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Install cancelled\n").await;
        return;
    }

    let args = ["-s", target_id.as_str(), "install", "-r", apk_path.as_str()];
    match adb_output(&args).await {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let log = format_adb_output(&stdout, &stderr);
            if !log.is_empty() {
                let _ = publish_log(&mut job_client, &job_id, &log).await;
            }
            let _ = publish_completed(
                &mut job_client,
                &job_id,
                "APK installed",
                vec![KeyValue {
                    key: "apk_path".into(),
                    value: apk_path,
                }],
            )
            .await;
        }
        Err(err) => {
            let code = if matches!(err, AdbFailure::NotFound) {
                ErrorCode::AdbNotAvailable
            } else {
                ErrorCode::InstallFailed
            };
            let detail = job_error_detail(
                code,
                "adb install failed",
                adb_failure_message(&err),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
        }
    }
}

async fn run_launch_job(
    job_id: String,
    target_id: String,
    application_id: String,
    activity: String,
) {
    let mut job_client = match connect_job().await {
        Ok(client) => client,
        Err(err) => {
            warn!("launch job {job_id}: failed to connect job service: {err}");
            return;
        }
    };

    let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;
    if job_is_cancelled(&mut job_client, &job_id).await {
        let _ = publish_log(&mut job_client, &job_id, "Launch cancelled before start\n").await;
        return;
    }

    let _ = publish_state(&mut job_client, &job_id, JobState::Running).await;
    let _ = publish_log(
        &mut job_client,
        &job_id,
        &format!("Launching {application_id} on {target_id}\n"),
    )
    .await;
    let _ = publish_progress(
        &mut job_client,
        &job_id,
        10,
        "checking target",
        vec![
            metric("target_id", &target_id),
            metric("application_id", &application_id),
            metric("activity", &activity),
        ],
    )
    .await;

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Launch cancelled\n").await;
        return;
    }

    let target_id = match ensure_target_ready(&mut job_client, &job_id, &target_id).await {
        Some(serial) => serial,
        None => return,
    };

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Launch cancelled\n").await;
        return;
    }

    let _ = publish_progress(
        &mut job_client,
        &job_id,
        60,
        "adb launch",
        vec![
            metric("adb_serial", &target_id),
            metric("application_id", &application_id),
            metric("activity", &activity),
        ],
    )
    .await;

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Launch cancelled\n").await;
        return;
    }

    let output = if activity.trim().is_empty() {
        let args = [
            "-s",
            target_id.as_str(),
            "shell",
            "monkey",
            "-p",
            application_id.as_str(),
            "-c",
            "android.intent.category.LAUNCHER",
            "1",
        ];
        adb_output(&args).await
    } else {
        let component = if activity.contains('/') {
            activity.clone()
        } else {
            format!("{}/{}", application_id, activity)
        };
        let args = [
            "-s",
            target_id.as_str(),
            "shell",
            "am",
            "start",
            "-n",
            component.as_str(),
        ];
        adb_output(&args).await
    };

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let log = format_adb_output(&stdout, &stderr);
            if !log.is_empty() {
                let _ = publish_log(&mut job_client, &job_id, &log).await;
            }
            let _ = publish_completed(
                &mut job_client,
                &job_id,
                "App launched",
                vec![KeyValue {
                    key: "application_id".into(),
                    value: application_id,
                }],
            )
            .await;
        }
        Err(err) => {
            let code = if matches!(err, AdbFailure::NotFound) {
                ErrorCode::AdbNotAvailable
            } else {
                ErrorCode::LaunchFailed
            };
            let detail = job_error_detail(
                code,
                "adb launch failed",
                adb_failure_message(&err),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
        }
    }
}

async fn run_stop_job(job_id: String, target_id: String, application_id: String) {
    let mut job_client = match connect_job().await {
        Ok(client) => client,
        Err(err) => {
            warn!("stop job {job_id}: failed to connect job service: {err}");
            return;
        }
    };

    let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;
    if job_is_cancelled(&mut job_client, &job_id).await {
        let _ = publish_log(&mut job_client, &job_id, "Stop cancelled before start\n").await;
        return;
    }

    let _ = publish_state(&mut job_client, &job_id, JobState::Running).await;
    let _ = publish_log(
        &mut job_client,
        &job_id,
        &format!("Stopping {application_id} on {target_id}\n"),
    )
    .await;
    let _ = publish_progress(
        &mut job_client,
        &job_id,
        10,
        "checking target",
        vec![
            metric("target_id", &target_id),
            metric("application_id", &application_id),
        ],
    )
    .await;

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Stop cancelled\n").await;
        return;
    }

    let target_id = match ensure_target_ready(&mut job_client, &job_id, &target_id).await {
        Some(serial) => serial,
        None => return,
    };

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Stop cancelled\n").await;
        return;
    }

    let _ = publish_progress(
        &mut job_client,
        &job_id,
        60,
        "adb stop",
        vec![
            metric("adb_serial", &target_id),
            metric("application_id", &application_id),
        ],
    )
    .await;

    let args = [
        "-s",
        target_id.as_str(),
        "shell",
        "am",
        "force-stop",
        application_id.as_str(),
    ];

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Stop cancelled\n").await;
        return;
    }

    match adb_output(&args).await {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let log = format_adb_output(&stdout, &stderr);
            if !log.is_empty() {
                let _ = publish_log(&mut job_client, &job_id, &log).await;
            }
            let _ = publish_completed(
                &mut job_client,
                &job_id,
                "App stopped",
                vec![KeyValue {
                    key: "application_id".into(),
                    value: application_id,
                }],
            )
            .await;
        }
        Err(err) => {
            let code = if matches!(err, AdbFailure::NotFound) {
                ErrorCode::AdbNotAvailable
            } else {
                ErrorCode::Internal
            };
            let detail =
                job_error_detail(code, "adb stop failed", adb_failure_message(&err), &job_id);
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
        }
    }
}

async fn emit_logcat(
    target_id: &str,
    adb_serial: &str,
    filter: &str,
    dump: bool,
    tx: &mpsc::Sender<Result<LogcatEvent, Status>>,
) -> Result<(), Status> {
    let mut cmd = Command::new(adb_path());
    cmd.arg("-s").arg(adb_serial).arg("logcat");
    if dump {
        cmd.arg("-d");
    }
    if !filter.trim().is_empty() {
        cmd.arg(filter);
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| Status::internal(format!("failed to spawn adb logcat: {e}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Status::internal("failed to capture adb logcat stdout"))?;
    let mut reader = BufReader::new(stdout).lines();

    while let Ok(Some(line)) = reader.next_line().await {
        let event = LogcatEvent {
            at: Some(now_ts()),
            target_id: Some(Id {
                value: target_id.to_string(),
            }),
            line: line.into_bytes(),
        };
        if tx.send(Ok(event)).await.is_err() {
            let _ = child.kill().await;
            return Ok(());
        }
    }

    let _ = child.wait().await;
    Ok(())
}

async fn stream_logcat_impl(
    target_id: String,
    adb_serial: String,
    filter: String,
    include_history: bool,
    tx: mpsc::Sender<Result<LogcatEvent, Status>>,
) {
    if include_history {
        if let Err(err) = emit_logcat(&target_id, &adb_serial, &filter, true, &tx).await {
            let _ = tx.send(Err(err)).await;
            return;
        }
    }

    if let Err(err) = emit_logcat(&target_id, &adb_serial, &filter, false, &tx).await {
        let _ = tx.send(Err(err)).await;
    }
}

#[tonic::async_trait]
impl TargetService for Svc {
    async fn list_targets(
        &self,
        request: Request<ListTargetsRequest>,
    ) -> Result<Response<ListTargetsResponse>, Status> {
        let req = request.into_inner();
        let mut targets = fetch_targets(req.include_offline).await?;
        {
            let mut st = self.state.lock().await;
            upsert_inventory_entries(&mut st.inventory, &targets, now_millis());
            save_state_best_effort(&st);
            merge_inventory_targets(&mut targets, &st.inventory, req.include_offline);
        }
        Ok(Response::new(ListTargetsResponse { targets }))
    }

    async fn set_default_target(
        &self,
        request: Request<SetDefaultTargetRequest>,
    ) -> Result<Response<SetDefaultTargetResponse>, Status> {
        let req = request.into_inner();
        let target_id = require_id(req.target_id, "target_id")?;
        let target_id = normalize_target_id(&target_id);
        if target_id.is_empty() {
            return Err(Status::invalid_argument("target_id is invalid"));
        }
        let targets = fetch_targets(true).await?;
        let target_key = normalize_target_id_for_compare(&target_id);
        let mut chosen = targets.into_iter().find(|t| {
            t.target_id
                .as_ref()
                .map(|i| normalize_target_id_for_compare(&i.value) == target_key)
                .unwrap_or(false)
        });

        let mut st = self.state.lock().await;
        if chosen.is_none() {
            chosen = st
                .inventory
                .iter()
                .find(|entry| normalize_target_id_for_compare(entry.target_id()) == target_key)
                .map(|entry| entry.to_target(Some("offline")));
        }
        let Some(chosen) = chosen else {
            return Ok(Response::new(SetDefaultTargetResponse { ok: false }));
        };
        st.default_target = Some(chosen);
        if let Some(target) = st.default_target.clone() {
            upsert_inventory_entries(
                &mut st.inventory,
                std::slice::from_ref(&target),
                now_millis(),
            );
        }
        if let Err(err) = save_state(&st) {
            return Err(Status::internal(format!(
                "failed to persist default target: {err}"
            )));
        }
        Ok(Response::new(SetDefaultTargetResponse { ok: true }))
    }

    async fn get_default_target(
        &self,
        _request: Request<GetDefaultTargetRequest>,
    ) -> Result<Response<GetDefaultTargetResponse>, Status> {
        let stored = { self.state.lock().await.default_target.clone() };
        let Some(stored) = stored else {
            return Ok(Response::new(GetDefaultTargetResponse { target: None }));
        };

        let stored_id = stored
            .target_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty());

        if let Some(target_id) = stored_id {
            let target_key = normalize_target_id_for_compare(&target_id);
            if let Ok(targets) = fetch_targets(true).await {
                if let Some(found) = targets.into_iter().find(|target| {
                    target
                        .target_id
                        .as_ref()
                        .map(|id| normalize_target_id_for_compare(&id.value) == target_key)
                        .unwrap_or(false)
                }) {
                    let mut st = self.state.lock().await;
                    st.default_target = Some(found.clone());
                    upsert_inventory_entries(
                        &mut st.inventory,
                        std::slice::from_ref(&found),
                        now_millis(),
                    );
                    save_state_best_effort(&st);
                    return Ok(Response::new(GetDefaultTargetResponse {
                        target: Some(found),
                    }));
                }
            }
            let inventory_target = {
                let st = self.state.lock().await;
                st.inventory
                    .iter()
                    .find(|entry| normalize_target_id_for_compare(entry.target_id()) == target_key)
                    .map(|entry| entry.to_target(Some("offline")))
            };
            if let Some(found) = inventory_target {
                let mut st = self.state.lock().await;
                st.default_target = Some(found.clone());
                save_state_best_effort(&st);
                return Ok(Response::new(GetDefaultTargetResponse {
                    target: Some(found),
                }));
            }
        }

        Ok(Response::new(GetDefaultTargetResponse {
            target: Some(stored),
        }))
    }

    async fn install_apk(
        &self,
        request: Request<InstallApkRequest>,
    ) -> Result<Response<InstallApkResponse>, Status> {
        let req = request.into_inner();
        let target_id = require_id(req.target_id.clone(), "target_id")?;
        let target_id = normalize_target_id(&target_id);
        if target_id.is_empty() {
            return Err(Status::invalid_argument("target_id is invalid"));
        }
        let apk_path = req.apk_path;

        if apk_path.trim().is_empty() {
            return Err(Status::invalid_argument("apk_path is required"));
        }
        if !Path::new(&apk_path).exists() {
            return Err(Status::not_found(format!("apk not found: {apk_path}")));
        }

        let mut job_client = connect_job().await?;
        let job_id = req
            .job_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let correlation_id = req.correlation_id.trim();
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "targets.install",
                vec![KeyValue {
                    key: "apk_path".into(),
                    value: apk_path.clone(),
                }],
                req.project_id,
                Some(Id {
                    value: target_id.clone(),
                }),
                correlation_id,
                req.run_id.clone(),
            )
            .await?
        } else {
            job_id
        };

        tokio::spawn(run_install_job(job_id.clone(), target_id, apk_path));
        Ok(Response::new(InstallApkResponse {
            job_id: Some(Id { value: job_id }),
        }))
    }

    async fn launch(
        &self,
        request: Request<LaunchRequest>,
    ) -> Result<Response<LaunchResponse>, Status> {
        let req = request.into_inner();
        let target_id = require_id(req.target_id.clone(), "target_id")?;
        let target_id = normalize_target_id(&target_id);
        if target_id.is_empty() {
            return Err(Status::invalid_argument("target_id is invalid"));
        }
        let application_id = req.application_id.trim().to_string();
        if application_id.is_empty() {
            return Err(Status::invalid_argument("application_id is required"));
        }

        let mut job_client = connect_job().await?;
        let job_id = req
            .job_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let correlation_id = req.correlation_id.trim();
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "targets.launch",
                vec![KeyValue {
                    key: "application_id".into(),
                    value: application_id.clone(),
                }],
                None,
                Some(Id {
                    value: target_id.clone(),
                }),
                correlation_id,
                req.run_id.clone(),
            )
            .await?
        } else {
            job_id
        };

        tokio::spawn(run_launch_job(
            job_id.clone(),
            target_id,
            application_id,
            req.activity,
        ));
        Ok(Response::new(LaunchResponse {
            job_id: Some(Id { value: job_id }),
        }))
    }

    async fn stop_app(
        &self,
        request: Request<StopAppRequest>,
    ) -> Result<Response<StopAppResponse>, Status> {
        let req = request.into_inner();
        let target_id = require_id(req.target_id.clone(), "target_id")?;
        let target_id = normalize_target_id(&target_id);
        if target_id.is_empty() {
            return Err(Status::invalid_argument("target_id is invalid"));
        }
        let application_id = req.application_id.trim().to_string();
        if application_id.is_empty() {
            return Err(Status::invalid_argument("application_id is required"));
        }

        let mut job_client = connect_job().await?;
        let job_id = req
            .job_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let correlation_id = req.correlation_id.trim();
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "targets.stop",
                vec![KeyValue {
                    key: "application_id".into(),
                    value: application_id.clone(),
                }],
                None,
                Some(Id {
                    value: target_id.clone(),
                }),
                correlation_id,
                req.run_id.clone(),
            )
            .await?
        } else {
            job_id
        };

        tokio::spawn(run_stop_job(job_id.clone(), target_id, application_id));
        Ok(Response::new(StopAppResponse {
            job_id: Some(Id { value: job_id }),
        }))
    }

    async fn install_cuttlefish(
        &self,
        request: Request<InstallCuttlefishRequest>,
    ) -> Result<Response<InstallCuttlefishResponse>, Status> {
        let req = request.into_inner();
        let force = req.force;
        let branch = req.branch.trim().to_string();
        let target = req.target.trim().to_string();
        let build_id = req.build_id.trim().to_string();

        let branch_override = if branch.is_empty() {
            None
        } else {
            Some(branch)
        };
        let target_override = if target.is_empty() {
            None
        } else {
            Some(target)
        };
        let build_id_override = if build_id.is_empty() {
            None
        } else {
            Some(build_id)
        };

        let mut job_client = connect_job().await?;
        let mut params = vec![KeyValue {
            key: "force".into(),
            value: force.to_string(),
        }];
        if let Some(ref branch) = branch_override {
            params.push(KeyValue {
                key: "branch".into(),
                value: branch.clone(),
            });
        }
        if let Some(ref target) = target_override {
            params.push(KeyValue {
                key: "target".into(),
                value: target.clone(),
            });
        }
        if let Some(ref build_id) = build_id_override {
            params.push(KeyValue {
                key: "build_id".into(),
                value: build_id.clone(),
            });
        }

        let job_id = req
            .job_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let correlation_id = req.correlation_id.trim();
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "targets.cuttlefish.install",
                params,
                None,
                None,
                correlation_id,
                req.run_id.clone(),
            )
            .await?
        } else {
            job_id
        };

        tokio::spawn(run_cuttlefish_install_job(
            job_id.clone(),
            CuttlefishInstallOptions {
                force,
                branch: branch_override,
                target: target_override,
                build_id: build_id_override,
            },
        ));

        Ok(Response::new(InstallCuttlefishResponse {
            job_id: Some(Id { value: job_id }),
        }))
    }

    async fn resolve_cuttlefish_build(
        &self,
        request: Request<ResolveCuttlefishBuildRequest>,
    ) -> Result<Response<ResolveCuttlefishBuildResponse>, Status> {
        let req = request.into_inner();
        let page_size = host_page_size();
        let config = resolve_cuttlefish_request_config(
            page_size,
            Some(req.branch),
            Some(req.target),
            Some(req.build_id),
        );

        if config.branch.trim().is_empty() || config.target.trim().is_empty() {
            return Err(Status::invalid_argument("branch and target are required"));
        }

        let info = resolve_build_info(
            &config.branch,
            &config.target,
            config.build_id_override.clone(),
        )
        .await
        .map_err(|err| Status::internal(format!("resolve build failed: {err}")))?;

        Ok(Response::new(ResolveCuttlefishBuildResponse {
            branch: config.branch,
            target: config.target,
            build_id: info.build_id,
            product: info.product,
        }))
    }

    async fn start_cuttlefish(
        &self,
        request: Request<StartCuttlefishRequest>,
    ) -> Result<Response<StartCuttlefishResponse>, Status> {
        let req = request.into_inner();
        let show_full_ui = req.show_full_ui;

        let mut job_client = connect_job().await?;
        let job_id = req
            .job_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let correlation_id = req.correlation_id.trim();
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "targets.cuttlefish.start",
                vec![KeyValue {
                    key: "show_full_ui".into(),
                    value: show_full_ui.to_string(),
                }],
                None,
                None,
                correlation_id,
                req.run_id.clone(),
            )
            .await?
        } else {
            job_id
        };

        tokio::spawn(run_cuttlefish_start_job(job_id.clone(), show_full_ui));

        Ok(Response::new(StartCuttlefishResponse {
            job_id: Some(Id { value: job_id }),
        }))
    }

    async fn stop_cuttlefish(
        &self,
        _request: Request<StopCuttlefishRequest>,
    ) -> Result<Response<StopCuttlefishResponse>, Status> {
        let req = _request.into_inner();
        let mut job_client = connect_job().await?;
        let job_id = req
            .job_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let correlation_id = req.correlation_id.trim();
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "targets.cuttlefish.stop",
                vec![],
                None,
                None,
                correlation_id,
                req.run_id.clone(),
            )
            .await?
        } else {
            job_id
        };

        tokio::spawn(run_cuttlefish_stop_job(job_id.clone()));

        Ok(Response::new(StopCuttlefishResponse {
            job_id: Some(Id { value: job_id }),
        }))
    }

    async fn get_cuttlefish_status(
        &self,
        _request: Request<GetCuttlefishStatusRequest>,
    ) -> Result<Response<GetCuttlefishStatusResponse>, Status> {
        let mut details = Vec::new();
        let mut adb_serial = cuttlefish_adb_serial();

        let state = match cuttlefish_status().await {
            Ok(status) => {
                if !status.adb_serial.is_empty() {
                    adb_serial = status.adb_serial;
                }
                for (key, value) in status.details {
                    details.push(KeyValue {
                        key: format!("cuttlefish_{key}"),
                        value,
                    });
                }
                if !status.raw.is_empty() {
                    details.push(KeyValue {
                        key: "cuttlefish_status_raw".into(),
                        value: status.raw,
                    });
                }
                if status.running {
                    "running".to_string()
                } else {
                    "stopped".to_string()
                }
            }
            Err(CuttlefishStatusError::NotInstalled) => "not_installed".to_string(),
            Err(CuttlefishStatusError::Failed(err)) => {
                details.push(KeyValue {
                    key: "cuttlefish_status_error".into(),
                    value: err,
                });
                "error".to_string()
            }
        };

        let adb_serial = normalize_target_id(&adb_serial);
        let adb_serial_canonical = canonicalize_adb_serial(&adb_serial);
        if !adb_serial.is_empty() {
            match adb_get_state(&adb_serial_canonical).await {
                Ok(adb_state) => details.push(KeyValue {
                    key: "adb_state".into(),
                    value: adb_state,
                }),
                Err(err) => details.push(KeyValue {
                    key: "adb_state_error".into(),
                    value: adb_failure_message(&err),
                }),
            }
        }

        Ok(Response::new(GetCuttlefishStatusResponse {
            state,
            adb_serial,
            details,
        }))
    }

    type StreamLogcatStream = ReceiverStream<Result<LogcatEvent, Status>>;

    async fn stream_logcat(
        &self,
        request: Request<StreamLogcatRequest>,
    ) -> Result<Response<Self::StreamLogcatStream>, Status> {
        let req = request.into_inner();
        let target_id = require_id(req.target_id, "target_id")?;
        let target_id = normalize_target_id(&target_id);
        if target_id.is_empty() {
            return Err(Status::invalid_argument("target_id is invalid"));
        }
        let adb_serial = canonicalize_adb_serial(&target_id);

        match adb_get_state(&adb_serial).await {
            Ok(state) if state == "device" => {}
            Ok(state) => {
                return Err(Status::failed_precondition(format!(
                    "target not ready (state={state})"
                )))
            }
            Err(err) => return Err(adb_failure_status(err)),
        }

        let (tx, rx) = mpsc::channel::<Result<LogcatEvent, Status>>(256);
        tokio::spawn(stream_logcat_impl(
            target_id,
            adb_serial,
            req.filter,
            req.include_history,
            tx.clone(),
        ));

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn reload_state(
        &self,
        _request: Request<ReloadStateRequest>,
    ) -> Result<Response<ReloadStateResponse>, Status> {
        let state = load_state();
        let count = state.inventory.len() as u32;
        let mut st = self.state.lock().await;
        *st = state;
        Ok(Response::new(ReloadStateResponse {
            ok: true,
            item_count: count,
            detail: "target state reloaded".into(),
        }))
    }
}
