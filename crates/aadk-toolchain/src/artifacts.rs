use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
};

use aadk_proto::aadk::v1::{InstalledToolchain, ToolchainArtifact};
use futures_util::StreamExt;
use reqwest::Client;
use sha2::{Digest, Sha256};
use tokio::{io::AsyncReadExt, io::AsyncWriteExt, process::Command, sync::watch};
use tonic::Status;
use uuid::Uuid;

use crate::cancel::cancel_requested;
use crate::hashing::{hex_encode, sha256_file, short_hash};
use crate::provenance::read_provenance;
use crate::state::data_dir;

pub(crate) fn download_dir() -> PathBuf {
    data_dir().join("downloads")
}

pub(crate) fn cached_artifact_path(url: &str) -> PathBuf {
    download_dir().join(cache_file_name(url))
}

pub(crate) fn is_remote_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

pub(crate) fn cached_path_for_entry(entry: &InstalledToolchain) -> Option<PathBuf> {
    if !is_remote_url(entry.source_url.as_str()) {
        return None;
    }
    let prov_path = Path::new(&entry.install_path).join("provenance.txt");
    if prov_path.exists() {
        if let Ok(prov) = read_provenance(&prov_path) {
            if !prov.cached_path.trim().is_empty() {
                return Some(PathBuf::from(prov.cached_path));
            }
        }
    }
    Some(cached_artifact_path(&entry.source_url))
}

pub(crate) fn cached_paths_for_installed(installed: &[InstalledToolchain]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in installed {
        if let Some(path) = cached_path_for_entry(entry) {
            paths.push(path);
        }
    }
    paths
}

pub(crate) fn local_artifact_path(url: &str) -> Option<PathBuf> {
    if url.starts_with("file://") {
        Some(PathBuf::from(url.trim_start_matches("file://")))
    } else if url.starts_with('/') {
        Some(PathBuf::from(url))
    } else {
        None
    }
}

pub(crate) async fn ensure_artifact_local(
    artifact: &ToolchainArtifact,
    verify_hash: bool,
    cancel_rx: Option<&watch::Receiver<bool>>,
) -> Result<PathBuf, Status> {
    if artifact.url.is_empty() {
        return Err(Status::invalid_argument("artifact url is missing"));
    }
    if verify_hash && artifact.sha256.is_empty() {
        return Err(Status::failed_precondition(
            "sha256 missing for artifact; cannot verify",
        ));
    }

    if is_remote_url(&artifact.url) {
        let cache_path = cached_artifact_path(&artifact.url);
        if cache_path.exists() {
            if verify_hash {
                let actual = sha256_file(&cache_path)
                    .map_err(|e| Status::internal(format!("hashing failed: {e}")))?;
                if actual == artifact.sha256 {
                    tracing::info!("Using cached artifact {}", cache_path.display());
                    return Ok(cache_path);
                }
                let _ = fs::remove_file(&cache_path);
            } else {
                tracing::info!("Using cached artifact {}", cache_path.display());
                return Ok(cache_path);
            }
        }

        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| Status::internal(format!("failed to create download dir: {e}")))?;
        }

        tracing::info!("Downloading artifact {}", artifact.url);
        download_artifact(
            &artifact.url,
            &cache_path,
            verify_hash,
            &artifact.sha256,
            cancel_rx,
        )
        .await?;
        tracing::info!("Saved artifact {}", cache_path.display());
        return Ok(cache_path);
    }

    let path = local_artifact_path(&artifact.url)
        .ok_or_else(|| Status::invalid_argument("artifact url is not a local file"))?;
    if !path.exists() {
        return Err(Status::not_found(format!(
            "artifact not found: {}",
            path.display()
        )));
    }

    if verify_hash {
        if artifact.sha256.is_empty() {
            return Err(Status::failed_precondition(
                "sha256 missing for artifact; cannot verify",
            ));
        }
        let actual =
            sha256_file(&path).map_err(|e| Status::internal(format!("hashing failed: {e}")))?;
        if actual != artifact.sha256 {
            return Err(Status::failed_precondition("sha256 mismatch"));
        }
    }

    Ok(path)
}

pub(crate) async fn extract_archive(
    archive: &Path,
    dest: &Path,
    cancel_rx: Option<&watch::Receiver<bool>>,
) -> Result<(), Status> {
    let archive_str = archive.to_string_lossy();
    let mut cmd = Command::new("tar");

    if archive_str.ends_with(".tar.xz") || archive_str.ends_with(".txz") {
        cmd.arg("-xJf");
    } else if archive_str.ends_with(".tar.zst") || archive_str.ends_with(".tzst") {
        cmd.arg("-I").arg("zstd").arg("-xf");
    } else if archive_str.ends_with(".tar.gz") || archive_str.ends_with(".tgz") {
        cmd.arg("-xzf");
    } else {
        return Err(Status::invalid_argument(
            "unsupported archive format (expected .tar.xz, .tar.zst, or .tar.gz)",
        ));
    }

    cmd.arg(archive);
    cmd.arg("-C").arg(dest);

    tracing::info!(
        "Extracting archive {} into {}",
        archive.display(),
        dest.display()
    );

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| Status::internal(format!("failed to run tar: {e}")))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout {
            let _ = out.read_to_end(&mut buf).await;
        }
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut out) = stderr {
            let _ = out.read_to_end(&mut buf).await;
        }
        buf
    });

    let mut cancel_rx = cancel_rx.cloned();
    let status = if let Some(cancel_rx) = cancel_rx.as_mut() {
        loop {
            tokio::select! {
                status = child.wait() => break status,
                _ = cancel_rx.changed() => {
                    if cancel_requested(Some(cancel_rx)) {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        return Err(Status::cancelled("extract cancelled"));
                    }
                }
            }
        }
    } else {
        child.wait().await
    }
    .map_err(|e| Status::internal(format!("failed to run tar: {e}")))?;

    let stdout = stdout_task.await.unwrap_or_default();
    let stderr = stderr_task.await.unwrap_or_default();

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let stdout = String::from_utf8_lossy(&stdout);
        return Err(Status::internal(format!(
            "tar failed: {}\n{}\n{}",
            status,
            stdout.trim(),
            stderr.trim()
        )));
    }

    Ok(())
}

#[allow(clippy::result_large_err)]
pub(crate) fn finalize_install(temp_dir: &Path, final_dir: &Path) -> Result<(), Status> {
    let entries = fs::read_dir(temp_dir)
        .map_err(|e| Status::internal(format!("failed to read install temp dir: {e}")))?;
    let mut root_dir: Option<PathBuf> = None;
    let mut extra_entries = 0usize;

    for entry in entries {
        let entry =
            entry.map_err(|e| Status::internal(format!("failed to read dir entry: {e}")))?;
        let name = entry.file_name();
        if name == "provenance.txt" {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|e| Status::internal(format!("failed to read dir entry type: {e}")))?;
        if file_type.is_dir() && root_dir.is_none() {
            root_dir = Some(entry.path());
        } else {
            extra_entries += 1;
        }
    }

    if let Some(root) = root_dir {
        if extra_entries == 0 {
            let prov_src = temp_dir.join("provenance.txt");
            if prov_src.exists() {
                let prov_dest = root.join("provenance.txt");
                if prov_dest.exists() {
                    let _ = fs::remove_file(&prov_dest);
                }
                fs::rename(&prov_src, &prov_dest).map_err(|e| {
                    Status::internal(format!("failed to move provenance into root: {e}"))
                })?;
            }

            fs::rename(&root, final_dir)
                .map_err(|e| Status::internal(format!("failed to finalize install: {e}")))?;
            fs::remove_dir_all(temp_dir)
                .map_err(|e| Status::internal(format!("failed to clean temp dir: {e}")))?;
            return Ok(());
        }
    }

    fs::rename(temp_dir, final_dir)
        .map_err(|e| Status::internal(format!("failed to finalize install: {e}")))?;
    Ok(())
}

async fn download_artifact(
    url: &str,
    dest: &Path,
    verify_hash: bool,
    expected_sha: &str,
    cancel_rx: Option<&watch::Receiver<bool>>,
) -> Result<(), Status> {
    if verify_hash && expected_sha.is_empty() {
        return Err(Status::failed_precondition(
            "sha256 missing for artifact; cannot verify",
        ));
    }

    let client = Client::builder()
        .user_agent("aadk-toolchain")
        .build()
        .map_err(|e| Status::internal(format!("failed to build http client: {e}")))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| Status::unavailable(format!("download failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(Status::unavailable(format!(
            "download failed with status {}",
            resp.status()
        )));
    }

    let tmp = dest.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .map_err(|e| Status::internal(format!("failed to create temp file: {e}")))?;

    let mut hasher = if verify_hash {
        Some(Sha256::new())
    } else {
        None
    };
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        if cancel_requested(cancel_rx) {
            let _ = fs::remove_file(&tmp);
            return Err(Status::cancelled("download cancelled"));
        }
        let chunk = chunk.map_err(|e| Status::unavailable(format!("download read failed: {e}")))?;
        if let Some(h) = hasher.as_mut() {
            h.update(&chunk);
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| Status::internal(format!("failed to write temp file: {e}")))?;
    }

    file.flush()
        .await
        .map_err(|e| Status::internal(format!("failed to flush temp file: {e}")))?;
    drop(file);

    if verify_hash {
        let actual = hex_encode(&hasher.unwrap().finalize());
        if actual != expected_sha {
            let _ = fs::remove_file(&tmp);
            return Err(Status::failed_precondition("sha256 mismatch"));
        }
    }

    fs::rename(&tmp, dest)
        .map_err(|e| Status::internal(format!("failed to finalize download: {e}")))?;
    Ok(())
}

fn cache_file_name(url: &str) -> String {
    let name = url.rsplit('/').next().unwrap_or("artifact.bin");
    let name = name.split('?').next().unwrap_or(name);
    format!("{}-{}", short_hash(url), name)
}
