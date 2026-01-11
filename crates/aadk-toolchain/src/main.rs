use std::{
    fs,
    io::{self, Read},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use aadk_proto::aadk::v1::{
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    toolchain_service_server::{ToolchainService, ToolchainServiceServer},
    AvailableToolchain, CreateToolchainSetRequest, CreateToolchainSetResponse, ErrorCode, ErrorDetail,
    GetActiveToolchainSetRequest, GetActiveToolchainSetResponse, Id, InstallToolchainRequest,
    InstallToolchainResponse, InstalledToolchain, JobCompleted, JobEvent, JobFailed, JobLogAppended,
    JobProgress, JobProgressUpdated, JobState, JobStateChanged, KeyValue, ListAvailableRequest,
    ListAvailableResponse, ListInstalledRequest, ListInstalledResponse, ListProvidersRequest,
    ListProvidersResponse, ListToolchainSetsRequest, ListToolchainSetsResponse, LogChunk, PageInfo,
    PublishJobEventRequest, SetActiveToolchainSetRequest,
    SetActiveToolchainSetResponse, StartJobRequest, Timestamp, ToolchainArtifact, ToolchainKind,
    ToolchainProvider, ToolchainSet, ToolchainVersion, VerifyToolchainRequest,
    VerifyToolchainResponse,
};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, process::Command, sync::Mutex};
use tonic::{transport::Channel, Request, Response, Status};
use tracing::{info, warn};
use uuid::Uuid;

const PROVIDER_SDK_ID: &str = "provider-android-sdk-custom";
const PROVIDER_NDK_ID: &str = "provider-android-ndk-custom";
const FIXTURES_ENV_DIR: &str = "AADK_TOOLCHAIN_FIXTURES_DIR";
const SDK_VERSION: &str = "36.0.0";
const NDK_VERSION: &str = "r29";

const SDK_AARCH64_LINUX_MUSL_URL: &str =
    "https://github.com/HomuHomu833/android-sdk-custom/releases/download/36.0.0/android-sdk-aarch64-linux-musl.tar.xz";
const SDK_AARCH64_LINUX_MUSL_SHA256: &str =
    "3a79b9cb06351ccb31d7e29100b3f11a3ba4bd491c839c9bbbaa7e174d60146a";
const SDK_AARCH64_LINUX_MUSL_SIZE: u64 = 156_067_092;

const NDK_AARCH64_LINUX_MUSL_URL: &str =
    "https://github.com/HomuHomu833/android-ndk-custom/releases/download/r29/android-ndk-r29-aarch64-linux-musl.tar.xz";
const NDK_AARCH64_LINUX_MUSL_SHA256: &str =
    "1eae8941df23773f8dc70cdbf019d755f82332f0956f8062072b676f89094bc2";
const NDK_AARCH64_LINUX_MUSL_SIZE: u64 = 196_684_092;
const DEFAULT_PAGE_SIZE: usize = 25;

#[derive(Default)]
struct State {
    active_set_id: Option<String>,
    toolchain_sets: Vec<ToolchainSet>,
    installed: Vec<InstalledToolchain>,
}

#[derive(Clone)]
struct Svc {
    state: Arc<Mutex<State>>,
}

struct Provenance {
    provider_id: String,
    provider_name: String,
    version: String,
    source_url: String,
    sha256: String,
    installed_at_unix_millis: i64,
    cached_path: String,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct PersistedState {
    installed: Vec<PersistedToolchain>,
    toolchain_sets: Vec<PersistedToolchainSet>,
    active_set_id: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct PersistedToolchain {
    toolchain_id: String,
    provider_id: String,
    provider_name: String,
    provider_kind: i32,
    version: String,
    channel: String,
    notes: String,
    install_path: String,
    verified: bool,
    installed_at_unix_millis: i64,
    source_url: String,
    sha256: String,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct PersistedToolchainSet {
    toolchain_set_id: String,
    sdk_toolchain_id: Option<String>,
    ndk_toolchain_id: Option<String>,
    display_name: String,
}

impl PersistedToolchain {
    fn from_installed(item: &InstalledToolchain) -> Self {
        let provider = item.provider.clone().unwrap_or_else(|| ToolchainProvider {
            provider_id: None,
            name: "".into(),
            kind: ToolchainKind::Unspecified as i32,
            description: "".into(),
        });
        let version = item.version.clone().unwrap_or_else(|| ToolchainVersion {
            version: "".into(),
            channel: "".into(),
            notes: "".into(),
        });
        PersistedToolchain {
            toolchain_id: item.toolchain_id.as_ref().map(|i| i.value.clone()).unwrap_or_default(),
            provider_id: provider.provider_id.map(|i| i.value).unwrap_or_default(),
            provider_name: provider.name,
            provider_kind: provider.kind,
            version: version.version,
            channel: version.channel,
            notes: version.notes,
            install_path: item.install_path.clone(),
            verified: item.verified,
            installed_at_unix_millis: item.installed_at.as_ref().map(|t| t.unix_millis).unwrap_or_default(),
            source_url: item.source_url.clone(),
            sha256: item.sha256.clone(),
        }
    }

    fn into_installed(self) -> InstalledToolchain {
        InstalledToolchain {
            toolchain_id: Some(Id { value: self.toolchain_id }),
            provider: Some(ToolchainProvider {
                provider_id: Some(Id { value: self.provider_id }),
                name: self.provider_name,
                kind: self.provider_kind,
                description: "".into(),
            }),
            version: Some(ToolchainVersion {
                version: self.version,
                channel: self.channel,
                notes: self.notes,
            }),
            install_path: self.install_path,
            verified: self.verified,
            installed_at: Some(Timestamp { unix_millis: self.installed_at_unix_millis }),
            source_url: self.source_url,
            sha256: self.sha256,
        }
    }
}

impl PersistedToolchainSet {
    fn from_proto(set: &ToolchainSet) -> Option<Self> {
        let set_id = set
            .toolchain_set_id
            .as_ref()
            .map(|id| id.value.trim())
            .filter(|value| !value.is_empty())?
            .to_string();
        Some(Self {
            toolchain_set_id: set_id,
            sdk_toolchain_id: set
                .sdk_toolchain_id
                .as_ref()
                .map(|id| id.value.trim().to_string())
                .filter(|value| !value.is_empty()),
            ndk_toolchain_id: set
                .ndk_toolchain_id
                .as_ref()
                .map(|id| id.value.trim().to_string())
                .filter(|value| !value.is_empty()),
            display_name: set.display_name.clone(),
        })
    }

    fn into_proto(self) -> ToolchainSet {
        ToolchainSet {
            toolchain_set_id: Some(Id {
                value: self.toolchain_set_id,
            }),
            sdk_toolchain_id: self.sdk_toolchain_id.map(|value| Id { value }),
            ndk_toolchain_id: self.ndk_toolchain_id.map(|value| Id { value }),
            display_name: self.display_name,
        }
    }
}

impl Default for Svc {
    fn default() -> Self {
        let state = load_state();
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }
}

fn now_ts() -> Timestamp {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    Timestamp { unix_millis: ms }
}

fn provider_sdk() -> ToolchainProvider {
    ToolchainProvider {
        provider_id: Some(Id { value: PROVIDER_SDK_ID.into() }),
        name: "android-sdk-custom".into(),
        kind: ToolchainKind::Sdk as i32,
        description: "Pinned SDK provider (android-sdk-custom)".into(),
    }
}

fn provider_ndk() -> ToolchainProvider {
    ToolchainProvider {
        provider_id: Some(Id { value: PROVIDER_NDK_ID.into() }),
        name: "android-ndk-custom".into(),
        kind: ToolchainKind::Ndk as i32,
        description: "Pinned NDK provider (android-ndk-custom)".into(),
    }
}

fn data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/aadk")
    } else {
        PathBuf::from("/tmp/aadk")
    }
}

fn state_file_path() -> PathBuf {
    data_dir().join("state").join("toolchains.json")
}

fn fixtures_dir() -> Option<PathBuf> {
    std::env::var(FIXTURES_ENV_DIR).ok().map(PathBuf::from)
}

fn fixture_artifact(dir: &Path, file_name: &str) -> ToolchainArtifact {
    let path = dir.join(file_name);
    let url = format!("file://{}", path.to_string_lossy());
    let (sha256, size_bytes) = match fs::metadata(&path) {
        Ok(meta) => {
            let size = meta.len();
            let sha = match sha256_file(&path) {
                Ok(value) => value,
                Err(e) => {
                    warn!("Failed to hash fixture {}: {}", path.display(), e);
                    String::new()
                }
            };
            (sha, size)
        }
        Err(e) => {
            warn!("Fixture missing at {}: {}", path.display(), e);
            (String::new(), 0)
        }
    };

    ToolchainArtifact {
        url,
        sha256,
        size_bytes,
    }
}

fn host_key() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "aarch64") => Some("aarch64-linux-musl"),
        _ => None,
    }
}

fn provider_by_id(provider_id: &str) -> Option<ToolchainProvider> {
    match provider_id {
        PROVIDER_SDK_ID => Some(provider_sdk()),
        PROVIDER_NDK_ID => Some(provider_ndk()),
        _ => None,
    }
}

fn available_from_fixtures(provider_id: &str, dir: &Path) -> Vec<AvailableToolchain> {
    match provider_id {
        PROVIDER_SDK_ID => vec![AvailableToolchain {
            provider: Some(provider_sdk()),
            version: Some(ToolchainVersion {
                version: SDK_VERSION.into(),
                channel: "stable".into(),
                notes: "Fixture-based SDK artifact".into(),
            }),
            artifact: Some(fixture_artifact(dir, "sdk-36.0.0.tar.zst")),
        }],
        PROVIDER_NDK_ID => vec![AvailableToolchain {
            provider: Some(provider_ndk()),
            version: Some(ToolchainVersion {
                version: NDK_VERSION.into(),
                channel: "stable".into(),
                notes: "Fixture-based NDK artifact".into(),
            }),
            artifact: Some(fixture_artifact(dir, "ndk-r29.tar.zst")),
        }],
        _ => vec![],
    }
}

fn sdk_remote_artifact(host: &str) -> Option<ToolchainArtifact> {
    match host {
        "aarch64-linux-musl" => Some(ToolchainArtifact {
            url: SDK_AARCH64_LINUX_MUSL_URL.into(),
            sha256: SDK_AARCH64_LINUX_MUSL_SHA256.into(),
            size_bytes: SDK_AARCH64_LINUX_MUSL_SIZE,
        }),
        _ => None,
    }
}

fn ndk_remote_artifact(host: &str) -> Option<ToolchainArtifact> {
    match host {
        "aarch64-linux-musl" => Some(ToolchainArtifact {
            url: NDK_AARCH64_LINUX_MUSL_URL.into(),
            sha256: NDK_AARCH64_LINUX_MUSL_SHA256.into(),
            size_bytes: NDK_AARCH64_LINUX_MUSL_SIZE,
        }),
        _ => None,
    }
}

fn available_from_remote(provider_id: &str) -> Vec<AvailableToolchain> {
    let Some(host) = host_key() else {
        warn!("Host is not supported for remote toolchain artifacts.");
        return vec![];
    };

    match provider_id {
        PROVIDER_SDK_ID => sdk_remote_artifact(host)
            .map(|artifact| AvailableToolchain {
                provider: Some(provider_sdk()),
                version: Some(ToolchainVersion {
                    version: SDK_VERSION.into(),
                    channel: "stable".into(),
                    notes: format!("Remote SDK artifact for host {host}"),
                }),
                artifact: Some(artifact),
            })
            .into_iter()
            .collect(),
        PROVIDER_NDK_ID => ndk_remote_artifact(host)
            .map(|artifact| AvailableToolchain {
                provider: Some(provider_ndk()),
                version: Some(ToolchainVersion {
                    version: NDK_VERSION.into(),
                    channel: "stable".into(),
                    notes: format!("Remote NDK artifact for host {host}"),
                }),
                artifact: Some(artifact),
            })
            .into_iter()
            .collect(),
        _ => vec![],
    }
}

fn available_for_provider(provider_id: &str) -> Vec<AvailableToolchain> {
    if let Some(dir) = fixtures_dir() {
        return available_from_fixtures(provider_id, &dir);
    }
    available_from_remote(provider_id)
}

fn find_available(provider_id: &str, version: &str) -> Option<AvailableToolchain> {
    available_for_provider(provider_id)
        .into_iter()
        .find(|item| item.version.as_ref().map(|v| v.version.as_str()) == Some(version))
}

fn default_install_root() -> PathBuf {
    data_dir().join("toolchains")
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

fn job_addr() -> String {
    std::env::var("AADK_JOB_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".into())
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

async fn start_job(
    client: &mut JobServiceClient<Channel>,
    job_type: &str,
    params: Vec<KeyValue>,
) -> Result<String, Status> {
    let resp = client
        .start_job(StartJobRequest {
            job_type: job_type.into(),
            params,
            project_id: None,
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
                at: None,
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
                stream: "toolchain".into(),
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

fn local_artifact_path(url: &str) -> Option<PathBuf> {
    if url.starts_with("file://") {
        Some(PathBuf::from(url.trim_start_matches("file://")))
    } else if url.starts_with('/') {
        Some(PathBuf::from(url))
    } else {
        None
    }
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(nibble_to_hex(b >> 4));
        out.push(nibble_to_hex(b & 0x0f));
    }
    out
}

fn nibble_to_hex(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => '0',
    }
}

fn write_provenance(dir: &Path, prov: &Provenance) -> io::Result<()> {
    let mut contents = format!(
        "provider_id={}\nprovider_name={}\nversion={}\nsource_url={}\nsha256={}\ninstalled_at_unix_millis={}\n",
        prov.provider_id,
        prov.provider_name,
        prov.version,
        prov.source_url,
        prov.sha256,
        prov.installed_at_unix_millis
    );
    if !prov.cached_path.is_empty() {
        contents.push_str(&format!("cached_path={}\n", prov.cached_path));
    }
    fs::write(dir.join("provenance.txt"), contents)
}

fn load_state() -> State {
    let path = state_file_path();
    let data = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return State::default(),
        Err(e) => {
            warn!("Failed to read toolchain state {}: {}", path.display(), e);
            return State::default();
        }
    };

    let parsed: PersistedState = match serde_json::from_slice(&data) {
        Ok(state) => state,
        Err(e) => {
            warn!("Failed to parse toolchain state {}: {}", path.display(), e);
            return State::default();
        }
    };

    let installed = parsed
        .installed
        .into_iter()
        .map(PersistedToolchain::into_installed)
        .collect();
    let toolchain_sets = parsed
        .toolchain_sets
        .into_iter()
        .filter_map(|set| {
            if set.toolchain_set_id.trim().is_empty() {
                warn!("Skipping toolchain set with empty id in persisted state");
                None
            } else {
                Some(set.into_proto())
            }
        })
        .collect::<Vec<_>>();
    let mut active_set_id = parsed.active_set_id.filter(|id| !id.trim().is_empty());
    if let Some(active_id) = active_set_id.as_ref() {
        let exists = toolchain_sets.iter().any(|set| {
            set.toolchain_set_id
                .as_ref()
                .map(|id| id.value.as_str())
                == Some(active_id.as_str())
        });
        if !exists {
            warn!(
                "Active toolchain set id '{}' missing from persisted state; clearing",
                active_id
            );
            active_set_id = None;
        }
    }
    State {
        active_set_id,
        toolchain_sets,
        installed,
    }
}

fn save_state(state: &State) -> io::Result<()> {
    let persist = PersistedState {
        installed: state
            .installed
            .iter()
            .map(PersistedToolchain::from_installed)
            .collect(),
        toolchain_sets: state
            .toolchain_sets
            .iter()
            .filter_map(PersistedToolchainSet::from_proto)
            .collect(),
        active_set_id: state.active_set_id.clone(),
    };
    let payload = serde_json::to_vec_pretty(&persist)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let path = state_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    fs::write(&tmp, payload)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

fn save_state_best_effort(state: &State) {
    if let Err(e) = save_state(state) {
        warn!("Failed to persist toolchain state: {}", e);
    }
}

fn normalize_id(id: Option<Id>) -> Option<String> {
    id.map(|value| value.value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn installed_toolchain_matches(
    installed: &[InstalledToolchain],
    id: &str,
    expected: ToolchainKind,
) -> bool {
    installed.iter().any(|item| {
        let item_id = item.toolchain_id.as_ref().map(|i| i.value.as_str());
        let matches_id = item_id == Some(id);
        if !matches_id {
            return false;
        }
        let kind = item
            .provider
            .as_ref()
            .and_then(|provider| ToolchainKind::try_from(provider.kind).ok())
            .unwrap_or(ToolchainKind::Unspecified);
        matches!(
            (expected, kind),
            (ToolchainKind::Sdk, ToolchainKind::Sdk)
                | (ToolchainKind::Ndk, ToolchainKind::Ndk)
                | (_, ToolchainKind::Unspecified)
        )
    })
}

fn toolchain_set_id(set: &ToolchainSet) -> Option<&str> {
    set.toolchain_set_id.as_ref().map(|id| id.value.as_str())
}

fn download_dir() -> PathBuf {
    data_dir().join("downloads")
}

fn short_hash(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let full = hex_encode(&hasher.finalize());
    full.chars().take(12).collect()
}

fn cache_file_name(url: &str) -> String {
    let name = url.rsplit('/').next().unwrap_or("artifact.bin");
    let name = name.split('?').next().unwrap_or(name);
    format!("{}-{}", short_hash(url), name)
}

fn cached_artifact_path(url: &str) -> PathBuf {
    download_dir().join(cache_file_name(url))
}

fn is_remote_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

async fn download_artifact(url: &str, dest: &Path, verify_hash: bool, expected_sha: &str) -> Result<(), Status> {
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

    let mut hasher = if verify_hash { Some(Sha256::new()) } else { None };
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
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

async fn ensure_artifact_local(
    artifact: &ToolchainArtifact,
    verify_hash: bool,
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
                    info!("Using cached artifact {}", cache_path.display());
                    return Ok(cache_path);
                }
                let _ = fs::remove_file(&cache_path);
            } else {
                info!("Using cached artifact {}", cache_path.display());
                return Ok(cache_path);
            }
        }

        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| Status::internal(format!("failed to create download dir: {e}")))?;
        }

        info!("Downloading artifact {}", artifact.url);
        download_artifact(&artifact.url, &cache_path, verify_hash, &artifact.sha256).await?;
        info!("Saved artifact {}", cache_path.display());
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
        let actual = sha256_file(&path)
            .map_err(|e| Status::internal(format!("hashing failed: {e}")))?;
        if actual != artifact.sha256 {
            return Err(Status::failed_precondition("sha256 mismatch"));
        }
    }

    Ok(path)
}

async fn extract_archive(archive: &Path, dest: &Path) -> Result<(), Status> {
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

    info!(
        "Extracting archive {} into {}",
        archive.display(),
        dest.display()
    );

    let output = cmd
        .output()
        .await
        .map_err(|e| Status::internal(format!("failed to run tar: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(Status::internal(format!(
            "tar failed: {}\n{}\n{}",
            output.status,
            stdout.trim(),
            stderr.trim()
        )));
    }

    Ok(())
}

fn finalize_install(temp_dir: &Path, final_dir: &Path) -> Result<(), Status> {
    let entries = fs::read_dir(temp_dir)
        .map_err(|e| Status::internal(format!("failed to read install temp dir: {e}")))?;
    let mut root_dir: Option<PathBuf> = None;
    let mut extra_entries = 0usize;

    for entry in entries {
        let entry = entry.map_err(|e| Status::internal(format!("failed to read dir entry: {e}")))?;
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

fn dir_has_entries(path: &Path) -> Result<bool, io::Error> {
    let mut entries = fs::read_dir(path)?;
    Ok(entries.next().is_some())
}

fn validate_sdk_layout(root: &Path) -> Result<(), String> {
    let adb = root.join("platform-tools").join("adb");
    let adb_alt = root.join("platform-tools").join("adb.exe");
    if !adb.exists() && !adb_alt.exists() {
        return Err("missing platform-tools/adb".into());
    }

    let sdkmanager = root.join("cmdline-tools").join("latest").join("bin").join("sdkmanager");
    let sdkmanager_alt = root.join("cmdline-tools").join("bin").join("sdkmanager");
    if !sdkmanager.exists() && !sdkmanager_alt.exists() {
        return Err("missing cmdline-tools sdkmanager".into());
    }

    let build_tools = root.join("build-tools");
    if !build_tools.is_dir() {
        return Err("missing build-tools directory".into());
    }
    match dir_has_entries(&build_tools) {
        Ok(true) => {}
        Ok(false) => return Err("build-tools directory is empty".into()),
        Err(err) => return Err(format!("failed to read build-tools directory: {err}")),
    }

    Ok(())
}

fn validate_ndk_layout(root: &Path) -> Result<(), String> {
    let ndk_build = root.join("ndk-build");
    let ndk_build_alt = root.join("ndk-build.cmd");
    let ndk_build_alt2 = root.join("build").join("ndk-build");
    if !ndk_build.exists() && !ndk_build_alt.exists() && !ndk_build_alt2.exists() {
        return Err("missing ndk-build".into());
    }

    let source_props = root.join("source.properties");
    if !source_props.exists() {
        return Err("missing source.properties".into());
    }

    let prebuilt = root.join("toolchains").join("llvm").join("prebuilt");
    if !prebuilt.is_dir() {
        return Err("missing toolchains/llvm/prebuilt".into());
    }
    match dir_has_entries(&prebuilt) {
        Ok(true) => {}
        Ok(false) => return Err("toolchains/llvm/prebuilt is empty".into()),
        Err(err) => return Err(format!("failed to read toolchains/llvm/prebuilt: {err}")),
    }

    Ok(())
}

fn validate_toolchain_layout(kind: ToolchainKind, root: &Path) -> Result<(), String> {
    match kind {
        ToolchainKind::Sdk => validate_sdk_layout(root),
        ToolchainKind::Ndk => validate_ndk_layout(root),
        _ => Ok(()),
    }
}

impl Svc {
    async fn run_install_job(
        &self,
        job_id: String,
        provider_id: String,
        version: String,
        install_root: String,
        verify_hash: bool,
    ) -> Result<(), Status> {
        let mut job_client = connect_job().await?;

        if let Err(err) = publish_state(&mut job_client, &job_id, JobState::Running).await {
            warn!("Failed to publish job state: {}", err);
        }
        if let Err(err) = publish_log(&mut job_client, &job_id, "Starting toolchain install\n").await {
            warn!("Failed to publish job log: {}", err);
        }

        let available = match find_available(&provider_id, &version) {
            Some(item) => item,
            None => {
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

        let provider = match available.provider.clone().or_else(|| provider_by_id(&provider_id)) {
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

        let artifact = available
            .artifact
            .clone()
            .unwrap_or(ToolchainArtifact {
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

        if !verify_hash && !artifact.sha256.is_empty() {
            warn!("Skipping hash verification for provider {}", provider.name);
        }

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            10,
            "resolve artifact",
            vec![
                KeyValue { key: "provider".into(), value: provider.name.clone() },
                KeyValue { key: "version".into(), value: version.clone() },
            ],
        )
        .await;
        let _ = publish_log(
            &mut job_client,
            &job_id,
            &format!("Artifact URL: {}\n", artifact.url),
        )
        .await;

        let archive_path = match ensure_artifact_local(&artifact, verify_hash).await {
            Ok(path) => path,
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

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            45,
            "downloaded",
            vec![KeyValue { key: "archive_path".into(), value: archive_path.to_string_lossy().to_string() }],
        )
        .await;

        let install_root = if install_root.trim().is_empty() {
            default_install_root()
        } else {
            expand_user(&install_root)
        };

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

        let _ = publish_progress(&mut job_client, &job_id, 65, "extracting", vec![]).await;
        if let Err(err) = extract_archive(&archive_path, &temp_dir).await {
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

        let _ = publish_progress(&mut job_client, &job_id, 85, "finalizing", vec![]).await;
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
            toolchain_id: Some(Id { value: toolchain_id.clone() }),
            provider: Some(provider.clone()),
            version: available.version.clone(),
            install_path: final_dir.to_string_lossy().to_string(),
            verified: verify_hash,
            installed_at: Some(installed_at),
            source_url: artifact.url,
            sha256: artifact.sha256,
        };

        let mut st = self.state.lock().await;
        st.installed.push(installed);
        save_state_best_effort(&st);
        drop(st);

        let _ = publish_progress(&mut job_client, &job_id, 100, "completed", vec![]).await;
        let _ = publish_completed(
            &mut job_client,
            &job_id,
            "Toolchain installed",
            vec![
                KeyValue { key: "toolchain_id".into(), value: toolchain_id },
                KeyValue { key: "provider".into(), value: provider.name },
                KeyValue { key: "version".into(), value: version },
                KeyValue { key: "install_path".into(), value: final_dir.to_string_lossy().to_string() },
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
        Ok(Response::new(ListProvidersResponse {
            providers: vec![provider_sdk(), provider_ndk()],
        }))
    }

    async fn list_available(
        &self,
        request: Request<ListAvailableRequest>,
    ) -> Result<Response<ListAvailableResponse>, Status> {
        let req = request.into_inner();
        let pid = req.provider_id.and_then(|i| Some(i.value)).unwrap_or_default();

        Ok(Response::new(ListAvailableResponse {
            items: available_for_provider(&pid),
            page_info: Some(PageInfo { next_page_token: "".into() }),
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
            .cloned()
            .filter(|item| {
                if filter == ToolchainKind::Unspecified {
                    true
                } else {
                    item.provider
                        .as_ref()
                        .map(|p| p.kind == filter as i32)
                        .unwrap_or(false)
                }
            })
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
        let next_token = if end < total { end.to_string() } else { String::new() };

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
            .ok_or_else(|| Status::invalid_argument("provider_id is required"))?
            .value;
        let version = if req.version.is_empty() {
            return Err(Status::invalid_argument("version is required"));
        } else {
            req.version
        };

        if fixtures_dir().is_none() && host_key().is_none() {
            let host = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
            return Err(Status::failed_precondition(format!(
                "unsupported host for remote toolchain artifacts: {host}"
            )));
        }

        let mut job_client = connect_job().await?;
        let job_id = start_job(
            &mut job_client,
            "toolchain.install",
            vec![
                KeyValue { key: "provider_id".into(), value: provider_id.clone() },
                KeyValue { key: "version".into(), value: version.clone() },
                KeyValue { key: "verify_hash".into(), value: req.verify_hash.to_string() },
            ],
        )
        .await?;

        let svc = self.clone();
        let install_root = req.install_root;
        let verify_hash = req.verify_hash;
        let job_id_for_spawn = job_id.clone();

        tokio::spawn(async move {
            if let Err(err) = svc
                .run_install_job(job_id_for_spawn.clone(), provider_id, version, install_root, verify_hash)
                .await
            {
                warn!("install job {} failed to run: {}", job_id_for_spawn, err);
            }
        });

        Ok(Response::new(InstallToolchainResponse {
            job_id: Some(Id { value: job_id.clone() }),
        }))
    }

    async fn verify_toolchain(
        &self,
        _request: Request<VerifyToolchainRequest>,
    ) -> Result<Response<VerifyToolchainResponse>, Status> {
        let req = _request.into_inner();
        let id = req
            .toolchain_id
            .ok_or_else(|| Status::invalid_argument("toolchain_id is required"))?
            .value;

        if fixtures_dir().is_none() && host_key().is_none() {
            let host = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
            return Err(Status::failed_precondition(format!(
                "unsupported host for remote toolchain artifacts: {host}"
            )));
        }

        let mut job_client = connect_job().await?;
        let job_id = start_job(
            &mut job_client,
            "toolchain.verify",
            vec![KeyValue { key: "toolchain_id".into(), value: id.clone() }],
        )
        .await?;

        if let Err(err) = publish_state(&mut job_client, &job_id, JobState::Running).await {
            warn!("Failed to publish job state: {}", err);
        }
        if let Err(err) = publish_log(&mut job_client, &job_id, "Starting toolchain verification\n").await {
            warn!("Failed to publish job log: {}", err);
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
                job_id: Some(Id { value: job_id.clone() }),
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
                job_id: Some(Id { value: job_id.clone() }),
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
                job_id: Some(Id { value: job_id.clone() }),
            };
            save_state_best_effort(&st);
            drop(st);
            let _ = publish_failed(&mut job_client, &job_id, resp.error.clone().unwrap()).await;
            return Ok(Response::new(resp));
        }

        if entry.source_url.is_empty() {
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
                job_id: Some(Id { value: job_id.clone() }),
            };
            save_state_best_effort(&st);
            drop(st);
            let _ = publish_failed(&mut job_client, &job_id, resp.error.clone().unwrap()).await;
            return Ok(Response::new(resp));
        }

        let artifact = ToolchainArtifact {
            url: entry.source_url.clone(),
            sha256: entry.sha256.clone(),
            size_bytes: 0,
        };

        drop(st);

        let verify_result = ensure_artifact_local(&artifact, true).await;

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
                job_id: Some(Id { value: job_id.clone() }),
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

                if let Err(msg) = validate_toolchain_layout(kind, Path::new(&entry.install_path)) {
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
                        job_id: Some(Id { value: job_id.clone() }),
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
                        job_id: Some(Id { value: job_id.clone() }),
                    }
                }
            }
            Err(err) => {
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
                    job_id: Some(Id { value: job_id.clone() }),
                }
            }
        };
        save_state_best_effort(&st);
        drop(st);

        if let Some(err_detail) = publish_failure {
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
        }
        if let Some((artifact_path, install_path)) = publish_success {
            let _ = publish_progress(
                &mut job_client,
                &job_id,
                100,
                "verified",
                vec![KeyValue { key: "artifact_path".into(), value: artifact_path }],
            )
            .await;
            let _ = publish_completed(
                &mut job_client,
                &job_id,
                "Toolchain verified",
                vec![
                    KeyValue { key: "toolchain_id".into(), value: id.clone() },
                    KeyValue { key: "install_path".into(), value: install_path },
                ],
            )
            .await;
        }

        Ok(Response::new(resp))
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
        let exists = st.toolchain_sets.iter().any(|set| {
            toolchain_set_id(set) == Some(set_id.as_str())
        });
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
            .and_then(|id| st.toolchain_sets.iter().find(|set| toolchain_set_id(set) == Some(id.as_str())))
            .cloned();
        Ok(Response::new(GetActiveToolchainSetResponse {
            set: active_set,
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let addr_str = std::env::var("AADK_TOOLCHAIN_ADDR").unwrap_or_else(|_| "127.0.0.1:50052".to_string());
    let addr: SocketAddr = addr_str.parse()?;

    let svc = Svc::default();
    info!("aadk-toolchain listening on {}", addr);

    tonic::transport::Server::builder()
        .add_service(ToolchainServiceServer::new(svc))
        .serve(addr)
        .await?;

    Ok(())
}
