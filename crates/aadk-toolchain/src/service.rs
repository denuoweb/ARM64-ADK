use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use aadk_util::now_ts;
use aadk_proto::aadk::v1::{
    toolchain_service_server::ToolchainService, CleanupToolchainCacheRequest,
    CleanupToolchainCacheResponse, CreateToolchainSetRequest, CreateToolchainSetResponse,
    ErrorCode, ErrorDetail, GetActiveToolchainSetRequest, GetActiveToolchainSetResponse, Id,
    InstallToolchainRequest, InstallToolchainResponse, InstalledToolchain, JobState, KeyValue,
    ListAvailableRequest, ListAvailableResponse, ListInstalledRequest, ListInstalledResponse,
    ListProvidersRequest, ListProvidersResponse, ListToolchainSetsRequest,
    ListToolchainSetsResponse, PageInfo, SetActiveToolchainSetRequest,
    SetActiveToolchainSetResponse, ToolchainArtifact, ToolchainKind, ToolchainSet,
    UninstallToolchainRequest, UninstallToolchainResponse, UpdateToolchainRequest,
    UpdateToolchainResponse, VerifyToolchainRequest, VerifyToolchainResponse,
};
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use tracing::warn;
use uuid::Uuid;

use crate::artifacts::{
    cached_path_for_entry, cached_paths_for_installed, download_dir, ensure_artifact_local,
    extract_archive, finalize_install, is_remote_url,
};
use crate::cancel::cancel_requested;
use crate::catalog::{
    available_for_provider, catalog_artifact_for_provenance, catalog_version_hosts, find_available,
    fixtures_dir, host_key, load_catalog, provider_from_catalog, Catalog,
};
use crate::jobs::{
    connect_job, job_error_detail, job_is_cancelled, metric, publish_completed, publish_failed,
    publish_log, publish_progress, publish_state, spawn_cancel_watcher, start_job,
    toolchain_base_metrics,
};
use crate::provenance::{read_provenance, write_provenance, Provenance};
use crate::state::{
    default_install_root, expand_user, install_root_for_entry, installed_toolchain_matches,
    load_state, normalize_id, provider_id_from_installed, replace_toolchain_in_sets,
    save_state_best_effort, scrub_toolchain_from_sets, toolchain_referenced_by_sets,
    toolchain_set_id, version_from_installed, State,
};
use crate::verify::{
    normalize_signature_value, signature_record_from_catalog, signature_record_from_provenance,
    transparency_record_from_catalog, transparency_record_from_provenance,
    validate_toolchain_layout, verify_provenance_entry, verify_signature_if_configured,
    verify_transparency_log,
};

const DEFAULT_PAGE_SIZE: usize = 25;

#[derive(Clone)]
pub(crate) struct Svc {
    state: Arc<Mutex<State>>,
    catalog: Arc<Catalog>,
}

impl Default for Svc {
    fn default() -> Self {
        let state = load_state();
        Self {
            state: Arc::new(Mutex::new(state)),
            catalog: Arc::new(load_catalog()),
        }
    }
}

impl Svc {
    #[allow(clippy::too_many_arguments)]
    async fn run_install_job(
        &self,
        job_id: String,
        provider_id: String,
        version: String,
        install_root: String,
        verify_hash: bool,
        host: String,
        fixtures: Option<PathBuf>,
    ) -> Result<(), Status> {
        let mut job_client = connect_job().await?;
        let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;

        if job_is_cancelled(&mut job_client, &job_id).await {
            let _ = publish_log(&mut job_client, &job_id, "Install cancelled before start\n").await;
            return Ok(());
        }

        if let Err(err) = publish_state(&mut job_client, &job_id, JobState::Running).await {
            warn!("Failed to publish job state: {}", err);
        }
        if let Err(err) =
            publish_log(&mut job_client, &job_id, "Starting toolchain install\n").await
        {
            warn!("Failed to publish job log: {}", err);
        }

        if cancel_requested(Some(&cancel_rx)) {
            let _ = publish_log(&mut job_client, &job_id, "Install cancelled\n").await;
            return Ok(());
        }

        let available = match find_available(
            &self.catalog,
            &provider_id,
            &version,
            &host,
            fixtures.as_deref(),
        ) {
            Some(item) => item,
            None => {
                if fixtures.is_none() {
                    if let Some(hosts) =
                        catalog_version_hosts(&self.catalog, &provider_id, &version)
                    {
                        let detail = if hosts.is_empty() {
                            format!("host={host} (no artifacts listed)")
                        } else {
                            format!("host={host} available_hosts={}", hosts.join(","))
                        };
                        let err_detail = job_error_detail(
                            ErrorCode::ToolchainIncompatibleHost,
                            "host not supported for requested toolchain",
                            detail,
                            &job_id,
                        );
                        let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                        return Ok(());
                    }
                }
                let err_detail = job_error_detail(
                    ErrorCode::ToolchainInstallFailed,
                    "requested toolchain version not found",
                    format!("provider_id={provider_id} version={version}"),
                    &job_id,
                );
                let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                return Ok(());
            }
        };

        if cancel_requested(Some(&cancel_rx)) {
            let _ = publish_log(&mut job_client, &job_id, "Install cancelled\n").await;
            return Ok(());
        }

        let provider = match available.provider.clone() {
            Some(p) => p,
            None => {
                let err_detail = job_error_detail(
                    ErrorCode::ToolchainInstallFailed,
                    "unknown provider_id",
                    provider_id.clone(),
                    &job_id,
                );
                let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                return Ok(());
            }
        };

        if cancel_requested(Some(&cancel_rx)) {
            let _ = publish_log(&mut job_client, &job_id, "Install cancelled\n").await;
            return Ok(());
        }

        let artifact = available.artifact.clone().unwrap_or(ToolchainArtifact {
            url: "".into(),
            sha256: "".into(),
            size_bytes: 0,
        });

        if artifact.url.is_empty() {
            let err_detail = job_error_detail(
                ErrorCode::ToolchainInstallFailed,
                "artifact url missing",
                "".into(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let catalog_artifact = catalog_artifact_for_provenance(
            &self.catalog,
            &provider_id,
            &version,
            &artifact.url,
            &artifact.sha256,
        );
        let signature_record = catalog_artifact
            .as_ref()
            .and_then(signature_record_from_catalog);
        let transparency_record = catalog_artifact
            .as_ref()
            .and_then(transparency_record_from_catalog);

        if !verify_hash && !artifact.sha256.is_empty() {
            warn!("Skipping hash verification for provider {}", provider.name);
        }
        if !verify_hash && signature_record.is_some() {
            warn!(
                "Skipping signature verification for provider {}",
                provider.name
            );
        }

        let install_root = if install_root.trim().is_empty() {
            default_install_root()
        } else {
            expand_user(&install_root)
        };
        let provider_kind =
            ToolchainKind::try_from(provider.kind).unwrap_or(ToolchainKind::Unspecified);

        let _ = publish_progress(&mut job_client, &job_id, 10, "resolve artifact", {
            let mut metrics = toolchain_base_metrics(
                &provider_id,
                &provider.name,
                &version,
                verify_hash,
                &host,
                Some(&artifact),
            );
            metrics.push(metric("provider_kind", format!("{provider_kind:?}")));
            metrics.push(metric("install_root", install_root.display()));
            metrics
        })
        .await;
        let _ = publish_log(
            &mut job_client,
            &job_id,
            &format!("Artifact URL: {}\n", artifact.url),
        )
        .await;

        let archive_path =
            match ensure_artifact_local(&artifact, verify_hash, Some(&cancel_rx)).await {
                Ok(path) => path,
                Err(err) if err.code() == tonic::Code::Cancelled => {
                    let _ = publish_log(
                        &mut job_client,
                        &job_id,
                        "Install cancelled during download\n",
                    )
                    .await;
                    return Ok(());
                }
                Err(err) => {
                    let err_detail = job_error_detail(
                        ErrorCode::ToolchainInstallFailed,
                        "failed to fetch or verify artifact",
                        err.message().to_string(),
                        &job_id,
                    );
                    let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                    return Ok(());
                }
            };

        if cancel_requested(Some(&cancel_rx)) {
            let _ = publish_log(&mut job_client, &job_id, "Install cancelled\n").await;
            return Ok(());
        }

        let mut signature_value = String::new();
        let mut signature_url = String::new();
        let mut signature_public_key = String::new();
        let mut transparency_entry = String::new();
        let mut transparency_entry_url = String::new();
        let mut transparency_public_key = String::new();
        if let Some(record) = signature_record.as_ref() {
            signature_url = record.signature_url.clone();
            signature_public_key = record.public_key.clone();
            if verify_hash {
                match verify_signature_if_configured(&archive_path, Some(record)).await {
                    Ok(Some(sig)) => signature_value = sig,
                    Ok(None) => {}
                    Err(err) => {
                        let err_detail = job_error_detail(
                            ErrorCode::ToolchainInstallFailed,
                            "signature verification failed",
                            err,
                            &job_id,
                        );
                        let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                        return Ok(());
                    }
                }
            } else if !record.signature.trim().is_empty() {
                signature_value = normalize_signature_value(&record.signature);
            }
        }
        if let Some(record) = transparency_record.as_ref() {
            transparency_entry_url = record.entry_url.clone();
            transparency_public_key = record.public_key.clone();
            match verify_transparency_log(record, &artifact.sha256).await {
                Ok((_entry, raw)) => {
                    transparency_entry = raw;
                }
                Err(err) => {
                    let err_detail = job_error_detail(
                        ErrorCode::ToolchainInstallFailed,
                        "transparency log validation failed",
                        err,
                        &job_id,
                    );
                    let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                    return Ok(());
                }
            }
        }

        let _ = publish_progress(&mut job_client, &job_id, 45, "downloaded", {
            let mut metrics = toolchain_base_metrics(
                &provider_id,
                &provider.name,
                &version,
                verify_hash,
                &host,
                Some(&artifact),
            );
            metrics.push(metric(
                "archive_path",
                archive_path.to_string_lossy().to_string(),
            ));
            metrics.push(metric("install_root", install_root.display()));
            metrics
        })
        .await;

        let provider_dir = install_root.join(&provider.name);
        if let Err(err) = fs::create_dir_all(&provider_dir) {
            let err_detail = job_error_detail(
                ErrorCode::ToolchainInstallFailed,
                "failed to create install root",
                err.to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let final_dir = provider_dir.join(&version);
        if final_dir.exists() {
            let err_detail = job_error_detail(
                ErrorCode::ToolchainInstallFailed,
                "toolchain already installed",
                final_dir.to_string_lossy().to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let temp_dir = provider_dir.join(format!(".tmp-{}", Uuid::new_v4()));
        if let Err(err) = fs::create_dir_all(&temp_dir) {
            let err_detail = job_error_detail(
                ErrorCode::ToolchainInstallFailed,
                "failed to create temp dir",
                err.to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let _ = publish_progress(&mut job_client, &job_id, 65, "extracting", {
            let mut metrics = toolchain_base_metrics(
                &provider_id,
                &provider.name,
                &version,
                verify_hash,
                &host,
                Some(&artifact),
            );
            metrics.push(metric(
                "archive_path",
                archive_path.to_string_lossy().to_string(),
            ));
            metrics.push(metric("install_root", install_root.display()));
            metrics
        })
        .await;
        if let Err(err) = extract_archive(&archive_path, &temp_dir, Some(&cancel_rx)).await {
            if err.code() == tonic::Code::Cancelled {
                let _ = publish_log(
                    &mut job_client,
                    &job_id,
                    "Install cancelled during extract\n",
                )
                .await;
                let _ = fs::remove_dir_all(&temp_dir);
                return Ok(());
            }
            let _ = fs::remove_dir_all(&temp_dir);
            let err_detail = job_error_detail(
                ErrorCode::ToolchainInstallFailed,
                "failed to extract archive",
                err.message().to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        if cancel_requested(Some(&cancel_rx)) {
            let _ = publish_log(&mut job_client, &job_id, "Install cancelled\n").await;
            let _ = fs::remove_dir_all(&temp_dir);
            return Ok(());
        }

        let installed_at = now_ts();
        let cached_path = if is_remote_url(&artifact.url) {
            archive_path.to_string_lossy().to_string()
        } else {
            String::new()
        };
        let prov = Provenance {
            provider_id: provider_id.clone(),
            provider_name: provider.name.clone(),
            version: version.clone(),
            source_url: artifact.url.clone(),
            sha256: artifact.sha256.clone(),
            installed_at_unix_millis: installed_at.unix_millis,
            cached_path,
            host: host.clone(),
            artifact_size_bytes: artifact.size_bytes,
            signature: signature_value,
            signature_url,
            signature_public_key,
            transparency_log_entry: transparency_entry,
            transparency_log_entry_url: transparency_entry_url,
            transparency_log_public_key: transparency_public_key,
        };
        if let Err(err) = write_provenance(&temp_dir, &prov) {
            let _ = fs::remove_dir_all(&temp_dir);
            let err_detail = job_error_detail(
                ErrorCode::ToolchainInstallFailed,
                "failed to write provenance",
                err.to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let _ = publish_progress(&mut job_client, &job_id, 85, "finalizing", {
            let mut metrics = toolchain_base_metrics(
                &provider_id,
                &provider.name,
                &version,
                verify_hash,
                &host,
                Some(&artifact),
            );
            metrics.push(metric("install_root", install_root.display()));
            metrics
        })
        .await;
        if let Err(err) = finalize_install(&temp_dir, &final_dir) {
            let _ = fs::remove_dir_all(&temp_dir);
            let err_detail = job_error_detail(
                ErrorCode::ToolchainInstallFailed,
                "failed to finalize install",
                err.message().to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let toolchain_id = format!("tc-{}", Uuid::new_v4());
        let installed = InstalledToolchain {
            toolchain_id: Some(Id {
                value: toolchain_id.clone(),
            }),
            provider: Some(provider.clone()),
            version: available.version.clone(),
            install_path: final_dir.to_string_lossy().to_string(),
            verified: verify_hash,
            installed_at: Some(installed_at),
            source_url: artifact.url.clone(),
            sha256: artifact.sha256.clone(),
        };

        let mut st = self.state.lock().await;
        st.installed.push(installed);
        save_state_best_effort(&st);
        drop(st);

        let _ = publish_progress(&mut job_client, &job_id, 100, "completed", {
            let mut metrics = toolchain_base_metrics(
                &provider_id,
                &provider.name,
                &version,
                verify_hash,
                &host,
                Some(&artifact),
            );
            metrics.push(metric("install_path", final_dir.display()));
            metrics
        })
        .await;
        let _ = publish_completed(
            &mut job_client,
            &job_id,
            "Toolchain installed",
            vec![
                KeyValue {
                    key: "toolchain_id".into(),
                    value: toolchain_id,
                },
                KeyValue {
                    key: "provider".into(),
                    value: provider.name,
                },
                KeyValue {
                    key: "version".into(),
                    value: version,
                },
                KeyValue {
                    key: "install_path".into(),
                    value: final_dir.to_string_lossy().to_string(),
                },
            ],
        )
        .await;

        Ok(())
    }

    async fn run_uninstall_job(
        &self,
        job_id: String,
        toolchain_id: String,
        remove_cached_artifact: bool,
        force: bool,
    ) -> Result<(), Status> {
        let mut job_client = connect_job().await?;
        let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;

        if job_is_cancelled(&mut job_client, &job_id).await {
            let _ = publish_log(
                &mut job_client,
                &job_id,
                "Uninstall cancelled before start\n",
            )
            .await;
            return Ok(());
        }

        let _ = publish_state(&mut job_client, &job_id, JobState::Running).await;
        let _ = publish_log(&mut job_client, &job_id, "Starting toolchain uninstall\n").await;

        let (entry, cached_path, keep_paths, early_error) = {
            let st = self.state.lock().await;
            let entry = st
                .installed
                .iter()
                .find(|item| {
                    item.toolchain_id.as_ref().map(|id| id.value.as_str())
                        == Some(toolchain_id.as_str())
                })
                .cloned();
            if let Some(entry) = entry {
                if toolchain_referenced_by_sets(&st.toolchain_sets, &toolchain_id) && !force {
                    let err_detail = job_error_detail(
                        ErrorCode::InvalidArgument,
                        "toolchain is referenced by a toolchain set",
                        "use force to remove references".into(),
                        &job_id,
                    );
                    (None, None, Vec::new(), Some(err_detail))
                } else {
                    let cached_path = cached_path_for_entry(&entry);
                    let mut keep_paths = Vec::new();
                    for item in st.installed.iter() {
                        if item.toolchain_id.as_ref().map(|id| id.value.as_str())
                            == Some(toolchain_id.as_str())
                        {
                            continue;
                        }
                        if let Some(path) = cached_path_for_entry(item) {
                            keep_paths.push(path);
                        }
                    }
                    (Some(entry), cached_path, keep_paths, None)
                }
            } else {
                let err_detail = job_error_detail(
                    ErrorCode::NotFound,
                    "toolchain not found",
                    format!("toolchain_id={toolchain_id}"),
                    &job_id,
                );
                (None, None, Vec::new(), Some(err_detail))
            }
        };

        if let Some(err_detail) = early_error {
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }
        let entry = match entry {
            Some(entry) => entry,
            None => return Ok(()),
        };

        if cancel_requested(Some(&cancel_rx)) {
            let _ = publish_log(&mut job_client, &job_id, "Uninstall cancelled\n").await;
            return Ok(());
        }

        let install_path = PathBuf::from(entry.install_path.clone());
        if install_path.exists() {
            if let Err(err) = fs::remove_dir_all(&install_path) {
                let err_detail = job_error_detail(
                    ErrorCode::ToolchainUninstallFailed,
                    "failed to remove install directory",
                    err.to_string(),
                    &job_id,
                );
                let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                return Ok(());
            }
        } else {
            let _ = publish_log(
                &mut job_client,
                &job_id,
                &format!("Install path missing: {}\n", install_path.display()),
            )
            .await;
        }

        let mut cached_removed = false;
        let mut cached_skipped = false;
        if remove_cached_artifact {
            if let Some(path) = cached_path {
                if keep_paths.iter().any(|keep| keep == &path) {
                    cached_skipped = true;
                } else if path.exists() {
                    if let Err(err) = fs::remove_file(&path) {
                        let _ = publish_log(
                            &mut job_client,
                            &job_id,
                            &format!(
                                "Failed to remove cached artifact {}: {err}\n",
                                path.display()
                            ),
                        )
                        .await;
                    } else {
                        cached_removed = true;
                    }
                }
            }
        }

        let mut st = self.state.lock().await;
        if force {
            let removed_sets = scrub_toolchain_from_sets(&mut st.toolchain_sets, &toolchain_id);
            if let Some(active) = st.active_set_id.clone() {
                if removed_sets.iter().any(|id| id == &active) {
                    st.active_set_id = None;
                }
            }
        }
        st.installed.retain(|item| {
            item.toolchain_id.as_ref().map(|id| id.value.as_str()) != Some(toolchain_id.as_str())
        });
        save_state_best_effort(&st);
        drop(st);

        let mut outputs = vec![
            KeyValue {
                key: "toolchain_id".into(),
                value: toolchain_id.clone(),
            },
            KeyValue {
                key: "install_path".into(),
                value: entry.install_path.clone(),
            },
        ];
        if cached_removed {
            outputs.push(KeyValue {
                key: "cached_artifact_removed".into(),
                value: "true".into(),
            });
        } else if cached_skipped {
            outputs.push(KeyValue {
                key: "cached_artifact_removed".into(),
                value: "false".into(),
            });
            outputs.push(KeyValue {
                key: "cached_artifact_in_use".into(),
                value: "true".into(),
            });
        }

        let _ = publish_completed(&mut job_client, &job_id, "Toolchain uninstalled", outputs).await;

        Ok(())
    }

    async fn run_update_job(
        &self,
        job_id: String,
        toolchain_id: String,
        target_version: String,
        verify_hash: bool,
        remove_cached_artifact: bool,
    ) -> Result<(), Status> {
        let mut job_client = connect_job().await?;
        let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;

        if job_is_cancelled(&mut job_client, &job_id).await {
            let _ = publish_log(&mut job_client, &job_id, "Update cancelled before start\n").await;
            return Ok(());
        }

        let _ = publish_state(&mut job_client, &job_id, JobState::Running).await;
        let _ = publish_log(&mut job_client, &job_id, "Starting toolchain update\n").await;

        if target_version.trim().is_empty() {
            let err_detail = job_error_detail(
                ErrorCode::InvalidArgument,
                "target version is required",
                "".into(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let (current, install_root, early_error) = {
            let st = self.state.lock().await;
            let entry = st
                .installed
                .iter()
                .find(|item| {
                    item.toolchain_id.as_ref().map(|id| id.value.as_str())
                        == Some(toolchain_id.as_str())
                })
                .cloned();
            if let Some(entry) = entry {
                let install_root = install_root_for_entry(&entry);
                (Some(entry), install_root, None)
            } else {
                let err_detail = job_error_detail(
                    ErrorCode::NotFound,
                    "toolchain not found",
                    format!("toolchain_id={toolchain_id}"),
                    &job_id,
                );
                (None, default_install_root(), Some(err_detail))
            }
        };

        if let Some(err_detail) = early_error {
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }
        let current = match current {
            Some(entry) => entry,
            None => return Ok(()),
        };

        let current_version = version_from_installed(&current).unwrap_or_default();
        if current_version == target_version {
            let err_detail = job_error_detail(
                ErrorCode::InvalidArgument,
                "target version matches installed version",
                current_version,
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let provider_id = match provider_id_from_installed(&current) {
            Some(id) => id,
            None => {
                let err_detail = job_error_detail(
                    ErrorCode::ToolchainUpdateFailed,
                    "installed toolchain missing provider id",
                    "".into(),
                    &job_id,
                );
                let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                return Ok(());
            }
        };

        let host = host_key().unwrap_or_else(|| "unknown".into());
        let fixtures = fixtures_dir();
        if fixtures.is_none() && host == "unknown" {
            let host_raw = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
            let err_detail = job_error_detail(
                ErrorCode::ToolchainIncompatibleHost,
                "unsupported host for toolchain artifacts",
                host_raw,
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let available = match find_available(
            &self.catalog,
            &provider_id,
            &target_version,
            &host,
            fixtures.as_deref(),
        ) {
            Some(item) => item,
            None => {
                let err_detail = job_error_detail(
                    ErrorCode::ToolchainUpdateFailed,
                    "requested toolchain version not found",
                    format!("provider_id={provider_id} version={target_version}"),
                    &job_id,
                );
                let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                return Ok(());
            }
        };

        let provider = match available.provider.clone() {
            Some(p) => p,
            None => {
                let err_detail = job_error_detail(
                    ErrorCode::ToolchainUpdateFailed,
                    "unknown provider_id",
                    provider_id.clone(),
                    &job_id,
                );
                let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                return Ok(());
            }
        };

        let artifact = available.artifact.clone().unwrap_or(ToolchainArtifact {
            url: "".into(),
            sha256: "".into(),
            size_bytes: 0,
        });

        if artifact.url.is_empty() {
            let err_detail = job_error_detail(
                ErrorCode::ToolchainUpdateFailed,
                "artifact url missing",
                "".into(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let catalog_artifact = catalog_artifact_for_provenance(
            &self.catalog,
            &provider_id,
            &target_version,
            &artifact.url,
            &artifact.sha256,
        );
        let signature_record = catalog_artifact
            .as_ref()
            .and_then(signature_record_from_catalog);
        let transparency_record = catalog_artifact
            .as_ref()
            .and_then(transparency_record_from_catalog);
        if !verify_hash && signature_record.is_some() {
            warn!(
                "Skipping signature verification for provider {}",
                provider.name
            );
        }

        let provider_kind =
            ToolchainKind::try_from(provider.kind).unwrap_or(ToolchainKind::Unspecified);
        let _ = publish_progress(&mut job_client, &job_id, 10, "resolve artifact", {
            let mut metrics = toolchain_base_metrics(
                &provider_id,
                &provider.name,
                &target_version,
                verify_hash,
                &host,
                Some(&artifact),
            );
            metrics.push(metric("provider_kind", format!("{provider_kind:?}")));
            metrics.push(metric("toolchain_id", &toolchain_id));
            metrics.push(metric("current_version", &current_version));
            metrics.push(metric("target_version", &target_version));
            metrics.push(metric("remove_cached", remove_cached_artifact));
            metrics.push(metric("install_root", install_root.display()));
            metrics
        })
        .await;

        let archive_path =
            match ensure_artifact_local(&artifact, verify_hash, Some(&cancel_rx)).await {
                Ok(path) => path,
                Err(err) if err.code() == tonic::Code::Cancelled => {
                    let _ = publish_log(
                        &mut job_client,
                        &job_id,
                        "Update cancelled during download\n",
                    )
                    .await;
                    return Ok(());
                }
                Err(err) => {
                    let err_detail = job_error_detail(
                        ErrorCode::ToolchainUpdateFailed,
                        "failed to fetch or verify artifact",
                        err.message().to_string(),
                        &job_id,
                    );
                    let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                    return Ok(());
                }
            };

        let mut signature_value = String::new();
        let mut signature_url = String::new();
        let mut signature_public_key = String::new();
        let mut transparency_entry = String::new();
        let mut transparency_entry_url = String::new();
        let mut transparency_public_key = String::new();
        if let Some(record) = signature_record.as_ref() {
            signature_url = record.signature_url.clone();
            signature_public_key = record.public_key.clone();
            if verify_hash {
                match verify_signature_if_configured(&archive_path, Some(record)).await {
                    Ok(Some(sig)) => signature_value = sig,
                    Ok(None) => {}
                    Err(err) => {
                        let err_detail = job_error_detail(
                            ErrorCode::ToolchainUpdateFailed,
                            "signature verification failed",
                            err,
                            &job_id,
                        );
                        let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                        return Ok(());
                    }
                }
            } else if !record.signature.trim().is_empty() {
                signature_value = normalize_signature_value(&record.signature);
            }
        }
        if let Some(record) = transparency_record.as_ref() {
            transparency_entry_url = record.entry_url.clone();
            transparency_public_key = record.public_key.clone();
            match verify_transparency_log(record, &artifact.sha256).await {
                Ok((_entry, raw)) => {
                    transparency_entry = raw;
                }
                Err(err) => {
                    let err_detail = job_error_detail(
                        ErrorCode::ToolchainUpdateFailed,
                        "transparency log validation failed",
                        err,
                        &job_id,
                    );
                    let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                    return Ok(());
                }
            }
        }

        let _ = publish_progress(&mut job_client, &job_id, 45, "downloaded", {
            let mut metrics = toolchain_base_metrics(
                &provider_id,
                &provider.name,
                &target_version,
                verify_hash,
                &host,
                Some(&artifact),
            );
            metrics.push(metric("toolchain_id", &toolchain_id));
            metrics.push(metric("current_version", &current_version));
            metrics.push(metric("target_version", &target_version));
            metrics.push(metric("remove_cached", remove_cached_artifact));
            metrics.push(metric(
                "archive_path",
                archive_path.to_string_lossy().to_string(),
            ));
            metrics.push(metric("install_root", install_root.display()));
            metrics
        })
        .await;

        let provider_dir = install_root.join(&provider.name);
        if let Err(err) = fs::create_dir_all(&provider_dir) {
            let err_detail = job_error_detail(
                ErrorCode::ToolchainUpdateFailed,
                "failed to create install root",
                err.to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let final_dir = provider_dir.join(&target_version);
        if final_dir.exists() {
            let err_detail = job_error_detail(
                ErrorCode::ToolchainUpdateFailed,
                "target version already installed",
                final_dir.to_string_lossy().to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let temp_dir = provider_dir.join(format!(".tmp-{}", Uuid::new_v4()));
        if let Err(err) = fs::create_dir_all(&temp_dir) {
            let err_detail = job_error_detail(
                ErrorCode::ToolchainUpdateFailed,
                "failed to create temp dir",
                err.to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let _ = publish_progress(&mut job_client, &job_id, 65, "extracting", {
            let mut metrics = toolchain_base_metrics(
                &provider_id,
                &provider.name,
                &target_version,
                verify_hash,
                &host,
                Some(&artifact),
            );
            metrics.push(metric("toolchain_id", &toolchain_id));
            metrics.push(metric("current_version", &current_version));
            metrics.push(metric("target_version", &target_version));
            metrics.push(metric(
                "archive_path",
                archive_path.to_string_lossy().to_string(),
            ));
            metrics.push(metric("install_root", install_root.display()));
            metrics
        })
        .await;
        if let Err(err) = extract_archive(&archive_path, &temp_dir, Some(&cancel_rx)).await {
            if err.code() == tonic::Code::Cancelled {
                let _ = publish_log(
                    &mut job_client,
                    &job_id,
                    "Update cancelled during extract\n",
                )
                .await;
                let _ = fs::remove_dir_all(&temp_dir);
                return Ok(());
            }
            let _ = fs::remove_dir_all(&temp_dir);
            let err_detail = job_error_detail(
                ErrorCode::ToolchainUpdateFailed,
                "failed to extract archive",
                err.message().to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let installed_at = now_ts();
        let cached_path = if is_remote_url(&artifact.url) {
            archive_path.to_string_lossy().to_string()
        } else {
            String::new()
        };
        let prov = Provenance {
            provider_id: provider_id.clone(),
            provider_name: provider.name.clone(),
            version: target_version.clone(),
            source_url: artifact.url.clone(),
            sha256: artifact.sha256.clone(),
            installed_at_unix_millis: installed_at.unix_millis,
            cached_path,
            host: host.clone(),
            artifact_size_bytes: artifact.size_bytes,
            signature: signature_value,
            signature_url,
            signature_public_key,
            transparency_log_entry: transparency_entry,
            transparency_log_entry_url: transparency_entry_url,
            transparency_log_public_key: transparency_public_key,
        };
        if let Err(err) = write_provenance(&temp_dir, &prov) {
            let _ = fs::remove_dir_all(&temp_dir);
            let err_detail = job_error_detail(
                ErrorCode::ToolchainUpdateFailed,
                "failed to write provenance",
                err.to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let _ = publish_progress(&mut job_client, &job_id, 85, "finalizing", {
            let mut metrics = toolchain_base_metrics(
                &provider_id,
                &provider.name,
                &target_version,
                verify_hash,
                &host,
                Some(&artifact),
            );
            metrics.push(metric("toolchain_id", &toolchain_id));
            metrics.push(metric("current_version", &current_version));
            metrics.push(metric("target_version", &target_version));
            metrics.push(metric("install_root", install_root.display()));
            metrics
        })
        .await;
        if let Err(err) = finalize_install(&temp_dir, &final_dir) {
            let _ = fs::remove_dir_all(&temp_dir);
            let err_detail = job_error_detail(
                ErrorCode::ToolchainUpdateFailed,
                "failed to finalize install",
                err.message().to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let new_toolchain_id = format!("tc-{}", Uuid::new_v4());
        let installed = InstalledToolchain {
            toolchain_id: Some(Id {
                value: new_toolchain_id.clone(),
            }),
            provider: Some(provider.clone()),
            version: available.version.clone(),
            install_path: final_dir.to_string_lossy().to_string(),
            verified: verify_hash,
            installed_at: Some(installed_at),
            source_url: artifact.url,
            sha256: artifact.sha256,
        };

        let mut removed_old_install = false;
        if !current.install_path.trim().is_empty() {
            let old_path = PathBuf::from(&current.install_path);
            if old_path.exists() {
                if let Err(err) = fs::remove_dir_all(&old_path) {
                    let _ = publish_log(
                        &mut job_client,
                        &job_id,
                        &format!(
                            "Failed to remove old install path {}: {err}\n",
                            old_path.display()
                        ),
                    )
                    .await;
                } else {
                    removed_old_install = true;
                }
            }
        }

        let mut cached_removed = false;
        if remove_cached_artifact {
            let cached_path = cached_path_for_entry(&current);
            let keep_paths = {
                let st = self.state.lock().await;
                st.installed
                    .iter()
                    .filter(|item| {
                        item.toolchain_id.as_ref().map(|id| id.value.as_str())
                            != Some(toolchain_id.as_str())
                    })
                    .filter_map(cached_path_for_entry)
                    .collect::<Vec<_>>()
            };
            if let Some(path) = cached_path {
                if !keep_paths.iter().any(|keep| keep == &path)
                    && path.exists()
                    && fs::remove_file(&path).is_ok()
                {
                    cached_removed = true;
                }
            }
        }

        let mut st = self.state.lock().await;
        st.installed.push(installed);
        replace_toolchain_in_sets(&mut st.toolchain_sets, &toolchain_id, &new_toolchain_id);
        st.installed.retain(|item| {
            item.toolchain_id.as_ref().map(|id| id.value.as_str()) != Some(toolchain_id.as_str())
        });
        save_state_best_effort(&st);
        drop(st);

        let mut outputs = vec![
            KeyValue {
                key: "old_toolchain_id".into(),
                value: toolchain_id.clone(),
            },
            KeyValue {
                key: "new_toolchain_id".into(),
                value: new_toolchain_id.clone(),
            },
            KeyValue {
                key: "old_version".into(),
                value: current_version,
            },
            KeyValue {
                key: "new_version".into(),
                value: target_version.clone(),
            },
            KeyValue {
                key: "install_path".into(),
                value: final_dir.to_string_lossy().to_string(),
            },
        ];
        if removed_old_install {
            outputs.push(KeyValue {
                key: "old_install_removed".into(),
                value: "true".into(),
            });
        }
        if cached_removed {
            outputs.push(KeyValue {
                key: "cached_artifact_removed".into(),
                value: "true".into(),
            });
        }

        let _ = publish_completed(&mut job_client, &job_id, "Toolchain updated", outputs).await;

        Ok(())
    }

    async fn run_cache_cleanup_job(
        &self,
        job_id: String,
        dry_run: bool,
        remove_all: bool,
    ) -> Result<(), Status> {
        let mut job_client = connect_job().await?;
        let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;

        if job_is_cancelled(&mut job_client, &job_id).await {
            let _ = publish_log(&mut job_client, &job_id, "Cleanup cancelled before start\n").await;
            return Ok(());
        }

        let _ = publish_state(&mut job_client, &job_id, JobState::Running).await;
        let _ = publish_log(
            &mut job_client,
            &job_id,
            "Starting toolchain cache cleanup\n",
        )
        .await;

        let download_dir = download_dir();
        if !download_dir.exists() {
            let _ = publish_completed(
                &mut job_client,
                &job_id,
                "Toolchain cache cleanup complete",
                vec![
                    KeyValue {
                        key: "removed_count".into(),
                        value: "0".into(),
                    },
                    KeyValue {
                        key: "removed_bytes".into(),
                        value: "0".into(),
                    },
                    KeyValue {
                        key: "dry_run".into(),
                        value: dry_run.to_string(),
                    },
                ],
            )
            .await;
            return Ok(());
        }

        let keep_paths = if remove_all {
            Vec::new()
        } else {
            let st = self.state.lock().await;
            cached_paths_for_installed(&st.installed)
        };

        let mut removed_count = 0u64;
        let mut removed_bytes = 0u64;
        let mut scanned = 0u64;

        let entries = match fs::read_dir(&download_dir) {
            Ok(entries) => entries,
            Err(err) => {
                let err_detail = job_error_detail(
                    ErrorCode::ToolchainCacheCleanupFailed,
                    "failed to read download directory",
                    err.to_string(),
                    &job_id,
                );
                let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
                return Ok(());
            }
        };

        for entry in entries.flatten() {
            if cancel_requested(Some(&cancel_rx)) {
                let _ = publish_log(&mut job_client, &job_id, "Cleanup cancelled\n").await;
                return Ok(());
            }
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            scanned += 1;
            if !remove_all && keep_paths.iter().any(|keep| keep == &path) {
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if !dry_run {
                if let Err(err) = fs::remove_file(&path) {
                    let _ = publish_log(
                        &mut job_client,
                        &job_id,
                        &format!("Failed to remove {}: {err}\n", path.display()),
                    )
                    .await;
                    continue;
                }
            }
            removed_count += 1;
            removed_bytes += size;
        }

        let _ = publish_completed(
            &mut job_client,
            &job_id,
            "Toolchain cache cleanup complete",
            vec![
                KeyValue {
                    key: "removed_count".into(),
                    value: removed_count.to_string(),
                },
                KeyValue {
                    key: "removed_bytes".into(),
                    value: removed_bytes.to_string(),
                },
                KeyValue {
                    key: "scanned".into(),
                    value: scanned.to_string(),
                },
                KeyValue {
                    key: "dry_run".into(),
                    value: dry_run.to_string(),
                },
                KeyValue {
                    key: "remove_all".into(),
                    value: remove_all.to_string(),
                },
            ],
        )
        .await;

        Ok(())
    }
}

#[tonic::async_trait]
impl ToolchainService for Svc {
    async fn list_providers(
        &self,
        _request: Request<ListProvidersRequest>,
    ) -> Result<Response<ListProvidersResponse>, Status> {
        let providers = self
            .catalog
            .providers
            .iter()
            .map(provider_from_catalog)
            .collect::<Vec<_>>();
        Ok(Response::new(ListProvidersResponse { providers }))
    }

    async fn list_available(
        &self,
        request: Request<ListAvailableRequest>,
    ) -> Result<Response<ListAvailableResponse>, Status> {
        let req = request.into_inner();
        let pid = req.provider_id.map(|i| i.value).unwrap_or_default();
        let fixtures = fixtures_dir();
        let host = host_key().unwrap_or_else(|| "unknown".into());
        if fixtures.is_none() && host == "unknown" {
            return Err(Status::failed_precondition(
                "unsupported host for available toolchains",
            ));
        }
        if !self
            .catalog
            .providers
            .iter()
            .any(|provider| provider.provider_id == pid)
        {
            return Err(Status::not_found(format!("provider_id not found: {pid}")));
        }
        Ok(Response::new(ListAvailableResponse {
            items: available_for_provider(&self.catalog, &pid, &host, fixtures.as_deref()),
            page_info: Some(PageInfo {
                next_page_token: "".into(),
            }),
        }))
    }

    async fn list_installed(
        &self,
        request: Request<ListInstalledRequest>,
    ) -> Result<Response<ListInstalledResponse>, Status> {
        let req = request.into_inner();
        let filter = ToolchainKind::try_from(req.kind).unwrap_or(ToolchainKind::Unspecified);
        let st = self.state.lock().await;
        let items = st
            .installed
            .iter()
            .filter(|&item| {
                if filter == ToolchainKind::Unspecified {
                    true
                } else {
                    item.provider
                        .as_ref()
                        .map(|p| p.kind == filter as i32)
                        .unwrap_or(false)
                }
            })
            .cloned()
            .collect();
        Ok(Response::new(ListInstalledResponse { items }))
    }

    async fn list_toolchain_sets(
        &self,
        request: Request<ListToolchainSetsRequest>,
    ) -> Result<Response<ListToolchainSetsResponse>, Status> {
        let req = request.into_inner();
        let page = req.page.unwrap_or_default();
        let page_size = if page.page_size == 0 {
            DEFAULT_PAGE_SIZE
        } else {
            page.page_size as usize
        };
        let start = if page.page_token.trim().is_empty() {
            0
        } else {
            page.page_token
                .parse::<usize>()
                .map_err(|_| Status::invalid_argument("invalid page_token"))?
        };

        let st = self.state.lock().await;
        let total = st.toolchain_sets.len();
        if start >= total && total != 0 {
            return Err(Status::invalid_argument("page_token out of range"));
        }
        let end = (start + page_size).min(total);
        let sets = st.toolchain_sets[start..end].to_vec();
        let next_token = if end < total {
            end.to_string()
        } else {
            String::new()
        };

        Ok(Response::new(ListToolchainSetsResponse {
            sets,
            page_info: Some(PageInfo {
                next_page_token: next_token,
            }),
        }))
    }

    async fn install_toolchain(
        &self,
        _request: Request<InstallToolchainRequest>,
    ) -> Result<Response<InstallToolchainResponse>, Status> {
        let req = _request.into_inner();
        let provider_id = req
            .provider_id
            .as_ref()
            .map(|id| id.value.trim())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Status::invalid_argument("provider_id is required"))?
            .to_string();
        let version = req.version.trim().to_string();
        if version.is_empty() {
            return Err(Status::invalid_argument("version is required"));
        };

        if !self
            .catalog
            .providers
            .iter()
            .any(|provider| provider.provider_id == provider_id)
        {
            return Err(Status::not_found(format!(
                "provider_id not found: {provider_id}"
            )));
        }

        let fixtures = fixtures_dir();
        let host = host_key();
        if fixtures.is_none() && host.is_none() {
            let host_raw = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
            return Err(Status::failed_precondition(format!(
                "unsupported host for toolchain artifacts: {host_raw}"
            )));
        }
        let host = host.unwrap_or_else(|| "unknown".into());

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
                "toolchain.install",
                vec![
                    KeyValue {
                        key: "provider_id".into(),
                        value: provider_id.clone(),
                    },
                    KeyValue {
                        key: "version".into(),
                        value: version.clone(),
                    },
                    KeyValue {
                        key: "verify_hash".into(),
                        value: req.verify_hash.to_string(),
                    },
                ],
                correlation_id,
                req.run_id.clone(),
            )
            .await?
        } else {
            job_id
        };

        let svc = self.clone();
        let install_root = req.install_root;
        let verify_hash = req.verify_hash;
        let job_id_for_spawn = job_id.clone();

        tokio::spawn(async move {
            if let Err(err) = svc
                .run_install_job(
                    job_id_for_spawn.clone(),
                    provider_id,
                    version,
                    install_root,
                    verify_hash,
                    host,
                    fixtures,
                )
                .await
            {
                warn!("install job {} failed to run: {}", job_id_for_spawn, err);
            }
        });

        Ok(Response::new(InstallToolchainResponse {
            job_id: Some(Id {
                value: job_id.clone(),
            }),
        }))
    }

    async fn verify_toolchain(
        &self,
        _request: Request<VerifyToolchainRequest>,
    ) -> Result<Response<VerifyToolchainResponse>, Status> {
        let req = _request.into_inner();
        let id = req
            .toolchain_id
            .as_ref()
            .map(|id| id.value.trim())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Status::invalid_argument("toolchain_id is required"))?
            .to_string();

        let fixtures = fixtures_dir();
        let host = host_key().unwrap_or_else(|| "unknown".into());
        if fixtures.is_none() && host == "unknown" {
            let host_raw = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
            return Err(Status::failed_precondition(format!(
                "unsupported host for toolchain artifacts: {host_raw}"
            )));
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
                "toolchain.verify",
                vec![KeyValue {
                    key: "toolchain_id".into(),
                    value: id.clone(),
                }],
                correlation_id,
                req.run_id.clone(),
            )
            .await?
        } else {
            job_id
        };

        let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;
        if job_is_cancelled(&mut job_client, &job_id).await {
            let _ = publish_log(
                &mut job_client,
                &job_id,
                "Verification cancelled before start\n",
            )
            .await;
            return Ok(Response::new(VerifyToolchainResponse {
                verified: false,
                error: Some(job_error_detail(
                    ErrorCode::Cancelled,
                    "verification cancelled",
                    "cancelled before start".into(),
                    &job_id,
                )),
                job_id: Some(Id { value: job_id }),
            }));
        }

        if let Err(err) = publish_state(&mut job_client, &job_id, JobState::Running).await {
            warn!("Failed to publish job state: {}", err);
        }
        if let Err(err) = publish_log(
            &mut job_client,
            &job_id,
            "Starting toolchain verification\n",
        )
        .await
        {
            warn!("Failed to publish job log: {}", err);
        }

        if cancel_requested(Some(&cancel_rx)) {
            let _ = publish_log(&mut job_client, &job_id, "Verification cancelled\n").await;
            return Ok(Response::new(VerifyToolchainResponse {
                verified: false,
                error: Some(job_error_detail(
                    ErrorCode::Cancelled,
                    "verification cancelled",
                    "cancelled before checks".into(),
                    &job_id,
                )),
                job_id: Some(Id { value: job_id }),
            }));
        }

        let mut st = self.state.lock().await;
        let entry = st
            .installed
            .iter_mut()
            .find(|item| item.toolchain_id.as_ref().map(|i| &i.value) == Some(&id));

        let Some(entry) = entry else {
            let err_detail = job_error_detail(
                ErrorCode::NotFound,
                "toolchain not found",
                format!("toolchain_id={id}"),
                &job_id,
            );
            let resp = VerifyToolchainResponse {
                verified: false,
                error: Some(err_detail),
                job_id: Some(Id {
                    value: job_id.clone(),
                }),
            };
            save_state_best_effort(&st);
            drop(st);
            let _ = publish_failed(&mut job_client, &job_id, resp.error.clone().unwrap()).await;
            return Ok(Response::new(resp));
        };

        let install_path = Path::new(&entry.install_path);
        if !install_path.exists() {
            entry.verified = false;
            let err_detail = job_error_detail(
                ErrorCode::ToolchainVerifyFailed,
                "install path missing",
                entry.install_path.clone(),
                &job_id,
            );
            let resp = VerifyToolchainResponse {
                verified: false,
                error: Some(err_detail),
                job_id: Some(Id {
                    value: job_id.clone(),
                }),
            };
            save_state_best_effort(&st);
            drop(st);
            let _ = publish_failed(&mut job_client, &job_id, resp.error.clone().unwrap()).await;
            return Ok(Response::new(resp));
        }

        let prov_path = install_path.join("provenance.txt");
        if !prov_path.exists() {
            entry.verified = false;
            let err_detail = job_error_detail(
                ErrorCode::ToolchainVerifyFailed,
                "provenance file missing",
                prov_path.to_string_lossy().to_string(),
                &job_id,
            );
            let resp = VerifyToolchainResponse {
                verified: false,
                error: Some(err_detail),
                job_id: Some(Id {
                    value: job_id.clone(),
                }),
            };
            save_state_best_effort(&st);
            drop(st);
            let _ = publish_failed(&mut job_client, &job_id, resp.error.clone().unwrap()).await;
            return Ok(Response::new(resp));
        }

        let provenance = match read_provenance(&prov_path) {
            Ok(prov) => prov,
            Err(err) => {
                entry.verified = false;
                let err_detail = job_error_detail(
                    ErrorCode::ToolchainVerifyFailed,
                    "failed to read provenance",
                    err.to_string(),
                    &job_id,
                );
                let resp = VerifyToolchainResponse {
                    verified: false,
                    error: Some(err_detail),
                    job_id: Some(Id {
                        value: job_id.clone(),
                    }),
                };
                save_state_best_effort(&st);
                drop(st);
                let _ = publish_failed(&mut job_client, &job_id, resp.error.clone().unwrap()).await;
                return Ok(Response::new(resp));
            }
        };

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            10,
            "loading provenance",
            vec![
                metric("toolchain_id", &id),
                metric("install_path", &entry.install_path),
                metric("provenance_path", prov_path.display()),
                metric("provider_id", &provenance.provider_id),
                metric("version", &provenance.version),
            ],
        )
        .await;

        if let Err(err) = verify_provenance_entry(entry, &provenance, &self.catalog) {
            entry.verified = false;
            let err_detail = job_error_detail(
                ErrorCode::ToolchainVerifyFailed,
                "provenance validation failed",
                err,
                &job_id,
            );
            let resp = VerifyToolchainResponse {
                verified: false,
                error: Some(err_detail),
                job_id: Some(Id {
                    value: job_id.clone(),
                }),
            };
            save_state_best_effort(&st);
            drop(st);
            let _ = publish_failed(&mut job_client, &job_id, resp.error.clone().unwrap()).await;
            return Ok(Response::new(resp));
        }

        let source_url = if entry.source_url.trim().is_empty() {
            provenance.source_url.clone()
        } else {
            entry.source_url.clone()
        };
        let sha256 = if entry.sha256.trim().is_empty() {
            provenance.sha256.clone()
        } else {
            entry.sha256.clone()
        };

        if source_url.is_empty() {
            entry.verified = false;
            let err_detail = job_error_detail(
                ErrorCode::ToolchainVerifyFailed,
                "no source url available for verification",
                "".into(),
                &job_id,
            );
            let resp = VerifyToolchainResponse {
                verified: false,
                error: Some(err_detail),
                job_id: Some(Id {
                    value: job_id.clone(),
                }),
            };
            save_state_best_effort(&st);
            drop(st);
            let _ = publish_failed(&mut job_client, &job_id, resp.error.clone().unwrap()).await;
            return Ok(Response::new(resp));
        }

        let signature_record = signature_record_from_provenance(&provenance).or_else(|| {
            if is_remote_url(&source_url) {
                catalog_artifact_for_provenance(
                    &self.catalog,
                    &provenance.provider_id,
                    &provenance.version,
                    &source_url,
                    &sha256,
                )
                .and_then(|artifact| signature_record_from_catalog(&artifact))
            } else {
                None
            }
        });
        let transparency_record = transparency_record_from_provenance(&provenance).or_else(|| {
            if is_remote_url(&source_url) {
                catalog_artifact_for_provenance(
                    &self.catalog,
                    &provenance.provider_id,
                    &provenance.version,
                    &source_url,
                    &sha256,
                )
                .and_then(|artifact| transparency_record_from_catalog(&artifact))
            } else {
                None
            }
        });

        if cancel_requested(Some(&cancel_rx)) {
            let _ = publish_log(&mut job_client, &job_id, "Verification cancelled\n").await;
            return Ok(Response::new(VerifyToolchainResponse {
                verified: false,
                error: Some(job_error_detail(
                    ErrorCode::Cancelled,
                    "verification cancelled",
                    "cancelled before download".into(),
                    &job_id,
                )),
                job_id: Some(Id { value: job_id }),
            }));
        }

        let artifact = ToolchainArtifact {
            url: source_url.clone(),
            sha256: sha256.clone(),
            size_bytes: 0,
        };

        drop(st);

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            45,
            "fetching artifact",
            vec![
                metric("toolchain_id", &id),
                metric("artifact_url", &artifact.url),
                metric("artifact_sha256", &artifact.sha256),
            ],
        )
        .await;

        let verify_result = ensure_artifact_local(&artifact, true, Some(&cancel_rx)).await;

        if let Ok(local_path) = &verify_result {
            let _ = publish_progress(
                &mut job_client,
                &job_id,
                80,
                "verifying artifact",
                vec![
                    metric("toolchain_id", &id),
                    metric("artifact_path", local_path.to_string_lossy().to_string()),
                    metric("artifact_sha256", &artifact.sha256),
                ],
            )
            .await;
        }

        let signature_result = match &verify_result {
            Ok(local_path) => {
                verify_signature_if_configured(local_path, signature_record.as_ref()).await
            }
            Err(_) => Ok(None),
        };
        let signature_error = signature_result.as_ref().err().map(|err| {
            job_error_detail(
                ErrorCode::ToolchainVerifyFailed,
                "signature verification failed",
                err.clone(),
                &job_id,
            )
        });

        let transparency_result = match (&verify_result, transparency_record.as_ref()) {
            (Ok(_), Some(record)) => verify_transparency_log(record, &artifact.sha256)
                .await
                .map(|(entry, _raw)| Some(entry)),
            _ => Ok(None),
        };
        let transparency_error = transparency_result.as_ref().err().map(|err| {
            job_error_detail(
                ErrorCode::ToolchainVerifyFailed,
                "transparency log validation failed",
                err.clone(),
                &job_id,
            )
        });
        let transparency_entry = transparency_result.clone().ok().and_then(|entry| entry);

        let mut st = self.state.lock().await;
        let entry = st
            .installed
            .iter_mut()
            .find(|item| item.toolchain_id.as_ref().map(|i| &i.value) == Some(&id));

        let Some(entry) = entry else {
            let err_detail = job_error_detail(
                ErrorCode::NotFound,
                "toolchain not found during verification",
                format!("toolchain_id={id}"),
                &job_id,
            );
            let resp = VerifyToolchainResponse {
                verified: false,
                error: Some(err_detail),
                job_id: Some(Id {
                    value: job_id.clone(),
                }),
            };
            save_state_best_effort(&st);
            drop(st);
            let _ = publish_failed(&mut job_client, &job_id, resp.error.clone().unwrap()).await;
            return Ok(Response::new(resp));
        };

        let mut publish_failure: Option<ErrorDetail> = None;
        let mut publish_success: Option<(String, String)> = None;

        let resp = match verify_result {
            Ok(local_path) => {
                let kind = entry
                    .provider
                    .as_ref()
                    .map(|p| ToolchainKind::try_from(p.kind).unwrap_or(ToolchainKind::Unspecified))
                    .unwrap_or(ToolchainKind::Unspecified);

                if let Some(err_detail) = signature_error.clone() {
                    entry.verified = false;
                    publish_failure = Some(err_detail.clone());
                    VerifyToolchainResponse {
                        verified: false,
                        error: Some(err_detail),
                        job_id: Some(Id {
                            value: job_id.clone(),
                        }),
                    }
                } else if let Some(err_detail) = transparency_error.clone() {
                    entry.verified = false;
                    publish_failure = Some(err_detail.clone());
                    VerifyToolchainResponse {
                        verified: false,
                        error: Some(err_detail),
                        job_id: Some(Id {
                            value: job_id.clone(),
                        }),
                    }
                } else {
                    let mut size_mismatch: Option<ErrorDetail> = None;
                    if provenance.artifact_size_bytes > 0 {
                        if let Ok(meta) = fs::metadata(&local_path) {
                            if meta.len() != provenance.artifact_size_bytes {
                                entry.verified = false;
                                let err_detail = job_error_detail(
                                    ErrorCode::ToolchainVerifyFailed,
                                    "artifact size mismatch",
                                    format!(
                                        "expected={} actual={}",
                                        provenance.artifact_size_bytes,
                                        meta.len()
                                    ),
                                    &job_id,
                                );
                                size_mismatch = Some(err_detail);
                            }
                        }
                    }

                    if let Some(err_detail) = size_mismatch {
                        publish_failure = Some(err_detail.clone());
                        VerifyToolchainResponse {
                            verified: false,
                            error: Some(err_detail),
                            job_id: Some(Id {
                                value: job_id.clone(),
                            }),
                        }
                    } else if let Err(msg) =
                        validate_toolchain_layout(kind, Path::new(&entry.install_path))
                    {
                        entry.verified = false;
                        let err_detail = job_error_detail(
                            ErrorCode::ToolchainVerifyFailed,
                            "post-install validation failed",
                            msg,
                            &job_id,
                        );
                        publish_failure = Some(err_detail.clone());
                        VerifyToolchainResponse {
                            verified: false,
                            error: Some(err_detail),
                            job_id: Some(Id {
                                value: job_id.clone(),
                            }),
                        }
                    } else {
                        entry.verified = true;
                        publish_success = Some((
                            local_path.to_string_lossy().to_string(),
                            entry.install_path.clone(),
                        ));
                        VerifyToolchainResponse {
                            verified: true,
                            error: None,
                            job_id: Some(Id {
                                value: job_id.clone(),
                            }),
                        }
                    }
                }
            }
            Err(err) => {
                if err.code() == tonic::Code::Cancelled {
                    VerifyToolchainResponse {
                        verified: false,
                        error: Some(job_error_detail(
                            ErrorCode::Cancelled,
                            "verification cancelled",
                            "cancelled during verification".into(),
                            &job_id,
                        )),
                        job_id: Some(Id {
                            value: job_id.clone(),
                        }),
                    }
                } else {
                    entry.verified = false;
                    let err_detail = job_error_detail(
                        ErrorCode::ToolchainVerifyFailed,
                        "verification failed",
                        err.message().to_string(),
                        &job_id,
                    );
                    publish_failure = Some(err_detail.clone());
                    VerifyToolchainResponse {
                        verified: false,
                        error: Some(err_detail),
                        job_id: Some(Id {
                            value: job_id.clone(),
                        }),
                    }
                }
            }
        };
        save_state_best_effort(&st);
        drop(st);

        if let Some(err_detail) = publish_failure {
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
        }
        if let Some((artifact_path, install_path)) = publish_success {
            let mut metrics = vec![
                metric("toolchain_id", &id),
                metric("artifact_path", &artifact_path),
                metric("install_path", &install_path),
                metric("provider_id", &provenance.provider_id),
                metric("version", &provenance.version),
            ];
            if let Some(entry) = transparency_entry.as_ref() {
                if !entry.log_id.trim().is_empty() {
                    metrics.push(metric("transparency_log_id", &entry.log_id));
                }
                if let Some(index) = entry.log_index {
                    metrics.push(metric("transparency_log_index", index));
                }
                if let Some(ts) = entry.timestamp_unix_millis {
                    metrics.push(metric("transparency_log_timestamp", ts));
                }
            }
            let _ = publish_progress(&mut job_client, &job_id, 100, "verified", metrics).await;
            let _ = publish_completed(
                &mut job_client,
                &job_id,
                "Toolchain verified",
                vec![
                    KeyValue {
                        key: "toolchain_id".into(),
                        value: id.clone(),
                    },
                    KeyValue {
                        key: "install_path".into(),
                        value: install_path,
                    },
                ],
            )
            .await;
        }

        Ok(Response::new(resp))
    }

    async fn update_toolchain(
        &self,
        request: Request<UpdateToolchainRequest>,
    ) -> Result<Response<UpdateToolchainResponse>, Status> {
        let req = request.into_inner();
        let toolchain_id = req
            .toolchain_id
            .ok_or_else(|| Status::invalid_argument("toolchain_id is required"))?
            .value;

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
                "toolchain.update",
                vec![
                    KeyValue {
                        key: "toolchain_id".into(),
                        value: toolchain_id.clone(),
                    },
                    KeyValue {
                        key: "version".into(),
                        value: req.version.clone(),
                    },
                    KeyValue {
                        key: "verify_hash".into(),
                        value: req.verify_hash.to_string(),
                    },
                    KeyValue {
                        key: "remove_cached".into(),
                        value: req.remove_cached_artifact.to_string(),
                    },
                ],
                correlation_id,
                req.run_id.clone(),
            )
            .await?
        } else {
            job_id
        };

        let svc = self.clone();
        let job_id_for_spawn = job_id.clone();
        tokio::spawn(async move {
            if let Err(err) = svc
                .run_update_job(
                    job_id_for_spawn.clone(),
                    toolchain_id,
                    req.version,
                    req.verify_hash,
                    req.remove_cached_artifact,
                )
                .await
            {
                warn!("update job {} failed to run: {}", job_id_for_spawn, err);
            }
        });

        Ok(Response::new(UpdateToolchainResponse {
            job_id: Some(Id { value: job_id }),
        }))
    }

    async fn uninstall_toolchain(
        &self,
        request: Request<UninstallToolchainRequest>,
    ) -> Result<Response<UninstallToolchainResponse>, Status> {
        let req = request.into_inner();
        let toolchain_id = req
            .toolchain_id
            .ok_or_else(|| Status::invalid_argument("toolchain_id is required"))?
            .value;

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
                "toolchain.uninstall",
                vec![
                    KeyValue {
                        key: "toolchain_id".into(),
                        value: toolchain_id.clone(),
                    },
                    KeyValue {
                        key: "remove_cached".into(),
                        value: req.remove_cached_artifact.to_string(),
                    },
                    KeyValue {
                        key: "force".into(),
                        value: req.force.to_string(),
                    },
                ],
                correlation_id,
                req.run_id.clone(),
            )
            .await?
        } else {
            job_id
        };

        let svc = self.clone();
        let job_id_for_spawn = job_id.clone();
        tokio::spawn(async move {
            if let Err(err) = svc
                .run_uninstall_job(
                    job_id_for_spawn.clone(),
                    toolchain_id,
                    req.remove_cached_artifact,
                    req.force,
                )
                .await
            {
                warn!("uninstall job {} failed to run: {}", job_id_for_spawn, err);
            }
        });

        Ok(Response::new(UninstallToolchainResponse {
            job_id: Some(Id { value: job_id }),
        }))
    }

    async fn cleanup_toolchain_cache(
        &self,
        request: Request<CleanupToolchainCacheRequest>,
    ) -> Result<Response<CleanupToolchainCacheResponse>, Status> {
        let req = request.into_inner();

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
                "toolchain.cleanup_cache",
                vec![
                    KeyValue {
                        key: "dry_run".into(),
                        value: req.dry_run.to_string(),
                    },
                    KeyValue {
                        key: "remove_all".into(),
                        value: req.remove_all.to_string(),
                    },
                ],
                correlation_id,
                req.run_id.clone(),
            )
            .await?
        } else {
            job_id
        };

        let svc = self.clone();
        let job_id_for_spawn = job_id.clone();
        tokio::spawn(async move {
            if let Err(err) = svc
                .run_cache_cleanup_job(job_id_for_spawn.clone(), req.dry_run, req.remove_all)
                .await
            {
                warn!(
                    "cache cleanup job {} failed to run: {}",
                    job_id_for_spawn, err
                );
            }
        });

        Ok(Response::new(CleanupToolchainCacheResponse {
            job_id: Some(Id { value: job_id }),
        }))
    }

    async fn create_toolchain_set(
        &self,
        request: Request<CreateToolchainSetRequest>,
    ) -> Result<Response<CreateToolchainSetResponse>, Status> {
        let req = request.into_inner();
        let sdk_id = normalize_id(req.sdk_toolchain_id);
        let ndk_id = normalize_id(req.ndk_toolchain_id);
        if sdk_id.is_none() && ndk_id.is_none() {
            return Err(Status::invalid_argument(
                "sdk_toolchain_id or ndk_toolchain_id is required",
            ));
        }

        let mut st = self.state.lock().await;
        if let Some(ref sdk_id) = sdk_id {
            if !installed_toolchain_matches(&st.installed, sdk_id, ToolchainKind::Sdk) {
                return Err(Status::not_found(format!(
                    "sdk toolchain not installed: {sdk_id}"
                )));
            }
        }
        if let Some(ref ndk_id) = ndk_id {
            if !installed_toolchain_matches(&st.installed, ndk_id, ToolchainKind::Ndk) {
                return Err(Status::not_found(format!(
                    "ndk toolchain not installed: {ndk_id}"
                )));
            }
        }

        let display_name = if req.display_name.trim().is_empty() {
            "SDK+NDK".into()
        } else {
            req.display_name.trim().to_string()
        };

        let set = ToolchainSet {
            toolchain_set_id: Some(Id {
                value: format!("set-{}", Uuid::new_v4()),
            }),
            sdk_toolchain_id: sdk_id.map(|value| Id { value }),
            ndk_toolchain_id: ndk_id.map(|value| Id { value }),
            display_name,
        };
        st.toolchain_sets.push(set.clone());
        save_state_best_effort(&st);

        Ok(Response::new(CreateToolchainSetResponse { set: Some(set) }))
    }

    async fn set_active_toolchain_set(
        &self,
        request: Request<SetActiveToolchainSetRequest>,
    ) -> Result<Response<SetActiveToolchainSetResponse>, Status> {
        let req = request.into_inner();
        let mut st = self.state.lock().await;
        let set_id = normalize_id(req.toolchain_set_id)
            .ok_or_else(|| Status::invalid_argument("toolchain_set_id is required"))?;
        let exists = st
            .toolchain_sets
            .iter()
            .any(|set| toolchain_set_id(set) == Some(set_id.as_str()));
        if !exists {
            return Err(Status::not_found(format!(
                "toolchain_set_id not found: {set_id}"
            )));
        }
        st.active_set_id = Some(set_id);
        save_state_best_effort(&st);
        Ok(Response::new(SetActiveToolchainSetResponse { ok: true }))
    }

    async fn get_active_toolchain_set(
        &self,
        _request: Request<GetActiveToolchainSetRequest>,
    ) -> Result<Response<GetActiveToolchainSetResponse>, Status> {
        let st = self.state.lock().await;
        let active_set = st
            .active_set_id
            .as_ref()
            .and_then(|id| {
                st.toolchain_sets
                    .iter()
                    .find(|set| toolchain_set_id(set) == Some(id.as_str()))
            })
            .cloned();
        Ok(Response::new(GetActiveToolchainSetResponse {
            set: active_set,
        }))
    }
}
