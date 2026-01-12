use std::{
    fs,
    io::{self, Read},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};

use aadk_proto::aadk::v1::{
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    toolchain_service_server::{ToolchainService, ToolchainServiceServer},
    AvailableToolchain, CleanupToolchainCacheRequest, CleanupToolchainCacheResponse,
    CreateToolchainSetRequest, CreateToolchainSetResponse, ErrorCode, ErrorDetail,
    GetActiveToolchainSetRequest, GetActiveToolchainSetResponse, GetJobRequest, Id,
    InstallToolchainRequest, InstallToolchainResponse, InstalledToolchain, JobCompleted, JobEvent,
    JobFailed, JobLogAppended, JobProgress, JobProgressUpdated, JobState, JobStateChanged, KeyValue,
    ListAvailableRequest, ListAvailableResponse, ListInstalledRequest, ListInstalledResponse,
    ListProvidersRequest, ListProvidersResponse, ListToolchainSetsRequest, ListToolchainSetsResponse,
    LogChunk, PageInfo, PublishJobEventRequest, SetActiveToolchainSetRequest,
    SetActiveToolchainSetResponse, StartJobRequest, StreamJobEventsRequest, Timestamp,
    ToolchainArtifact, ToolchainKind, ToolchainProvider, ToolchainSet, ToolchainVersion,
    UninstallToolchainRequest, UninstallToolchainResponse, UpdateToolchainRequest,
    UpdateToolchainResponse, VerifyToolchainRequest, VerifyToolchainResponse,
};
use base64::engine::general_purpose;
use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, process::Command, sync::{Mutex, watch}};
use tonic::{transport::Channel, Request, Response, Status};
use tracing::{info, warn};
use uuid::Uuid;

const FIXTURES_ENV_DIR: &str = "AADK_TOOLCHAIN_FIXTURES_DIR";
const CATALOG_ENV: &str = "AADK_TOOLCHAIN_CATALOG";
const HOST_OVERRIDE_ENV: &str = "AADK_TOOLCHAIN_HOST";
const HOST_FALLBACK_ENV: &str = "AADK_TOOLCHAIN_HOST_FALLBACK";
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
    catalog: Arc<Catalog>,
}

#[derive(Clone, Default)]
struct Provenance {
    provider_id: String,
    provider_name: String,
    version: String,
    source_url: String,
    sha256: String,
    installed_at_unix_millis: i64,
    cached_path: String,
    host: String,
    artifact_size_bytes: u64,
    signature: String,
    signature_url: String,
    signature_public_key: String,
}

#[derive(Clone, Default)]
struct SignatureRecord {
    signature: String,
    signature_url: String,
    public_key: String,
}

#[derive(Clone, Deserialize)]
#[serde(default)]
struct Catalog {
    schema_version: u32,
    providers: Vec<CatalogProvider>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct CatalogProvider {
    provider_id: String,
    name: String,
    kind: String,
    description: String,
    versions: Vec<CatalogVersion>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct CatalogVersion {
    version: String,
    channel: String,
    notes: String,
    artifacts: Vec<CatalogArtifact>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct CatalogArtifact {
    host: String,
    url: String,
    sha256: String,
    size_bytes: u64,
    signature: String,
    signature_url: String,
    signature_public_key: String,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            schema_version: 1,
            providers: Vec::new(),
        }
    }
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
            catalog: Arc::new(load_catalog()),
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

fn default_catalog() -> Catalog {
    let raw = include_str!("../catalog.json");
    match serde_json::from_str::<Catalog>(raw) {
        Ok(catalog) => catalog,
        Err(err) => {
            warn!("Failed to parse default catalog: {err}");
            Catalog::default()
        }
    }
}

fn load_catalog() -> Catalog {
    if let Ok(path) = std::env::var(CATALOG_ENV) {
        let path = PathBuf::from(path);
        match fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<Catalog>(&raw) {
                Ok(catalog) => return catalog,
                Err(err) => warn!("Failed to parse catalog {}: {}", path.display(), err),
            },
            Err(err) => warn!("Failed to read catalog {}: {}", path.display(), err),
        }
    }
    default_catalog()
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

fn fixture_filename(kind: ToolchainKind, version: &str) -> String {
    match kind {
        ToolchainKind::Sdk => format!("sdk-{version}.tar.zst"),
        ToolchainKind::Ndk => format!("ndk-{version}.tar.zst"),
        _ => format!("toolchain-{version}.tar.zst"),
    }
}

fn fixture_artifact_for(dir: &Path, kind: ToolchainKind, version: &str) -> ToolchainArtifact {
    let file = fixture_filename(kind, version);
    fixture_artifact(dir, &file)
}

fn host_key() -> Option<String> {
    if let Ok(override_key) = std::env::var(HOST_OVERRIDE_ENV) {
        if !override_key.trim().is_empty() {
            return Some(override_key);
        }
    }
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "aarch64") => Some("linux-aarch64".into()),
        ("linux", "x86_64") => Some("linux-x86_64".into()),
        ("macos", "aarch64") => Some("darwin-aarch64".into()),
        ("macos", "x86_64") => Some("darwin-x86_64".into()),
        ("windows", "x86_64") => Some("windows-x86_64".into()),
        ("windows", "aarch64") => Some("windows-aarch64".into()),
        _ => None,
    }
}

fn host_aliases(host: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    match host {
        "linux-aarch64" => aliases.push("aarch64-linux-musl".into()),
        "linux-x86_64" => aliases.push("x86_64-linux-gnu".into()),
        "darwin-aarch64" => aliases.push("macos-aarch64".into()),
        "darwin-x86_64" => aliases.push("macos-x86_64".into()),
        _ => {}
    }
    aliases
}

fn fallback_hosts() -> Vec<String> {
    match std::env::var(HOST_FALLBACK_ENV) {
        Ok(value) => value
            .split(',')
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect(),
        Err(_) => vec![],
    }
}

fn host_candidates(host: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    candidates.push(host.to_string());
    candidates.extend(host_aliases(host));
    candidates.extend(fallback_hosts());
    candidates
}

fn parse_kind(kind: &str) -> ToolchainKind {
    match kind.to_lowercase().as_str() {
        "sdk" => ToolchainKind::Sdk,
        "ndk" => ToolchainKind::Ndk,
        _ => ToolchainKind::Unspecified,
    }
}

fn provider_from_catalog(provider: &CatalogProvider) -> ToolchainProvider {
    ToolchainProvider {
        provider_id: Some(Id { value: provider.provider_id.clone() }),
        name: provider.name.clone(),
        kind: parse_kind(&provider.kind) as i32,
        description: provider.description.clone(),
    }
}

fn version_from_catalog(version: &CatalogVersion) -> ToolchainVersion {
    ToolchainVersion {
        version: version.version.clone(),
        channel: version.channel.clone(),
        notes: version.notes.clone(),
    }
}

fn artifact_for_host(
    version: &CatalogVersion,
    host_candidates: &[String],
) -> Option<ToolchainArtifact> {
    for host in host_candidates {
        if let Some(artifact) = version
            .artifacts
            .iter()
            .find(|item| item.host == *host && !item.url.trim().is_empty())
        {
            return Some(ToolchainArtifact {
                url: artifact.url.clone(),
                sha256: artifact.sha256.clone(),
                size_bytes: artifact.size_bytes,
            });
        }
    }
    if let Some(artifact) = version
        .artifacts
        .iter()
        .find(|item| item.host == "any" && !item.url.trim().is_empty())
    {
        return Some(ToolchainArtifact {
            url: artifact.url.clone(),
            sha256: artifact.sha256.clone(),
            size_bytes: artifact.size_bytes,
        });
    }
    None
}

fn available_for_provider(
    catalog: &Catalog,
    provider_id: &str,
    host: &str,
    fixtures: Option<&Path>,
) -> Vec<AvailableToolchain> {
    let Some(provider) = catalog
        .providers
        .iter()
        .find(|item| item.provider_id == provider_id)
    else {
        return vec![];
    };
    let provider_proto = provider_from_catalog(provider);
    let kind = parse_kind(&provider.kind);
    let candidates = host_candidates(host);

    provider
        .versions
        .iter()
        .filter_map(|version| {
            let artifact = if let Some(dir) = fixtures {
                Some(fixture_artifact_for(dir, kind, &version.version))
            } else {
                artifact_for_host(version, &candidates)
            }?;
            Some(AvailableToolchain {
                provider: Some(provider_proto.clone()),
                version: Some(version_from_catalog(version)),
                artifact: Some(artifact),
            })
        })
        .collect()
}

fn find_available(
    catalog: &Catalog,
    provider_id: &str,
    version: &str,
    host: &str,
    fixtures: Option<&Path>,
) -> Option<AvailableToolchain> {
    available_for_provider(catalog, provider_id, host, fixtures)
        .into_iter()
        .find(|item| item.version.as_ref().map(|v| v.version.as_str()) == Some(version))
}

fn catalog_version_hosts(
    catalog: &Catalog,
    provider_id: &str,
    version: &str,
) -> Option<Vec<String>> {
    let provider = catalog
        .providers
        .iter()
        .find(|item| item.provider_id == provider_id)?;
    let entry = provider.versions.iter().find(|item| item.version == version)?;
    let mut hosts = entry
        .artifacts
        .iter()
        .map(|item| item.host.clone())
        .collect::<Vec<_>>();
    hosts.sort();
    hosts.dedup();
    Some(hosts)
}

fn default_install_root() -> PathBuf {
    data_dir().join("toolchains")
}

fn install_root_for_entry(entry: &InstalledToolchain) -> PathBuf {
    let install_path = Path::new(&entry.install_path);
    if let Some(provider_dir) = install_path.parent() {
        if let Some(root) = provider_dir.parent() {
            return root.to_path_buf();
        }
    }
    default_install_root()
}

fn provider_id_from_installed(entry: &InstalledToolchain) -> Option<String> {
    entry
        .provider
        .as_ref()
        .and_then(|provider| provider.provider_id.as_ref().map(|id| id.value.clone()))
        .filter(|value| !value.trim().is_empty())
}

fn provider_name_from_installed(entry: &InstalledToolchain) -> Option<String> {
    entry
        .provider
        .as_ref()
        .map(|provider| provider.name.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn version_from_installed(entry: &InstalledToolchain) -> Option<String> {
    entry
        .version
        .as_ref()
        .map(|version| version.version.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn kind_from_installed(entry: &InstalledToolchain) -> ToolchainKind {
    entry
        .provider
        .as_ref()
        .and_then(|provider| ToolchainKind::try_from(provider.kind).ok())
        .unwrap_or(ToolchainKind::Unspecified)
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

async fn job_is_cancelled(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
) -> bool {
    let resp = client
        .get_job(GetJobRequest {
            job_id: Some(Id { value: job_id.to_string() }),
        })
        .await;
    let job = match resp {
        Ok(resp) => resp.into_inner().job,
        Err(_) => return false,
    };
    match job.and_then(|job| JobState::try_from(job.state).ok()) {
        Some(JobState::Cancelled) => true,
        _ => false,
    }
}

async fn spawn_cancel_watcher(job_id: String) -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);
    let mut client = match connect_job().await {
        Ok(client) => client,
        Err(err) => {
            warn!("cancel watcher: failed to connect job service: {err}");
            return rx;
        }
    };
    let mut stream = match client
        .stream_job_events(StreamJobEventsRequest {
            job_id: Some(Id { value: job_id.clone() }),
            include_history: true,
        })
        .await
    {
        Ok(resp) => resp.into_inner(),
        Err(err) => {
            warn!("cancel watcher: stream failed for {job_id}: {err}");
            return rx;
        }
    };

    tokio::spawn(async move {
        loop {
            match stream.message().await {
                Ok(Some(evt)) => {
                    if let Some(JobPayload::StateChanged(state)) = evt.payload {
                        if JobState::try_from(state.new_state)
                            .unwrap_or(JobState::Unspecified)
                            == JobState::Cancelled
                        {
                            let _ = tx.send(true);
                            break;
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
    });

    rx
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

fn metric(key: &str, value: impl ToString) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: value.to_string(),
    }
}

fn toolchain_base_metrics(
    provider_id: &str,
    provider_name: &str,
    version: &str,
    verify_hash: bool,
    host: &str,
    artifact: Option<&ToolchainArtifact>,
) -> Vec<KeyValue> {
    let mut metrics = vec![
        metric("provider_id", provider_id),
        metric("provider", provider_name),
        metric("version", version),
        metric("verify_hash", verify_hash),
        metric("host", host),
    ];

    if let Some(artifact) = artifact {
        if !artifact.url.trim().is_empty() {
            metrics.push(metric("artifact_url", &artifact.url));
        }
        if !artifact.sha256.trim().is_empty() {
            metrics.push(metric("artifact_sha256", &artifact.sha256));
        }
        if artifact.size_bytes > 0 {
            metrics.push(metric("artifact_size_bytes", artifact.size_bytes));
        }
    }

    metrics
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

fn sha256_file_bytes(path: &Path) -> io::Result<[u8; 32]> {
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
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
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

fn normalize_signature_value(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    let value = value.trim().trim_start_matches("0x");
    if value.len() % 2 != 0 {
        return Err("hex value length must be even".into());
    }
    let mut out = Vec::with_capacity(value.len() / 2);
    let bytes = value.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        let high = hex_value(bytes[idx]).ok_or_else(|| "invalid hex character".to_string())?;
        let low = hex_value(bytes[idx + 1]).ok_or_else(|| "invalid hex character".to_string())?;
        out.push((high << 4) | low);
        idx += 2;
    }
    Ok(out)
}

fn decode_signature_field(value: &str) -> Result<Vec<u8>, String> {
    let normalized = normalize_signature_value(value);
    if normalized.is_empty() {
        return Err("signature field is empty".into());
    }
    if let Ok(bytes) = decode_hex(&normalized) {
        return Ok(bytes);
    }
    general_purpose::STANDARD
        .decode(normalized.as_bytes())
        .map_err(|_| "signature field is not valid hex or base64".to_string())
}

fn verify_ed25519_signature(
    artifact_path: &Path,
    signature_value: &str,
    public_key_value: &str,
) -> Result<(), String> {
    let signature_bytes = decode_signature_field(signature_value)?;
    let public_key_bytes = decode_signature_field(public_key_value)?;

    let signature: [u8; 64] = signature_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "signature must be 64 bytes".to_string())?;
    let public_key: [u8; 32] = public_key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "public key must be 32 bytes".to_string())?;

    let message = sha256_file_bytes(artifact_path)
        .map_err(|e| format!("failed to hash artifact: {e}"))?;
    let key = VerifyingKey::from_bytes(&public_key)
        .map_err(|e| format!("invalid public key: {e}"))?;
    let signature = Signature::from_bytes(&signature);

    key.verify_strict(&message, &signature)
        .map_err(|e| format!("signature verification failed: {e}"))
}

fn cancel_requested(cancel_rx: Option<&watch::Receiver<bool>>) -> bool {
    cancel_rx.map(|rx| *rx.borrow()).unwrap_or(false)
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
    if !prov.host.is_empty() {
        contents.push_str(&format!("host={}\n", prov.host));
    }
    if prov.artifact_size_bytes > 0 {
        contents.push_str(&format!(
            "artifact_size_bytes={}\n",
            prov.artifact_size_bytes
        ));
    }
    if !prov.signature.is_empty() {
        contents.push_str(&format!("signature={}\n", prov.signature));
    }
    if !prov.signature_url.is_empty() {
        contents.push_str(&format!("signature_url={}\n", prov.signature_url));
    }
    if !prov.signature_public_key.is_empty() {
        contents.push_str(&format!("signature_public_key={}\n", prov.signature_public_key));
    }
    fs::write(dir.join("provenance.txt"), contents)
}

fn parse_provenance(contents: &str) -> Provenance {
    let mut prov = Provenance::default();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "provider_id" => prov.provider_id = value.to_string(),
            "provider_name" => prov.provider_name = value.to_string(),
            "version" => prov.version = value.to_string(),
            "source_url" => prov.source_url = value.to_string(),
            "sha256" => prov.sha256 = value.to_string(),
            "installed_at_unix_millis" => {
                if let Ok(parsed) = value.parse::<i64>() {
                    prov.installed_at_unix_millis = parsed;
                }
            }
            "cached_path" => prov.cached_path = value.to_string(),
            "host" => prov.host = value.to_string(),
            "artifact_size_bytes" => {
                if let Ok(parsed) = value.parse::<u64>() {
                    prov.artifact_size_bytes = parsed;
                }
            }
            "signature" => prov.signature = value.to_string(),
            "signature_url" => prov.signature_url = value.to_string(),
            "signature_public_key" => prov.signature_public_key = value.to_string(),
            _ => {}
        }
    }
    prov
}

fn read_provenance(path: &Path) -> io::Result<Provenance> {
    let contents = fs::read_to_string(path)?;
    Ok(parse_provenance(&contents))
}

fn catalog_artifact_for_provenance(
    catalog: &Catalog,
    provider_id: &str,
    version: &str,
    url: &str,
    sha256: &str,
) -> Option<CatalogArtifact> {
    let provider = catalog
        .providers
        .iter()
        .find(|item| item.provider_id == provider_id)?;
    let version_entry = provider.versions.iter().find(|item| item.version == version)?;
    if let Some(artifact) = version_entry.artifacts.iter().find(|item| item.url == url) {
        return Some(artifact.clone());
    }
    if !sha256.is_empty() {
        if let Some(artifact) = version_entry
            .artifacts
            .iter()
            .find(|item| item.sha256 == sha256)
        {
            return Some(artifact.clone());
        }
    }
    None
}

fn signature_record_from_catalog(artifact: &CatalogArtifact) -> Option<SignatureRecord> {
    let signature = artifact.signature.trim().to_string();
    let signature_url = artifact.signature_url.trim().to_string();
    let public_key = artifact.signature_public_key.trim().to_string();
    if signature.is_empty() && signature_url.is_empty() && public_key.is_empty() {
        return None;
    }
    Some(SignatureRecord {
        signature,
        signature_url,
        public_key,
    })
}

fn signature_record_from_provenance(prov: &Provenance) -> Option<SignatureRecord> {
    let signature = prov.signature.trim().to_string();
    let signature_url = prov.signature_url.trim().to_string();
    let public_key = prov.signature_public_key.trim().to_string();
    if signature.is_empty() && signature_url.is_empty() && public_key.is_empty() {
        return None;
    }
    Some(SignatureRecord {
        signature,
        signature_url,
        public_key,
    })
}

async fn fetch_signature_value(signature_url: &str) -> Result<String, String> {
    if signature_url.trim().is_empty() {
        return Err("signature url is empty".into());
    }
    if is_remote_url(signature_url) {
        let client = Client::builder()
            .user_agent("aadk-toolchain")
            .build()
            .map_err(|e| format!("failed to build http client: {e}"))?;
        let resp = client
            .get(signature_url)
            .send()
            .await
            .map_err(|e| format!("signature download failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "signature download failed with status {}",
                resp.status()
            ));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| format!("signature download read failed: {e}"))?;
        return Ok(body);
    }
    let path = local_artifact_path(signature_url)
        .ok_or_else(|| "signature url is not a local file".to_string())?;
    fs::read_to_string(&path)
        .map_err(|e| format!("failed to read signature file {}: {e}", path.display()))
}

async fn resolve_signature_value(record: &SignatureRecord) -> Result<String, String> {
    if !record.signature.trim().is_empty() {
        return Ok(normalize_signature_value(&record.signature));
    }
    if record.signature_url.trim().is_empty() {
        return Err("signature is missing".into());
    }
    let raw = fetch_signature_value(&record.signature_url).await?;
    let normalized = normalize_signature_value(&raw);
    if normalized.is_empty() {
        return Err("signature content empty".into());
    }
    Ok(normalized)
}

async fn verify_signature_if_configured(
    artifact_path: &Path,
    record: Option<&SignatureRecord>,
) -> Result<Option<String>, String> {
    let Some(record) = record else {
        return Ok(None);
    };
    if record.public_key.trim().is_empty() {
        return Err("signature public key missing".into());
    }
    let signature_value = resolve_signature_value(record).await?;
    verify_ed25519_signature(artifact_path, &signature_value, &record.public_key)?;
    Ok(Some(signature_value))
}

fn verify_provenance_entry(
    entry: &InstalledToolchain,
    prov: &Provenance,
    catalog: &Catalog,
) -> Result<(), String> {
    if prov.provider_id.trim().is_empty() {
        return Err("provenance missing provider_id".into());
    }
    let entry_provider_id = provider_id_from_installed(entry)
        .ok_or_else(|| "installed entry missing provider_id".to_string())?;
    if prov.provider_id != entry_provider_id {
        return Err(format!(
            "provenance provider_id mismatch (expected {entry_provider_id}, got {})",
            prov.provider_id
        ));
    }

    if prov.version.trim().is_empty() {
        return Err("provenance missing version".into());
    }
    let entry_version =
        version_from_installed(entry).ok_or_else(|| "installed entry missing version".to_string())?;
    if prov.version != entry_version {
        return Err(format!(
            "provenance version mismatch (expected {entry_version}, got {})",
            prov.version
        ));
    }

    if prov.source_url.trim().is_empty() {
        return Err("provenance missing source_url".into());
    }
    if !entry.source_url.trim().is_empty() && prov.source_url != entry.source_url {
        return Err("provenance source_url mismatch".into());
    }

    if prov.sha256.trim().is_empty() && !entry.sha256.trim().is_empty() {
        return Err("provenance missing sha256".into());
    }
    if !prov.sha256.trim().is_empty() && !entry.sha256.trim().is_empty() && prov.sha256 != entry.sha256 {
        return Err("provenance sha256 mismatch".into());
    }

    if !prov.provider_name.trim().is_empty() {
        if let Some(entry_name) = provider_name_from_installed(entry) {
            if prov.provider_name != entry_name {
                return Err("provenance provider_name mismatch".into());
            }
        }
    }

    if is_remote_url(&prov.source_url) {
        let artifact = catalog_artifact_for_provenance(
            catalog,
            &entry_provider_id,
            &entry_version,
            &prov.source_url,
            &prov.sha256,
        )
        .ok_or_else(|| "provenance source_url not found in catalog".to_string())?;
        if !artifact.sha256.is_empty() && !prov.sha256.is_empty() && artifact.sha256 != prov.sha256
        {
            return Err("catalog sha256 mismatch".into());
        }
        if prov.artifact_size_bytes > 0
            && artifact.size_bytes > 0
            && prov.artifact_size_bytes != artifact.size_bytes
        {
            return Err("catalog size_bytes mismatch".into());
        }
    }

    Ok(())
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

fn toolchain_referenced_by_sets(sets: &[ToolchainSet], toolchain_id: &str) -> bool {
    sets.iter().any(|set| {
        set.sdk_toolchain_id
            .as_ref()
            .map(|id| id.value.as_str())
            == Some(toolchain_id)
            || set
                .ndk_toolchain_id
                .as_ref()
                .map(|id| id.value.as_str())
                == Some(toolchain_id)
    })
}

fn scrub_toolchain_from_sets(sets: &mut Vec<ToolchainSet>, toolchain_id: &str) -> Vec<String> {
    let mut removed_sets = Vec::new();
    for set in sets.iter_mut() {
        if set
            .sdk_toolchain_id
            .as_ref()
            .map(|id| id.value.as_str())
            == Some(toolchain_id)
        {
            set.sdk_toolchain_id = None;
        }
        if set
            .ndk_toolchain_id
            .as_ref()
            .map(|id| id.value.as_str())
            == Some(toolchain_id)
        {
            set.ndk_toolchain_id = None;
        }
    }

    sets.retain(|set| {
        let has_any = set.sdk_toolchain_id.is_some() || set.ndk_toolchain_id.is_some();
        if !has_any {
            if let Some(set_id) = toolchain_set_id(set) {
                removed_sets.push(set_id.to_string());
            }
        }
        has_any
    });

    removed_sets
}

fn replace_toolchain_in_sets(
    sets: &mut Vec<ToolchainSet>,
    old_id: &str,
    new_id: &str,
) -> usize {
    let mut updated = 0usize;
    for set in sets {
        if set
            .sdk_toolchain_id
            .as_ref()
            .map(|id| id.value.as_str())
            == Some(old_id)
        {
            set.sdk_toolchain_id = Some(Id { value: new_id.to_string() });
            updated += 1;
        }
        if set
            .ndk_toolchain_id
            .as_ref()
            .map(|id| id.value.as_str())
            == Some(old_id)
        {
            set.ndk_toolchain_id = Some(Id { value: new_id.to_string() });
            updated += 1;
        }
    }
    updated
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

fn cached_path_for_entry(entry: &InstalledToolchain) -> Option<PathBuf> {
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

fn cached_paths_for_installed(installed: &[InstalledToolchain]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in installed {
        if let Some(path) = cached_path_for_entry(entry) {
            paths.push(path);
        }
    }
    paths
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

    let mut hasher = if verify_hash { Some(Sha256::new()) } else { None };
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

async fn ensure_artifact_local(
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
        download_artifact(
            &artifact.url,
            &cache_path,
            verify_hash,
            &artifact.sha256,
            cancel_rx,
        )
        .await?;
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

async fn extract_archive(
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

    info!(
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

    let stdout = match stdout_task.await {
        Ok(buf) => buf,
        Err(_) => Vec::new(),
    };
    let stderr = match stderr_task.await {
        Ok(buf) => buf,
        Err(_) => Vec::new(),
    };

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
        if let Err(err) = publish_log(&mut job_client, &job_id, "Starting toolchain install\n").await {
            warn!("Failed to publish job log: {}", err);
        }

        if cancel_requested(Some(&cancel_rx)) {
            let _ = publish_log(&mut job_client, &job_id, "Install cancelled\n").await;
            return Ok(());
        }

        let available = match find_available(&self.catalog, &provider_id, &version, &host, fixtures.as_deref()) {
            Some(item) => item,
            None => {
                if fixtures.is_none() {
                    if let Some(hosts) = catalog_version_hosts(&self.catalog, &provider_id, &version) {
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

        let signature_record = catalog_artifact_for_provenance(
            &self.catalog,
            &provider_id,
            &version,
            &artifact.url,
            &artifact.sha256,
        )
        .and_then(|artifact| signature_record_from_catalog(&artifact));

        if !verify_hash && !artifact.sha256.is_empty() {
            warn!("Skipping hash verification for provider {}", provider.name);
        }
        if !verify_hash && signature_record.is_some() {
            warn!("Skipping signature verification for provider {}", provider.name);
        }

        let install_root = if install_root.trim().is_empty() {
            default_install_root()
        } else {
            expand_user(&install_root)
        };
        let provider_kind = ToolchainKind::try_from(provider.kind).unwrap_or(ToolchainKind::Unspecified);

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            10,
            "resolve artifact",
            {
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
            },
        )
        .await;
        let _ = publish_log(
            &mut job_client,
            &job_id,
            &format!("Artifact URL: {}\n", artifact.url),
        )
        .await;

        let archive_path = match ensure_artifact_local(&artifact, verify_hash, Some(&cancel_rx)).await {
            Ok(path) => path,
            Err(err) if err.code() == tonic::Code::Cancelled => {
                let _ = publish_log(&mut job_client, &job_id, "Install cancelled during download\n").await;
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

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            45,
            "downloaded",
            {
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
            },
        )
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

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            65,
            "extracting",
            {
                let mut metrics = toolchain_base_metrics(
                    &provider_id,
                    &provider.name,
                    &version,
                    verify_hash,
                    &host,
                    Some(&artifact),
                );
                metrics.push(metric("archive_path", archive_path.to_string_lossy().to_string()));
                metrics.push(metric("install_root", install_root.display()));
                metrics
            },
        )
        .await;
        if let Err(err) = extract_archive(&archive_path, &temp_dir, Some(&cancel_rx)).await {
            if err.code() == tonic::Code::Cancelled {
                let _ = publish_log(&mut job_client, &job_id, "Install cancelled during extract\n").await;
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

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            85,
            "finalizing",
            {
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
            },
        )
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

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            100,
            "completed",
            {
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
            },
        )
        .await;
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
            let _ = publish_log(&mut job_client, &job_id, "Uninstall cancelled before start\n").await;
            return Ok(());
        }

        let _ = publish_state(&mut job_client, &job_id, JobState::Running).await;
        let _ = publish_log(&mut job_client, &job_id, "Starting toolchain uninstall\n").await;

        let (entry, cached_path, keep_paths, early_error) = {
            let st = self.state.lock().await;
            let entry = st
                .installed
                .iter()
                .find(|item| item.toolchain_id.as_ref().map(|id| id.value.as_str()) == Some(toolchain_id.as_str()))
                .cloned();
            if entry.is_none() {
                let err_detail = job_error_detail(
                    ErrorCode::NotFound,
                    "toolchain not found",
                    format!("toolchain_id={toolchain_id}"),
                    &job_id,
                );
                (None, None, Vec::new(), Some(err_detail))
            } else if toolchain_referenced_by_sets(&st.toolchain_sets, &toolchain_id) && !force {
                let err_detail = job_error_detail(
                    ErrorCode::InvalidArgument,
                    "toolchain is referenced by a toolchain set",
                    "use force to remove references".into(),
                    &job_id,
                );
                (None, None, Vec::new(), Some(err_detail))
            } else {
                let entry = entry.unwrap();
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
                            &format!("Failed to remove cached artifact {}: {err}\n", path.display()),
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
        st.installed.retain(|item| item.toolchain_id.as_ref().map(|id| id.value.as_str()) != Some(toolchain_id.as_str()));
        save_state_best_effort(&st);
        drop(st);

        let mut outputs = vec![
            KeyValue { key: "toolchain_id".into(), value: toolchain_id.clone() },
            KeyValue { key: "install_path".into(), value: entry.install_path.clone() },
        ];
        if cached_removed {
            outputs.push(KeyValue { key: "cached_artifact_removed".into(), value: "true".into() });
        } else if cached_skipped {
            outputs.push(KeyValue { key: "cached_artifact_removed".into(), value: "false".into() });
            outputs.push(KeyValue { key: "cached_artifact_in_use".into(), value: "true".into() });
        }

        let _ = publish_completed(
            &mut job_client,
            &job_id,
            "Toolchain uninstalled",
            outputs,
        )
        .await;

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
                .find(|item| item.toolchain_id.as_ref().map(|id| id.value.as_str()) == Some(toolchain_id.as_str()))
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

        let available = match find_available(&self.catalog, &provider_id, &target_version, &host, fixtures.as_deref()) {
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
                ErrorCode::ToolchainUpdateFailed,
                "artifact url missing",
                "".into(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, err_detail).await;
            return Ok(());
        }

        let signature_record = catalog_artifact_for_provenance(
            &self.catalog,
            &provider_id,
            &target_version,
            &artifact.url,
            &artifact.sha256,
        )
        .and_then(|artifact| signature_record_from_catalog(&artifact));
        if !verify_hash && signature_record.is_some() {
            warn!("Skipping signature verification for provider {}", provider.name);
        }

        let provider_kind = ToolchainKind::try_from(provider.kind).unwrap_or(ToolchainKind::Unspecified);
        let _ = publish_progress(
            &mut job_client,
            &job_id,
            10,
            "resolve artifact",
            {
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
            },
        )
        .await;

        let archive_path = match ensure_artifact_local(&artifact, verify_hash, Some(&cancel_rx)).await {
            Ok(path) => path,
            Err(err) if err.code() == tonic::Code::Cancelled => {
                let _ = publish_log(&mut job_client, &job_id, "Update cancelled during download\n").await;
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

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            45,
            "downloaded",
            {
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
                metrics.push(metric("archive_path", archive_path.to_string_lossy().to_string()));
                metrics.push(metric("install_root", install_root.display()));
                metrics
            },
        )
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

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            65,
            "extracting",
            {
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
                metrics.push(metric("archive_path", archive_path.to_string_lossy().to_string()));
                metrics.push(metric("install_root", install_root.display()));
                metrics
            },
        )
        .await;
        if let Err(err) = extract_archive(&archive_path, &temp_dir, Some(&cancel_rx)).await {
            if err.code() == tonic::Code::Cancelled {
                let _ = publish_log(&mut job_client, &job_id, "Update cancelled during extract\n").await;
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

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            85,
            "finalizing",
            {
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
            },
        )
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
            toolchain_id: Some(Id { value: new_toolchain_id.clone() }),
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
                        &format!("Failed to remove old install path {}: {err}\n", old_path.display()),
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
                    .filter(|item| item.toolchain_id.as_ref().map(|id| id.value.as_str()) != Some(toolchain_id.as_str()))
                    .filter_map(cached_path_for_entry)
                    .collect::<Vec<_>>()
            };
            if let Some(path) = cached_path {
                if !keep_paths.iter().any(|keep| keep == &path) && path.exists() {
                    if fs::remove_file(&path).is_ok() {
                        cached_removed = true;
                    }
                }
            }
        }

        let mut st = self.state.lock().await;
        st.installed.push(installed);
        replace_toolchain_in_sets(&mut st.toolchain_sets, &toolchain_id, &new_toolchain_id);
        st.installed.retain(|item| item.toolchain_id.as_ref().map(|id| id.value.as_str()) != Some(toolchain_id.as_str()));
        save_state_best_effort(&st);
        drop(st);

        let mut outputs = vec![
            KeyValue { key: "old_toolchain_id".into(), value: toolchain_id.clone() },
            KeyValue { key: "new_toolchain_id".into(), value: new_toolchain_id.clone() },
            KeyValue { key: "old_version".into(), value: current_version },
            KeyValue { key: "new_version".into(), value: target_version.clone() },
            KeyValue { key: "install_path".into(), value: final_dir.to_string_lossy().to_string() },
        ];
        if removed_old_install {
            outputs.push(KeyValue { key: "old_install_removed".into(), value: "true".into() });
        }
        if cached_removed {
            outputs.push(KeyValue { key: "cached_artifact_removed".into(), value: "true".into() });
        }

        let _ = publish_completed(
            &mut job_client,
            &job_id,
            "Toolchain updated",
            outputs,
        )
        .await;

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
        let _ = publish_log(&mut job_client, &job_id, "Starting toolchain cache cleanup\n").await;

        let download_dir = download_dir();
        if !download_dir.exists() {
            let _ = publish_completed(
                &mut job_client,
                &job_id,
                "Toolchain cache cleanup complete",
                vec![
                    KeyValue { key: "removed_count".into(), value: "0".into() },
                    KeyValue { key: "removed_bytes".into(), value: "0".into() },
                    KeyValue { key: "dry_run".into(), value: dry_run.to_string() },
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
                KeyValue { key: "removed_count".into(), value: removed_count.to_string() },
                KeyValue { key: "removed_bytes".into(), value: removed_bytes.to_string() },
                KeyValue { key: "scanned".into(), value: scanned.to_string() },
                KeyValue { key: "dry_run".into(), value: dry_run.to_string() },
                KeyValue { key: "remove_all".into(), value: remove_all.to_string() },
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
        Ok(Response::new(ListProvidersResponse {
            providers,
        }))
    }

    async fn list_available(
        &self,
        request: Request<ListAvailableRequest>,
    ) -> Result<Response<ListAvailableResponse>, Status> {
        let req = request.into_inner();
        let pid = req.provider_id.and_then(|i| Some(i.value)).unwrap_or_default();
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
            .unwrap_or_else(String::new);
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "toolchain.install",
                vec![
                    KeyValue { key: "provider_id".into(), value: provider_id.clone() },
                    KeyValue { key: "version".into(), value: version.clone() },
                    KeyValue { key: "verify_hash".into(), value: req.verify_hash.to_string() },
                ],
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
            .unwrap_or_else(String::new);
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "toolchain.verify",
                vec![KeyValue { key: "toolchain_id".into(), value: id.clone() }],
            )
            .await?
        } else {
            job_id
        };

        let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;
        if job_is_cancelled(&mut job_client, &job_id).await {
            let _ = publish_log(&mut job_client, &job_id, "Verification cancelled before start\n").await;
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
        if let Err(err) = publish_log(&mut job_client, &job_id, "Starting toolchain verification\n").await {
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
                    job_id: Some(Id { value: job_id.clone() }),
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
                job_id: Some(Id { value: job_id.clone() }),
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
                job_id: Some(Id { value: job_id.clone() }),
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
            Ok(local_path) => verify_signature_if_configured(local_path, signature_record.as_ref()).await,
            Err(_) => Ok(None),
        };
        let signature_error = signature_result
            .as_ref()
            .err()
            .map(|err| job_error_detail(
                ErrorCode::ToolchainVerifyFailed,
                "signature verification failed",
                err.clone(),
                &job_id,
            ));

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

                if let Some(err_detail) = signature_error.clone() {
                    entry.verified = false;
                    publish_failure = Some(err_detail.clone());
                    VerifyToolchainResponse {
                        verified: false,
                        error: Some(err_detail),
                        job_id: Some(Id { value: job_id.clone() }),
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
                            job_id: Some(Id { value: job_id.clone() }),
                        }
                    } else if let Err(msg) = validate_toolchain_layout(kind, Path::new(&entry.install_path)) {
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
                        job_id: Some(Id { value: job_id.clone() }),
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
                        job_id: Some(Id { value: job_id.clone() }),
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
            let _ = publish_progress(
                &mut job_client,
                &job_id,
                100,
                "verified",
                vec![
                    metric("toolchain_id", &id),
                    metric("artifact_path", &artifact_path),
                    metric("install_path", &install_path),
                    metric("provider_id", &provenance.provider_id),
                    metric("version", &provenance.version),
                ],
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
            .unwrap_or_else(String::new);
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "toolchain.update",
                vec![
                    KeyValue { key: "toolchain_id".into(), value: toolchain_id.clone() },
                    KeyValue { key: "version".into(), value: req.version.clone() },
                    KeyValue { key: "verify_hash".into(), value: req.verify_hash.to_string() },
                    KeyValue { key: "remove_cached".into(), value: req.remove_cached_artifact.to_string() },
                ],
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
            .unwrap_or_else(String::new);
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "toolchain.uninstall",
                vec![
                    KeyValue { key: "toolchain_id".into(), value: toolchain_id.clone() },
                    KeyValue { key: "remove_cached".into(), value: req.remove_cached_artifact.to_string() },
                    KeyValue { key: "force".into(), value: req.force.to_string() },
                ],
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
            .unwrap_or_else(String::new);
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "toolchain.cleanup_cache",
                vec![
                    KeyValue { key: "dry_run".into(), value: req.dry_run.to_string() },
                    KeyValue { key: "remove_all".into(), value: req.remove_all.to_string() },
                ],
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
                warn!("cache cleanup job {} failed to run: {}", job_id_for_spawn, err);
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
