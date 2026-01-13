use std::{
    collections::{HashMap, HashSet},
    fs,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::Arc,
};

use aadk_proto::aadk::v1::{
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    target_service_server::{TargetService, TargetServiceServer},
    ErrorCode, ErrorDetail, GetCuttlefishStatusRequest, GetCuttlefishStatusResponse,
    GetDefaultTargetRequest, GetDefaultTargetResponse, Id, InstallApkRequest, InstallApkResponse,
    InstallCuttlefishRequest, InstallCuttlefishResponse, JobCompleted, JobEvent, JobFailed,
    JobLogAppended, JobProgress, JobProgressUpdated, JobState, JobStateChanged, KeyValue,
    LaunchRequest, LaunchResponse, ListTargetsRequest, ListTargetsResponse, LogChunk, LogcatEvent,
    PublishJobEventRequest, ResolveCuttlefishBuildRequest, ResolveCuttlefishBuildResponse,
    SetDefaultTargetRequest, SetDefaultTargetResponse, StartCuttlefishRequest,
    StartCuttlefishResponse, StartJobRequest, StopAppRequest, StopAppResponse, StopCuttlefishRequest,
    StopCuttlefishResponse, StreamLogcatRequest, Target, TargetKind, Timestamp,
    StreamJobEventsRequest, GetJobRequest,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::{mpsc, Mutex, watch},
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{transport::Channel, Request, Response, Status};
use tracing::{info, warn};

use serde::{Deserialize, Serialize};

const STATE_FILE_NAME: &str = "targets.json";

#[derive(Default)]
struct State {
    default_target: Option<Target>,
    inventory: Vec<TargetInventoryEntry>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct PersistedState {
    default_target: Option<PersistedTarget>,
    inventory: Vec<PersistedInventoryEntry>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct PersistedTarget {
    target_id: String,
    kind: i32,
    display_name: String,
    provider: String,
    address: String,
    api_level: String,
    state: String,
    details: Vec<PersistedDetail>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct PersistedDetail {
    key: String,
    value: String,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct PersistedInventoryEntry {
    target: PersistedTarget,
    last_seen_unix_millis: i64,
}

#[derive(Clone, Default)]
struct TargetInventoryEntry {
    target: PersistedTarget,
    last_seen_unix_millis: i64,
}

#[derive(Clone)]
struct Svc {
    state: Arc<Mutex<State>>,
}

impl Default for Svc {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(load_state())),
        }
    }
}

impl PersistedTarget {
    fn from_proto(target: &Target) -> Option<Self> {
        let target_id = target
            .target_id
            .as_ref()
            .map(|id| id.value.trim())
            .filter(|value| !value.is_empty())?
            .to_string();
        let target_id = normalize_target_id(&target_id);
        if target_id.is_empty() {
            return None;
        }
        Some(Self {
            target_id,
            kind: target.kind,
            display_name: target.display_name.clone(),
            provider: target.provider.clone(),
            address: normalize_target_address(&target.address),
            api_level: target.api_level.clone(),
            state: target.state.clone(),
            details: target
                .details
                .iter()
                .map(|detail| PersistedDetail {
                    key: detail.key.clone(),
                    value: detail.value.clone(),
                })
                .collect(),
        })
    }

    fn into_proto(self) -> Target {
        Target {
            target_id: Some(Id { value: self.target_id }),
            kind: self.kind,
            display_name: self.display_name,
            provider: self.provider,
            address: self.address,
            api_level: self.api_level,
            state: self.state,
            details: self
                .details
                .into_iter()
                .map(|detail| KeyValue {
                    key: detail.key,
                    value: detail.value,
                })
                .collect(),
        }
    }
}

impl TargetInventoryEntry {
    fn from_target(target: &Target, last_seen_unix_millis: i64) -> Option<Self> {
        let target = PersistedTarget::from_proto(target)?;
        Some(Self {
            target,
            last_seen_unix_millis,
        })
    }

    fn to_target(&self, state_override: Option<&str>) -> Target {
        let mut target = self.target.clone().into_proto();
        if let Some(state) = state_override {
            target.state = state.to_string();
            target.details.push(KeyValue {
                key: "inventory_state".into(),
                value: state.to_string(),
            });
        }
        target.details.push(KeyValue {
            key: "last_seen_unix_millis".into(),
            value: self.last_seen_unix_millis.to_string(),
        });
        target
    }
}

fn now_ts() -> Timestamp {
    let ms = now_millis();
    Timestamp { unix_millis: ms }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn state_file_path() -> PathBuf {
    data_dir().join("state").join(STATE_FILE_NAME)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn load_state() -> State {
    let path = state_file_path();
    match fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str::<PersistedState>(&data) {
            Ok(parsed) => {
                let default_target = parsed.default_target.clone();
                let mut inventory: Vec<TargetInventoryEntry> = parsed
                    .inventory
                    .into_iter()
                    .filter_map(|entry| {
                        if entry.target.target_id.trim().is_empty() {
                            None
                        } else {
                            Some(TargetInventoryEntry {
                                target: entry.target,
                                last_seen_unix_millis: entry.last_seen_unix_millis,
                            })
                        }
                    })
                    .collect();
                if let Some(default_target) = default_target.as_ref() {
                    let key = normalize_target_id_for_compare(&default_target.target_id);
                    if !inventory
                        .iter()
                        .any(|entry| normalize_target_id_for_compare(&entry.target.target_id) == key)
                    {
                        inventory.push(TargetInventoryEntry {
                            target: default_target.clone(),
                            last_seen_unix_millis: 0,
                        });
                    }
                }
                State {
                    default_target: default_target.map(PersistedTarget::into_proto),
                    inventory,
                }
            }
            Err(err) => {
                warn!("Failed to parse {}: {}", path.display(), err);
                State::default()
            }
        },
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                warn!("Failed to read {}: {}", path.display(), err);
            }
            State::default()
        }
    }
}

fn save_state(state: &State) -> io::Result<()> {
    let persist = PersistedState {
        default_target: state
            .default_target
            .as_ref()
            .and_then(PersistedTarget::from_proto),
        inventory: state
            .inventory
            .iter()
            .map(|entry| PersistedInventoryEntry {
                target: entry.target.clone(),
                last_seen_unix_millis: entry.last_seen_unix_millis,
            })
            .collect(),
    };
    write_json_atomic(&state_file_path(), &persist)
}

fn save_state_best_effort(state: &State) {
    if let Err(err) = save_state(state) {
        warn!("Failed to persist target state: {}", err);
    }
}

#[derive(Debug)]
enum AdbFailure {
    NotFound,
    Io(String),
    Exit { status: i32, stdout: String, stderr: String },
}

fn adb_path() -> PathBuf {
    if let Ok(path) = std::env::var("AADK_ADB_PATH") {
        return PathBuf::from(path);
    }
    if let Ok(path) = std::env::var("ADB_PATH") {
        return PathBuf::from(path);
    }
    if let Ok(sdk_root) = std::env::var("ANDROID_SDK_ROOT")
        .or_else(|_| std::env::var("ANDROID_HOME"))
    {
        let candidate = PathBuf::from(&sdk_root).join("platform-tools").join("adb");
        if candidate.exists() {
            return candidate;
        }
        let candidate = PathBuf::from(&sdk_root)
            .join("platform-tools")
            .join("adb.exe");
        if candidate.exists() {
            return candidate;
        }
    }
    let page_size = host_page_size();
    let candidate = cuttlefish_host_dir(page_size).join("bin").join("adb");
    if candidate.is_file() {
        return candidate;
    }
    PathBuf::from("adb")
}

async fn adb_output(args: &[&str]) -> Result<Output, AdbFailure> {
    let mut cmd = Command::new(adb_path());
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = cmd.output().await.map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            AdbFailure::NotFound
        } else {
            AdbFailure::Io(e.to_string())
        }
    })?;

    if output.status.success() {
        Ok(output)
    } else {
        Err(AdbFailure::Exit {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

fn format_adb_output(stdout: &str, stderr: &str) -> String {
    let stdout = stdout.trim();
    let stderr = stderr.trim();
    let mut out = String::new();

    if !stdout.is_empty() {
        out.push_str("stdout:\n");
        out.push_str(stdout);
        out.push('\n');
    }
    if !stderr.is_empty() {
        out.push_str("stderr:\n");
        out.push_str(stderr);
        out.push('\n');
    }

    out
}

fn format_adb_failure_message(status: i32, stdout: &str, stderr: &str) -> String {
    let detail = format_adb_output(stdout, stderr);
    if detail.trim().is_empty() {
        format!("adb command failed with exit {status}")
    } else {
        format!("adb command failed with exit {status}: {}", detail.trim())
    }
}

fn adb_failure_message(err: &AdbFailure) -> String {
    match err {
        AdbFailure::NotFound => "adb not found (set AADK_ADB_PATH or ANDROID_SDK_ROOT)".into(),
        AdbFailure::Io(msg) => msg.clone(),
        AdbFailure::Exit { status, stdout, stderr } => {
            format_adb_failure_message(*status, stdout, stderr)
        }
    }
}

fn adb_failure_status(err: AdbFailure) -> Status {
    match err {
        AdbFailure::NotFound => Status::failed_precondition(
            "adb not found (set AADK_ADB_PATH or ANDROID_SDK_ROOT)",
        ),
        AdbFailure::Io(msg) => Status::internal(format!("adb failed: {msg}")),
        AdbFailure::Exit { status, stdout, stderr } => {
            Status::unavailable(format_adb_failure_message(status, &stdout, &stderr))
        }
    }
}

async fn adb_get_state(serial: &str) -> Result<String, AdbFailure> {
    let args = ["-s", serial, "get-state"];
    let output = adb_output(&args).await?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_string())
}

fn classify_target_kind(serial: &str) -> TargetKind {
    if serial.starts_with("emulator-") {
        TargetKind::Emulatorlike
    } else if serial.contains(':') {
        TargetKind::Remote
    } else {
        TargetKind::Device
    }
}

fn health_state_from_adb_state(state: &str) -> &'static str {
    match state {
        "device" => "online",
        "unauthorized" => "unauthorized",
        "offline" => "offline",
        "recovery" => "recovery",
        "bootloader" => "bootloader",
        _ => "unknown",
    }
}

#[derive(Default)]
struct CuttlefishStatus {
    adb_serial: String,
    running: bool,
    raw: String,
    details: Vec<(String, String)>,
}

#[derive(Debug)]
enum CuttlefishStatusError {
    NotInstalled,
    Failed(String),
}

fn cuttlefish_enabled() -> bool {
    match std::env::var("AADK_CUTTLEFISH_ENABLE") {
        Ok(val) => !(val == "0" || val.eq_ignore_ascii_case("false")),
        Err(_) => true,
    }
}

fn cuttlefish_page_size_check_enabled() -> bool {
    match std::env::var("AADK_CUTTLEFISH_PAGE_SIZE_CHECK") {
        Ok(val) => !(val == "0" || val.eq_ignore_ascii_case("false")),
        Err(_) => true,
    }
}

fn cuttlefish_kvm_check_enabled() -> bool {
    match std::env::var("AADK_CUTTLEFISH_KVM_CHECK") {
        Ok(val) => !(val == "0" || val.eq_ignore_ascii_case("false")),
        Err(_) => true,
    }
}

fn cuttlefish_connect_enabled() -> bool {
    match std::env::var("AADK_CUTTLEFISH_CONNECT") {
        Ok(val) => !(val == "0" || val.eq_ignore_ascii_case("false")),
        Err(_) => true,
    }
}

fn cuttlefish_adb_serial() -> String {
    std::env::var("AADK_CUTTLEFISH_ADB_SERIAL").unwrap_or_else(|_| "127.0.0.1:6520".into())
}

fn cuttlefish_web_url() -> String {
    std::env::var("AADK_CUTTLEFISH_WEBRTC_URL").unwrap_or_else(|_| "https://localhost:8443".into())
}

fn cuttlefish_env_url() -> String {
    std::env::var("AADK_CUTTLEFISH_ENV_URL").unwrap_or_else(|_| "https://localhost:1443".into())
}

fn cuttlefish_cvd_bin() -> String {
    std::env::var("AADK_CVD_BIN").unwrap_or_else(|_| "cvd".into())
}

fn cuttlefish_launch_bin() -> String {
    std::env::var("AADK_LAUNCH_CVD_BIN").unwrap_or_else(|_| "launch_cvd".into())
}

fn cuttlefish_stop_bin() -> String {
    std::env::var("AADK_STOP_CVD_BIN").unwrap_or_else(|_| "stop_cvd".into())
}

fn cuttlefish_gpu_mode() -> Option<String> {
    read_env_trimmed("AADK_CUTTLEFISH_GPU_MODE")
}

fn find_command(cmd: &str) -> Option<PathBuf> {
    if cmd.contains('/') {
        let path = PathBuf::from(cmd);
        return path.is_file().then_some(path);
    }

    let mut candidates = Vec::new();
    if let Some(paths) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&paths));
    }
    candidates.extend([
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/usr/sbin"),
        PathBuf::from("/sbin"),
    ]);

    for dir in candidates {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn host_page_size() -> Option<usize> {
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size > 0 {
        Some(size as usize)
    } else {
        None
    }
}

#[derive(Debug)]
struct KvmStatus {
    present: bool,
    accessible: bool,
    detail: Option<String>,
}

fn kvm_status() -> KvmStatus {
    let path = Path::new("/dev/kvm");
    if !path.exists() {
        return KvmStatus {
            present: false,
            accessible: false,
            detail: Some("missing /dev/kvm".into()),
        };
    }

    match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(_) => KvmStatus {
            present: true,
            accessible: true,
            detail: None,
        },
        Err(err) => KvmStatus {
            present: true,
            accessible: false,
            detail: Some(err.to_string()),
        },
    }
}

fn is_root_user() -> bool {
    if let Ok(value) = std::env::var("EUID") {
        if value == "0" {
            return true;
        }
    }
    if let Ok(value) = std::env::var("UID") {
        if value == "0" {
            return true;
        }
    }
    matches!(std::env::var("USER"), Ok(value) if value == "root")
}

fn sudo_prefix() -> Option<String> {
    if is_root_user() {
        return Some(String::new());
    }
    let sudo = find_command("sudo")?;
    Some(format!("{} -n ", sudo.display()))
}

fn data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/aadk")
    } else {
        PathBuf::from("/tmp/aadk")
    }
}

fn cuttlefish_env_for_page_size(base: &str, page_size: Option<usize>) -> Option<String> {
    if let Some(size) = page_size {
        if size > 4096 {
            return read_env_trimmed(&format!("{base}_16K"));
        }
        if let Some(value) = read_env_trimmed(&format!("{base}_4K")) {
            return Some(value);
        }
    }
    read_env_trimmed(base)
}

fn cuttlefish_home_dir(page_size: Option<usize>) -> PathBuf {
    if let Some(path) = cuttlefish_env_for_page_size("AADK_CUTTLEFISH_HOME", page_size) {
        return PathBuf::from(path);
    }
    let base = data_dir().join("cuttlefish");
    let suffix = page_size.map(page_size_label).unwrap_or("default");
    base.join(suffix.to_lowercase())
}

fn cuttlefish_images_dir(page_size: Option<usize>) -> PathBuf {
    if let Some(path) = cuttlefish_env_for_page_size("AADK_CUTTLEFISH_IMAGES_DIR", page_size) {
        return PathBuf::from(path);
    }
    cuttlefish_home_dir(page_size)
}

fn cuttlefish_host_dir(page_size: Option<usize>) -> PathBuf {
    if let Some(path) = cuttlefish_env_for_page_size("AADK_CUTTLEFISH_HOST_DIR", page_size) {
        return PathBuf::from(path);
    }
    cuttlefish_home_dir(page_size)
}

fn cuttlefish_branch(page_size: Option<usize>) -> String {
    if let Some(branch) = cuttlefish_env_for_page_size("AADK_CUTTLEFISH_BRANCH", page_size) {
        return branch;
    }
    if page_size.unwrap_or(0) > 4096 {
        return "main-16k-with-phones".into();
    }
    "aosp-android-latest-release".into()
}

fn cuttlefish_target(page_size: Option<usize>) -> String {
    if let Some(target) = cuttlefish_env_for_page_size("AADK_CUTTLEFISH_TARGET", page_size) {
        return target;
    }
    if page_size.unwrap_or(0) > 4096 {
        return match std::env::consts::ARCH {
            "aarch64" => "aosp_cf_arm64".into(),
            _ => "aosp_cf_x86_64".into(),
        };
    }
    match std::env::consts::ARCH {
        "aarch64" => "aosp_cf_arm64_only_phone-userdebug".into(),
        "riscv64" => "aosp_cf_riscv64_phone-userdebug".into(),
        _ => "aosp_cf_x86_64_only_phone-userdebug".into(),
    }
}

fn cuttlefish_fallback_branch_target(_page_size: Option<usize>) -> Option<(String, String)> {
    match std::env::consts::ARCH {
        "aarch64" => Some((
            "aosp-main-throttled".into(),
            "aosp_cf_arm64_only_phone-trunk_staging-userdebug".into(),
        )),
        "riscv64" => Some((
            "aosp-main".into(),
            "aosp_cf_riscv64_phone-trunk_staging-userdebug".into(),
        )),
        _ => Some((
            "aosp-main".into(),
            "aosp_cf_x86_64_phone-trunk_staging-userdebug".into(),
        )),
    }
}

fn cuttlefish_build_id_override() -> Option<String> {
    read_env_trimmed("AADK_CUTTLEFISH_BUILD_ID")
}

fn cuttlefish_images_ready(images_dir: &Path) -> bool {
    images_dir.join("system.img").exists()
        || images_dir.join("super.img").exists()
        || images_dir.join("boot.img").exists()
}

fn read_env_trimmed(key: &str) -> Option<String> {
    let value = std::env::var(key).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn page_size_label(size: usize) -> &'static str {
    if size > 4096 {
        "16K"
    } else {
        "4K"
    }
}

fn shell_escape(value: &str) -> String {
    if value.is_empty() {
        "''".to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

fn cuttlefish_product_from_target(target: &str) -> String {
    target.split('-').next().unwrap_or(target).to_string()
}

fn cuttlefish_branch_grid_url(branch: &str) -> String {
    format!("https://ci.android.com/builds/branches/{branch}/grid")
}

fn extract_js_variables_payload(html: &str) -> Option<String> {
    let marker = "var JSVariables = ";
    let start = html.find(marker)? + marker.len();
    let mut depth = 0;
    let mut in_string = false;
    let mut escape = false;

    for (idx, ch) in html[start..].char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                if depth > 0 {
                    depth -= 1;
                }
                if depth == 0 {
                    return Some(html[start..start + idx + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CiBranchGrid {
    builds: Vec<CiBuild>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CiBuild {
    build_id: String,
    targets: Vec<CiBuildTarget>,
}

#[derive(Debug, Deserialize)]
struct CiBuildTarget {
    target: CiTargetInfo,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CiTargetInfo {
    name: String,
    build_commands: Vec<String>,
    product: Option<String>,
}

fn parse_target_product_from_commands(commands: &[String]) -> Option<String> {
    for command in commands {
        if let Some(idx) = command.find("TARGET_PRODUCT=") {
            let rest = &command[idx + "TARGET_PRODUCT=".len()..];
            let end = rest
                .find(|ch: char| ch.is_whitespace())
                .unwrap_or_else(|| rest.len());
            let candidate = rest[..end].trim_matches('"').trim();
            if !candidate.is_empty() {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

async fn fetch_branch_grid(branch: &str) -> Result<CiBranchGrid, String> {
    let url = cuttlefish_branch_grid_url(branch);
    let cmd = format!("curl -fsSL {}", shell_escape(&url));
    let (success, code, stdout, stderr) = run_shell_command_raw(&cmd)
        .await
        .map_err(|e| e.to_string())?;
    if !success {
        let detail = if stderr.trim().is_empty() {
            format!("exit_code={code}")
        } else {
            stderr.trim().to_string()
        };
        return Err(format!("failed to query CI grid: {detail}"));
    }
    let payload = extract_js_variables_payload(&stdout)
        .ok_or_else(|| "failed to locate JSVariables in CI grid".to_string())?;
    serde_json::from_str(&payload).map_err(|e| format!("invalid CI grid payload: {e}"))
}

struct CuttlefishBuildInfo {
    build_id: String,
    product: String,
}

#[derive(Clone, Debug)]
struct CuttlefishInstallOptions {
    force: bool,
    branch: Option<String>,
    target: Option<String>,
    build_id: Option<String>,
}

struct CuttlefishRequestConfig {
    branch: String,
    target: String,
    build_id_override: Option<String>,
    has_branch_override: bool,
    has_target_override: bool,
}

fn normalize_override(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve_cuttlefish_request_config(
    page_size: Option<usize>,
    branch_override: Option<String>,
    target_override: Option<String>,
    build_id_override: Option<String>,
) -> CuttlefishRequestConfig {
    let env_branch_override =
        cuttlefish_env_for_page_size("AADK_CUTTLEFISH_BRANCH", page_size).is_some();
    let env_target_override =
        cuttlefish_env_for_page_size("AADK_CUTTLEFISH_TARGET", page_size).is_some();

    let branch_override = normalize_override(branch_override);
    let target_override = normalize_override(target_override);
    let build_id_override = normalize_override(build_id_override).or_else(cuttlefish_build_id_override);

    let branch = branch_override
        .clone()
        .unwrap_or_else(|| cuttlefish_branch(page_size));
    let target = target_override
        .clone()
        .unwrap_or_else(|| cuttlefish_target(page_size));

    CuttlefishRequestConfig {
        branch,
        target,
        build_id_override,
        has_branch_override: branch_override.is_some() || env_branch_override,
        has_target_override: target_override.is_some() || env_target_override,
    }
}

async fn resolve_build_info(
    branch: &str,
    target: &str,
    build_id_override: Option<String>,
) -> Result<CuttlefishBuildInfo, String> {
    if let Some(build_id) = build_id_override {
        let mut resolved_product = None;
        if let Ok(grid) = fetch_branch_grid(branch).await {
            for build in grid.builds {
                if build.build_id != build_id {
                    continue;
                }
                if let Some(target_info) = build
                    .targets
                    .iter()
                    .find(|entry| entry.target.name == target)
                {
                    resolved_product =
                        parse_target_product_from_commands(&target_info.target.build_commands)
                            .or_else(|| target_info.target.product.clone());
                }
                break;
            }
        }
        let product = resolved_product.unwrap_or_else(|| cuttlefish_product_from_target(target));
        return Ok(CuttlefishBuildInfo { build_id, product });
    }

    let grid = fetch_branch_grid(branch).await?;
    let mut last_err = None;

    for build in grid.builds {
        let Some(target_info) = build
            .targets
            .iter()
            .find(|entry| entry.target.name == target)
        else {
            continue;
        };

        let product = parse_target_product_from_commands(&target_info.target.build_commands)
            .or_else(|| target_info.target.product.clone())
            .unwrap_or_else(|| cuttlefish_product_from_target(target));

        let target_paths = candidate_target_paths(target, &product);
        let img_candidates =
            cuttlefish_image_artifact_candidates(&product, target, &build.build_id);
        let host_candidates = cuttlefish_host_artifact_candidates(&build.build_id);

        if let Err(err) =
            resolve_artifact_url_for_targets(&build.build_id, &target_paths, &img_candidates).await
        {
            last_err = Some(err);
            continue;
        }

        if let Err(err) = resolve_artifact_url_for_targets(
            &build.build_id,
            &target_paths,
            &host_candidates,
        )
        .await
        {
            last_err = Some(err);
            continue;
        }

        return Ok(CuttlefishBuildInfo {
            build_id: build.build_id,
            product,
        });
    }

    if let Some(err) = last_err {
        return Err(err);
    }

    Err(format!(
        "no builds found for target {target} on branch {branch}"
    ))
}

fn cuttlefish_image_artifact_candidates(product: &str, target: &str, build_id: &str) -> Vec<String> {
    vec![
        format!("{product}-img-{build_id}.zip"),
        format!("{target}-img-{build_id}.zip"),
        format!("{product}-{build_id}.zip"),
        format!("{target}-{build_id}.zip"),
    ]
}

fn cuttlefish_host_artifact_candidates(build_id: &str) -> Vec<String> {
    vec![
        "cvd-host_package.tar.gz".to_string(),
        format!("cvd-host_package-{build_id}.tar.gz"),
    ]
}

fn candidate_target_paths(target: &str, product: &str) -> Vec<String> {
    let mut out = Vec::new();
    let target = target.trim();
    if !target.is_empty() {
        out.push(target.to_string());
    }
    let product = product.trim();
    if !product.is_empty() && product != target {
        out.push(product.to_string());
    }
    out
}

fn headers_look_like_html(headers: &str) -> bool {
    let lower = headers.to_ascii_lowercase();
    lower.contains("content-type: text/html")
        || lower.contains("content-type: text/plain")
        || lower.contains("content-type: application/json")
}

fn body_looks_like_html(body: &str) -> bool {
    let trimmed = body.trim_start().to_ascii_lowercase();
    trimmed.starts_with("<!doctype html") || trimmed.starts_with("<html")
}

async fn artifact_url_is_downloadable(url: &str) -> bool {
    let head_cmd = format!("curl -fsSIL {}", shell_escape(url));
    if let Ok((true, _, stdout, _)) = run_shell_command_raw(&head_cmd).await {
        if headers_look_like_html(&stdout) {
            return false;
        }
    }

    let range_cmd = format!("curl -fsSL --range 0-200 {}", shell_escape(url));
    if let Ok((true, _, stdout, _)) = run_shell_command_raw(&range_cmd).await {
        return !body_looks_like_html(&stdout);
    }

    false
}

#[derive(Debug, Deserialize)]
struct ArtifactViewerVariables {
    #[serde(rename = "artifactUrl")]
    artifact_url: Option<String>,
}

fn extract_artifact_url_from_viewer(html: &str) -> Option<String> {
    let payload = extract_js_variables_payload(html)?;
    let parsed: ArtifactViewerVariables = serde_json::from_str(&payload).ok()?;
    parsed.artifact_url
}

async fn resolve_artifact_url_via_viewer(
    build_id: &str,
    target: &str,
    artifact: &str,
) -> Option<String> {
    let url = format!("https://ci.android.com/builds/submitted/{build_id}/{target}/latest/{artifact}");
    let cmd = format!("curl -fsSL {}", shell_escape(&url));
    let (ok, _, stdout, _) = run_shell_command_raw(&cmd).await.ok()?;
    if !ok {
        return None;
    }
    let artifact_url = extract_artifact_url_from_viewer(&stdout)?;
    let trimmed = artifact_url.trim();
    if trimmed.is_empty() {
        return None;
    }
    if artifact_url_is_downloadable(trimmed).await {
        return Some(trimmed.to_string());
    }
    None
}

async fn resolve_artifact_url(
    build_id: &str,
    target: &str,
    artifacts: &[String],
) -> Result<String, String> {
    let bases = [
        format!(
            "https://android-ci.googleusercontent.com/builds/submitted/{build_id}/{target}/latest/raw/"
        ),
        format!(
            "https://android-ci.googleusercontent.com/builds/submitted/{build_id}/{target}/latest/"
        ),
        format!("https://ci.android.com/builds/submitted/{build_id}/{target}/latest/raw/"),
        format!("https://ci.android.com/builds/submitted/{build_id}/{target}/latest/"),
    ];

    for artifact in artifacts {
        for base in &bases {
            let url = format!("{base}{artifact}");
            if artifact_url_is_downloadable(&url).await {
                return Ok(url);
            }
        }
        if let Some(url) = resolve_artifact_url_via_viewer(build_id, target, artifact).await {
            return Ok(url);
        }
    }

    Err(format!(
        "unable to resolve artifact url for build_id={build_id}, target={target}"
    ))
}

async fn resolve_artifact_url_for_targets(
    build_id: &str,
    target_paths: &[String],
    artifacts: &[String],
) -> Result<String, String> {
    let mut last_err = None;
    for target in target_paths {
        match resolve_artifact_url(build_id, target, artifacts).await {
            Ok(url) if !url.trim().is_empty() => return Ok(url),
            Ok(_) => {
                last_err = Some(format!(
                    "empty artifact url for build_id={build_id}, target={target}"
                ));
            }
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        format!("unable to resolve artifact url for build_id={build_id}")
    }))
}

fn cuttlefish_cvd_path() -> Option<PathBuf> {
    find_command(&cuttlefish_cvd_bin())
}

fn cuttlefish_launch_path(page_size: Option<usize>) -> Option<PathBuf> {
    if let Some(path) = find_command(&cuttlefish_launch_bin()) {
        return Some(path);
    }
    let candidate = cuttlefish_host_dir(page_size).join("bin").join(cuttlefish_launch_bin());
    candidate.is_file().then_some(candidate)
}

fn cuttlefish_stop_path(page_size: Option<usize>) -> Option<PathBuf> {
    if let Some(path) = find_command(&cuttlefish_stop_bin()) {
        return Some(path);
    }
    let candidate = cuttlefish_host_dir(page_size).join("bin").join(cuttlefish_stop_bin());
    candidate.is_file().then_some(candidate)
}

fn cuttlefish_home_env_prefix(home: &Path) -> String {
    let home_str = home.to_string_lossy();
    format!("HOME={} ", shell_escape(home_str.as_ref()))
}

async fn cuttlefish_status() -> Result<CuttlefishStatus, CuttlefishStatusError> {
    let page_size = host_page_size();
    let home_dir = cuttlefish_home_dir(page_size);
    let cvd_path = cuttlefish_cvd_path();
    let launch_path = cuttlefish_launch_path(page_size);
    if cvd_path.is_none() && launch_path.is_none() {
        return Err(CuttlefishStatusError::NotInstalled);
    }

    let mut status = CuttlefishStatus::default();
    status.adb_serial = cuttlefish_adb_serial();

    let Some(cvd_path) = cvd_path else {
        return Ok(status);
    };

    let host_dir = cuttlefish_host_dir(page_size);
    let mut cmd = Command::new(&cvd_path);
    cmd.arg("status")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HOME", &home_dir);
    if host_dir.is_dir() {
        cmd.current_dir(&host_dir);
    }
    let output = cmd.output().await.map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            CuttlefishStatusError::NotInstalled
        } else {
            CuttlefishStatusError::Failed(e.to_string())
        }
    })?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        if stderr.to_lowercase().contains("not applicable: no device") {
            return Ok(status);
        }
        let mut message = String::new();
        if !stdout.is_empty() {
            message.push_str("stdout:\n");
            message.push_str(&stdout);
            message.push('\n');
        }
        if !stderr.is_empty() {
            message.push_str("stderr:\n");
            message.push_str(&stderr);
            message.push('\n');
        }
        if message.trim().is_empty() {
            message = format!("exit_code={exit_code}");
        }
        return Err(CuttlefishStatusError::Failed(message.trim().to_string()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    status.raw = stdout.trim().to_string();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_lowercase();
        if lower.contains("running") && !lower.contains("not running") {
            status.running = true;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_lowercase().replace(' ', "_");
            let value = value.trim().to_string();
            if key == "adb" || key == "adb_serial" || key == "adb_address" {
                status.adb_serial = value.clone();
            }
            status.details.push((key, value));
        }
    }

    Ok(status)
}

fn normalize_adb_addr(addr: &str) -> String {
    let addr = addr.trim();
    let lower = addr.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("localhost:") {
        return format!("localhost:{rest}");
    }
    if let Some(rest) = lower.strip_prefix("127.0.0.1:") {
        return format!("localhost:{rest}");
    }
    if let Some(rest) = lower.strip_prefix("0.0.0.0:") {
        return format!("localhost:{rest}");
    }
    if let Some(rest) = lower.strip_prefix("[::1]:") {
        return format!("localhost:{rest}");
    }
    if let Some(rest) = lower.strip_prefix("[::]:") {
        return format!("localhost:{rest}");
    }
    addr.to_string()
}

fn normalize_target_id(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.contains(':') {
        return normalize_adb_addr(trimmed);
    }
    trimmed.to_string()
}

fn normalize_target_address(value: &str) -> String {
    normalize_target_id(value)
}

fn normalize_target_id_for_compare(value: &str) -> String {
    normalize_target_id(value).to_ascii_lowercase()
}

fn canonicalize_adb_serial(addr: &str) -> String {
    let addr = addr.trim();
    if let Some(rest) = addr.strip_prefix("localhost:") {
        return format!("127.0.0.1:{rest}");
    }
    if let Some(rest) = addr.strip_prefix("0.0.0.0:") {
        return format!("127.0.0.1:{rest}");
    }
    if let Some(rest) = addr.strip_prefix("[::1]:") {
        return format!("127.0.0.1:{rest}");
    }
    if let Some(rest) = addr.strip_prefix("[::]:") {
        return format!("127.0.0.1:{rest}");
    }
    addr.to_string()
}

async fn adb_connect(addr: &str) -> Option<String> {
    match adb_output(&["connect", addr]).await {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let merged = format_adb_output(&stdout, &stderr);
            if merged.is_empty() {
                Some("adb connect: no output".into())
            } else {
                Some(merged.trim().to_string())
            }
        }
        Err(err) => Some(adb_failure_message(&err)),
    }
}

async fn adb_get_prop(serial: &str, prop: &str) -> Result<String, AdbFailure> {
    let args = ["-s", serial, "shell", "getprop", prop];
    let output = adb_output(&args).await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn adb_shell(serial: &str, cmd: &str) -> Result<(), AdbFailure> {
    let args = ["-s", serial, "shell", cmd];
    let _ = adb_output(&args).await?;
    Ok(())
}

fn upsert_detail(details: &mut Vec<KeyValue>, key: &str, value: impl ToString) {
    let value = value.to_string();
    if let Some(item) = details.iter_mut().find(|item| item.key == key) {
        item.value = value;
    } else {
        details.push(KeyValue {
            key: key.to_string(),
            value,
        });
    }
}

async fn adb_get_prop_timeout(serial: &str, prop: &str) -> Option<String> {
    let timeout = std::time::Duration::from_secs(2);
    match tokio::time::timeout(timeout, adb_get_prop(serial, prop)).await {
        Ok(Ok(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

#[derive(Default)]
struct TargetProps {
    api_level: Option<String>,
    release: Option<String>,
    abi: Option<String>,
    abi_list: Option<String>,
    manufacturer: Option<String>,
    model: Option<String>,
    device: Option<String>,
    product: Option<String>,
    boot_completed: Option<String>,
}

async fn adb_collect_props(serial: &str) -> TargetProps {
    TargetProps {
        api_level: adb_get_prop_timeout(serial, "ro.build.version.sdk").await,
        release: adb_get_prop_timeout(serial, "ro.build.version.release").await,
        abi: adb_get_prop_timeout(serial, "ro.product.cpu.abi").await,
        abi_list: adb_get_prop_timeout(serial, "ro.product.cpu.abilist").await,
        manufacturer: adb_get_prop_timeout(serial, "ro.product.manufacturer").await,
        model: adb_get_prop_timeout(serial, "ro.product.model").await,
        device: adb_get_prop_timeout(serial, "ro.product.device").await,
        product: adb_get_prop_timeout(serial, "ro.product.name").await,
        boot_completed: adb_get_prop_timeout(serial, "sys.boot_completed").await,
    }
}

fn parse_adb_devices(output: &str, include_offline: bool) -> Vec<Target> {
    let mut targets = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("List of devices attached") {
            continue;
        }

        let mut parts = line.split_whitespace();
        let serial = match parts.next() {
            Some(s) => s,
            None => continue,
        };
        let state = match parts.next() {
            Some(s) => s,
            None => continue,
        };

        if !include_offline && state != "device" {
            continue;
        }

        let mut model = None;
        let mut product = None;
        let mut device = None;
        let mut transport_id = None;
        let mut extra = Vec::new();

        for part in parts {
            if let Some((key, value)) = part.split_once(':') {
                match key {
                    "model" => model = Some(value.to_string()),
                    "product" => product = Some(value.to_string()),
                    "device" => device = Some(value.to_string()),
                    "transport_id" => transport_id = Some(value.to_string()),
                    _ => extra.push((key.to_string(), value.to_string())),
                }
            } else {
                extra.push(("info".into(), part.to_string()));
            }
        }

        let normalized = normalize_target_id(serial);
        let display_name = model.clone().unwrap_or_else(|| normalized.clone());
        let mut details = Vec::new();

        if let Some(value) = product {
            details.push(KeyValue { key: "product".into(), value });
        }
        if let Some(value) = model.clone() {
            details.push(KeyValue { key: "model".into(), value });
        }
        if let Some(value) = device {
            details.push(KeyValue { key: "device".into(), value });
        }
        if let Some(value) = transport_id {
            details.push(KeyValue { key: "transport_id".into(), value });
        }
        for (key, value) in extra {
            details.push(KeyValue { key, value });
        }
        details.push(KeyValue {
            key: "adb_state".into(),
            value: state.into(),
        });

        targets.push(Target {
            target_id: Some(Id { value: normalized.clone() }),
            kind: classify_target_kind(serial) as i32,
            display_name,
            provider: "adb".into(),
            address: normalized,
            api_level: "".into(),
            state: state.into(),
            details,
        });
    }

    targets
}

async fn enrich_adb_targets(targets: &mut [Target]) {
    for target in targets.iter_mut() {
        if target.provider != "adb" {
            continue;
        }
        let state = target.state.trim().to_string();
        upsert_detail(
            &mut target.details,
            "health_state",
            health_state_from_adb_state(&state),
        );
        if state != "device" {
            continue;
        }

        let mut serial = target.address.clone();
        if serial.trim().is_empty() {
            serial = target
                .target_id
                .as_ref()
                .map(|id| id.value.clone())
                .unwrap_or_default();
        }
        let serial = canonicalize_adb_serial(&serial);
        upsert_detail(&mut target.details, "adb_serial", &serial);

        let props = adb_collect_props(&serial).await;
        if let Some(api_level) = props.api_level.as_ref() {
            target.api_level = api_level.clone();
            upsert_detail(&mut target.details, "api_level", api_level);
        }
        if let Some(release) = props.release.as_ref() {
            upsert_detail(&mut target.details, "android_release", release);
        }
        if let Some(abi) = props.abi.as_ref() {
            upsert_detail(&mut target.details, "abi", abi);
        }
        if let Some(abi_list) = props.abi_list.as_ref() {
            upsert_detail(&mut target.details, "abi_list", abi_list);
        }
        if let Some(manufacturer) = props.manufacturer.as_ref() {
            upsert_detail(&mut target.details, "manufacturer", manufacturer);
        }
        if let Some(model) = props.model.as_ref() {
            upsert_detail(&mut target.details, "model", model);
        }
        if let Some(device) = props.device.as_ref() {
            upsert_detail(&mut target.details, "device", device);
        }
        if let Some(product) = props.product.as_ref() {
            upsert_detail(&mut target.details, "product_name", product);
        }
        if let Some(boot) = props.boot_completed.as_ref() {
            upsert_detail(&mut target.details, "boot_completed", boot);
            if boot != "1" {
                upsert_detail(&mut target.details, "health_state", "booting");
            }
        }
    }
}

async fn maybe_cuttlefish_target(
    adb_targets: &mut Vec<Target>,
    include_offline: bool,
) -> Result<Option<Target>, Status> {
    if !cuttlefish_enabled() {
        return Ok(None);
    }

    let mut status = CuttlefishStatus::default();
    let mut status_error = None;
    match cuttlefish_status().await {
        Ok(found) => status = found,
        Err(CuttlefishStatusError::NotInstalled) => return Ok(None),
        Err(CuttlefishStatusError::Failed(err)) => {
            warn!("Cuttlefish status failed: {}", err);
            status_error = Some(err);
        }
    };

    let page_size = host_page_size();
    let adb_serial_config = cuttlefish_adb_serial();
    let adb_serial_config_normalized = normalize_target_id(&adb_serial_config);
    let normalized_config = normalize_target_id_for_compare(&adb_serial_config);
    let mut adb_serial = if status.adb_serial.is_empty() {
        adb_serial_config.clone()
    } else {
        status.adb_serial.clone()
    };

    let mut adb_entry = None;
    if let Some(index) = adb_targets.iter().position(|t| {
        t.target_id
            .as_ref()
            .map(|i| normalize_target_id_for_compare(&i.value) == normalized_config)
            .unwrap_or(false)
    }) {
        adb_entry = Some(adb_targets.remove(index));
        if let Some(id) = adb_entry.as_ref().and_then(|entry| entry.target_id.as_ref()) {
            adb_serial = id.value.clone();
        }
    }
    let adb_serial_normalized = normalize_target_id(&adb_serial);
    let adb_serial_canonical = canonicalize_adb_serial(&adb_serial_normalized);

    let status_running = status_error.is_none() && status.running;
    let connect_enabled = cuttlefish_connect_enabled();

    let mut details = Vec::new();
    details.push(KeyValue { key: "cvd_bin".into(), value: cuttlefish_cvd_bin() });
    details.push(KeyValue { key: "launch_cvd_bin".into(), value: cuttlefish_launch_bin() });
    details.push(KeyValue { key: "adb_path".into(), value: adb_path().display().to_string() });
    details.push(KeyValue { key: "adb_serial".into(), value: adb_serial_normalized.clone() });
    if adb_serial_normalized != adb_serial_config_normalized {
        details.push(KeyValue { key: "adb_serial_config".into(), value: adb_serial_config_normalized.clone() });
    }
    if let Some(size) = page_size {
        details.push(KeyValue { key: "host_page_size".into(), value: size.to_string() });
    }
    let kvm = kvm_status();
    details.push(KeyValue { key: "kvm_present".into(), value: kvm.present.to_string() });
    details.push(KeyValue { key: "kvm_access".into(), value: kvm.accessible.to_string() });
    details.push(KeyValue { key: "kvm_check_enabled".into(), value: cuttlefish_kvm_check_enabled().to_string() });
    if let Some(detail) = kvm.detail {
        details.push(KeyValue { key: "kvm_detail".into(), value: detail });
    }
    details.push(KeyValue { key: "cuttlefish_running".into(), value: status_running.to_string() });
    details.push(KeyValue { key: "cuttlefish_connect_enabled".into(), value: connect_enabled.to_string() });
    details.push(KeyValue { key: "cuttlefish_home".into(), value: cuttlefish_home_dir(page_size).display().to_string() });
    details.push(KeyValue { key: "cuttlefish_images_dir".into(), value: cuttlefish_images_dir(page_size).display().to_string() });
    details.push(KeyValue { key: "cuttlefish_host_dir".into(), value: cuttlefish_host_dir(page_size).display().to_string() });
    details.push(KeyValue { key: "cuttlefish_branch".into(), value: cuttlefish_branch(page_size) });
    details.push(KeyValue { key: "cuttlefish_target".into(), value: cuttlefish_target(page_size) });
    if let Some(mode) = cuttlefish_gpu_mode() {
        details.push(KeyValue { key: "cuttlefish_gpu_mode".into(), value: mode });
    }
    if let Some(build_id) = cuttlefish_build_id_override() {
        details.push(KeyValue { key: "cuttlefish_build_id".into(), value: build_id });
    }
    details.push(KeyValue { key: "cuttlefish_webrtc_url".into(), value: cuttlefish_web_url() });
    details.push(KeyValue { key: "cuttlefish_env_url".into(), value: cuttlefish_env_url() });
    for (key, value) in &status.details {
        details.push(KeyValue { key: format!("cuttlefish_{key}"), value: value.clone() });
    }
    if let Some(err) = status_error.as_ref() {
        details.push(KeyValue { key: "cuttlefish_status_error".into(), value: err.clone() });
    }
    if !status.raw.is_empty() {
        details.push(KeyValue { key: "cuttlefish_status_raw".into(), value: status.raw.clone() });
    }

    let should_connect = status_running
        && adb_entry.is_none()
        && connect_enabled
        && adb_serial_canonical.contains(':');
    if should_connect {
        if let Some(msg) = adb_connect(&adb_serial_canonical).await {
            details.push(KeyValue { key: "adb_connect".into(), value: msg });
        }
    } else if status_error.is_some() {
        details.push(KeyValue { key: "adb_connect_status".into(), value: "skipped (cuttlefish status error)".into() });
    } else if !connect_enabled {
        details.push(KeyValue { key: "adb_connect_status".into(), value: "skipped (AADK_CUTTLEFISH_CONNECT=0)".into() });
    } else if adb_entry.is_some() {
        details.push(KeyValue { key: "adb_connect_status".into(), value: "skipped (already listed)".into() });
    } else {
        details.push(KeyValue { key: "adb_connect_status".into(), value: "skipped (cuttlefish not running)".into() });
    }

    let mut api_level = String::new();
    let mut release = String::new();
    let adb_state = if let Some(entry) = &adb_entry {
        details.extend(entry.details.clone());
        details.push(KeyValue { key: "adb_state".into(), value: entry.state.clone() });
        Some(entry.state.clone())
    } else if connect_enabled {
        match adb_get_state(&adb_serial_canonical).await {
            Ok(state) => {
                details.push(KeyValue { key: "adb_state".into(), value: state.clone() });
                Some(state)
            }
            Err(err) => {
                details.push(KeyValue { key: "adb_state_error".into(), value: adb_failure_message(&err) });
                None
            }
        }
    } else {
        None
    };

    if adb_state.as_deref() == Some("device") {
        if let Ok(value) = adb_get_prop(&adb_serial_canonical, "ro.build.version.sdk").await {
            api_level = value.clone();
            details.push(KeyValue { key: "api_level".into(), value });
        }
        if let Ok(value) = adb_get_prop(&adb_serial_canonical, "ro.build.version.release").await {
            release = value.clone();
            details.push(KeyValue { key: "android_release".into(), value });
        }
        if let Some(abi) = adb_get_prop_timeout(&adb_serial_canonical, "ro.product.cpu.abi").await {
            details.push(KeyValue { key: "abi".into(), value: abi });
        }
        if let Some(abi_list) = adb_get_prop_timeout(&adb_serial_canonical, "ro.product.cpu.abilist").await {
            details.push(KeyValue { key: "abi_list".into(), value: abi_list });
        }
    }

    let mut state = if let Some(state) = adb_state {
        state
    } else if status_running {
        "running".into()
    } else if status_error.is_some() {
        "error".into()
    } else {
        "stopped".into()
    };
    if !include_offline && state != "device" && state != "running" {
        return Ok(None);
    }

    if state == "device" && release.is_empty() {
        if !status_running {
            state = "offline".into();
        }
    }

    let health_state = if state == "device" {
        "online"
    } else if state == "running" {
        "booting"
    } else if state == "error" {
        "error"
    } else if state == "offline" || state == "unauthorized" {
        health_state_from_adb_state(&state)
    } else {
        "stopped"
    };
    details.push(KeyValue {
        key: "health_state".into(),
        value: health_state.to_string(),
    });

    let display_name = adb_entry
        .as_ref()
        .map(|entry| entry.display_name.clone())
        .unwrap_or_else(|| "Cuttlefish (local)".into());

    Ok(Some(Target {
        target_id: Some(Id { value: adb_serial_normalized.clone() }),
        kind: TargetKind::Emulatorlike as i32,
        display_name,
        provider: "cuttlefish".into(),
        address: adb_serial_normalized,
        api_level,
        state,
        details,
    }))
}

enum TargetProvider {
    Adb,
    Cuttlefish,
}

impl TargetProvider {
    async fn list_targets(&self, include_offline: bool) -> Result<Vec<Target>, Status> {
        match self {
            TargetProvider::Adb => list_adb_targets(include_offline).await,
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

async fn list_adb_targets(include_offline: bool) -> Result<Vec<Target>, Status> {
    let output = adb_output(&["devices", "-l"])
        .await
        .map_err(adb_failure_status)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut targets = parse_adb_devices(&stdout, include_offline);
    enrich_adb_targets(&mut targets).await;
    Ok(targets)
}

async fn fetch_targets(include_offline: bool) -> Result<Vec<Target>, Status> {
    let providers = target_providers();
    let mut targets = Vec::new();

    for provider in &providers {
        let mut items = provider.list_targets(include_offline).await?;
        targets.append(&mut items);
    }

    for provider in &providers {
        provider.augment_targets(&mut targets, include_offline).await?;
    }

    Ok(targets)
}

fn upsert_inventory_entries(
    inventory: &mut Vec<TargetInventoryEntry>,
    targets: &[Target],
    now: i64,
) {
    let mut index: HashMap<String, usize> = HashMap::new();
    for (idx, entry) in inventory.iter().enumerate() {
        index.insert(
            normalize_target_id_for_compare(&entry.target.target_id),
            idx,
        );
    }

    for target in targets {
        let Some(entry) = TargetInventoryEntry::from_target(target, now) else {
            continue;
        };
        let key = normalize_target_id_for_compare(&entry.target.target_id);
        if let Some(existing) = index.get(&key).copied() {
            inventory[existing] = entry;
        } else {
            inventory.push(entry);
        }
    }
}

fn merge_inventory_targets(
    live: &mut Vec<Target>,
    inventory: &[TargetInventoryEntry],
    include_offline: bool,
) {
    if !include_offline {
        return;
    }
    let mut seen: HashSet<String> = HashSet::new();
    for target in live.iter() {
        if let Some(id) = target.target_id.as_ref() {
            seen.insert(normalize_target_id_for_compare(&id.value));
        }
    }
    for entry in inventory {
        let key = normalize_target_id_for_compare(&entry.target.target_id);
        if !seen.contains(&key) {
            live.push(entry.to_target(Some("offline")));
        }
    }
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
    project_id: Option<Id>,
    target_id: Option<Id>,
) -> Result<String, Status> {
    let resp = client
        .start_job(StartJobRequest {
            job_type: job_type.into(),
            params,
            project_id,
            target_id,
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
                stream: "targets".into(),
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

fn cancel_requested(cancel_rx: &watch::Receiver<bool>) -> bool {
    *cancel_rx.borrow()
}

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
            let _ = publish_progress(
                client,
                job_id,
                20,
                "target online",
                metrics,
            )
            .await;
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
                vec![KeyValue { key: "apk_path".into(), value: apk_path }],
            )
            .await;
        }
        Err(err) => {
            let code = if matches!(err, AdbFailure::NotFound) {
                ErrorCode::AdbNotAvailable
            } else {
                ErrorCode::InstallFailed
            };
            let detail = job_error_detail(code, "adb install failed", adb_failure_message(&err), &job_id);
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
        }
    }
}

async fn run_launch_job(job_id: String, target_id: String, application_id: String, activity: String) {
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
                vec![KeyValue { key: "application_id".into(), value: application_id }],
            )
            .await;
        }
        Err(err) => {
            let code = if matches!(err, AdbFailure::NotFound) {
                ErrorCode::AdbNotAvailable
            } else {
                ErrorCode::LaunchFailed
            };
            let detail = job_error_detail(code, "adb launch failed", adb_failure_message(&err), &job_id);
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
                vec![KeyValue { key: "application_id".into(), value: application_id }],
            )
            .await;
        }
        Err(err) => {
            let code = if matches!(err, AdbFailure::NotFound) {
                ErrorCode::AdbNotAvailable
            } else {
                ErrorCode::Internal
            };
            let detail = job_error_detail(code, "adb stop failed", adb_failure_message(&err), &job_id);
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
        }
    }
}

fn is_cuttlefish_package_missing(detail: &str) -> bool {
    let lower = detail.to_lowercase();
    lower.contains("unable to locate package cuttlefish")
        || lower.contains("cuttlefish-base")
        || lower.contains("cuttlefish-user")
        || lower.contains("no installation candidate")
}

fn classify_install_error(detail: &str) -> ErrorCode {
    if is_cuttlefish_package_missing(detail) {
        return ErrorCode::NotFound;
    }
    let lower = detail.to_lowercase();
    if lower.contains("permission denied")
        || lower.contains("a password is required")
        || (lower.contains("sudo") && lower.contains("password"))
    {
        ErrorCode::PermissionDenied
    } else {
        ErrorCode::Internal
    }
}

async fn run_shell_command(command: &str) -> Result<(bool, i32, String), io::Error> {
    let (success, code, stdout, stderr) = run_shell_command_inner(command, None).await?;
    let log = format_adb_output(&stdout, &stderr);
    Ok((success, code, log))
}

async fn run_shell_command_in_dir(command: &str, dir: &Path) -> Result<(bool, i32, String), io::Error> {
    let (success, code, stdout, stderr) = run_shell_command_inner(command, Some(dir)).await?;
    let log = format_adb_output(&stdout, &stderr);
    Ok((success, code, log))
}

async fn run_shell_command_raw(
    command: &str,
) -> Result<(bool, i32, String, String), io::Error> {
    run_shell_command_inner(command, None).await
}

async fn run_shell_command_inner(
    command: &str,
    dir: Option<&Path>,
) -> Result<(bool, i32, String, String), io::Error> {
    let mut cmd = Command::new("sh");
    cmd.arg("-lc")
        .arg(command)
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("APT_LISTCHANGES_FRONTEND", "none")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    let output = cmd.output().await?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    Ok((output.status.success(), code, stdout, stderr))
}

async fn current_user_groups() -> Option<Vec<String>> {
    let (ok, _, stdout, _) = run_shell_command_raw("id -Gn").await.ok()?;
    if !ok {
        return None;
    }
    let groups: Vec<String> = stdout
        .split_whitespace()
        .map(|group| group.trim().to_string())
        .filter(|group| !group.is_empty())
        .collect();
    if groups.is_empty() {
        None
    } else {
        Some(groups)
    }
}

fn missing_groups(groups: &[String], required: &[&str]) -> Vec<String> {
    required
        .iter()
        .filter(|required| !groups.iter().any(|group| group == *required))
        .map(|group| group.to_string())
        .collect()
}

async fn run_diag_command(label: &str, command: &str) -> Option<String> {
    match run_shell_command(command).await {
        Ok((_, _, log)) => {
            if log.trim().is_empty() {
                None
            } else {
                Some(format!("{label}:\n{log}"))
            }
        }
        Err(_) => None,
    }
}

async fn collect_cuttlefish_diagnostics() -> String {
    let mut out = String::new();
    let page_size = host_page_size();
    if let Some(size) = page_size {
        out.push_str(&format!("host page size: {size}\n"));
    }
    out.push_str(&format!("cuttlefish_home: {}\n", cuttlefish_home_dir(page_size).display()));
    out.push_str(&format!("cuttlefish_images_dir: {}\n", cuttlefish_images_dir(page_size).display()));
    out.push_str(&format!("cuttlefish_host_dir: {}\n\n", cuttlefish_host_dir(page_size).display()));
    let kvm = kvm_status();
    out.push_str(&format!("kvm_present: {}\n", kvm.present));
    out.push_str(&format!("kvm_access: {}\n", kvm.accessible));
    if let Some(detail) = kvm.detail {
        out.push_str(&format!("kvm_detail: {}\n", detail.trim()));
    }
    out.push('\n');

    if let Some(cvd_path) = cuttlefish_cvd_path() {
        let cmd = format!("{} status", cvd_path.display());
        let host_dir = cuttlefish_host_dir(page_size);
        let result = if host_dir.is_dir() {
            run_shell_command_in_dir(&cmd, &host_dir).await
        } else {
            run_shell_command(&cmd).await
        };
        if let Ok((_, _, log)) = result {
            if !log.trim().is_empty() {
                out.push_str("cvd status:\n");
                out.push_str(&log);
                out.push_str("\n\n");
            }
        }
    }

    let adb_cmd = format!("{} devices -l", adb_path().display());
    if let Some(section) = run_diag_command("adb devices", &adb_cmd).await {
        out.push_str(&section);
        out.push_str("\n\n");
    }

    if let Some(section) = run_diag_command("kvm device", "ls -l /dev/kvm").await {
        out.push_str(&section);
        out.push_str("\n\n");
    }

    if let Some(section) = run_diag_command("groups", "id -nG").await {
        out.push_str(&section);
        out.push_str("\n\n");
    }

    if let Some(section) = run_diag_command("uname -a", "uname -a").await {
        out.push_str(&section);
        out.push('\n');
    }

    out.trim().to_string()
}

async fn append_cuttlefish_diagnostics(detail: &mut String) {
    let diagnostics = collect_cuttlefish_diagnostics().await;
    if diagnostics.is_empty() {
        return;
    }
    detail.push_str("\n\nDiagnostics:\n");
    detail.push_str(&diagnostics);
}

struct CuttlefishCommandOutcome {
    success: bool,
    exit_code: i32,
    log: String,
}

async fn run_cuttlefish_command(
    job_client: &mut JobServiceClient<Channel>,
    job_id: &str,
    command: &str,
    phase: &str,
    percent: u32,
    cwd: Option<&Path>,
) -> Result<CuttlefishCommandOutcome, ErrorDetail> {
    let mut metrics = vec![metric("command", command)];
    if let Some(dir) = cwd {
        metrics.push(metric("cwd", dir.display()));
    }
    let _ = publish_progress(job_client, job_id, percent, phase, metrics).await;
    let _ = publish_log(job_client, job_id, &format!("Running: {command}\n")).await;

    let result = match cwd {
        Some(dir) => run_shell_command_in_dir(command, dir).await,
        None => run_shell_command(command).await,
    };
    match result {
        Ok((success, exit_code, log)) => {
            if !log.is_empty() {
                let _ = publish_log(job_client, job_id, &log).await;
            }
            Ok(CuttlefishCommandOutcome { success, exit_code, log })
        }
        Err(err) => {
            let code = if err.kind() == io::ErrorKind::NotFound {
                ErrorCode::NotFound
            } else {
                ErrorCode::Internal
            };
            Err(job_error_detail(code, "failed to run cuttlefish command", err.to_string(), job_id))
        }
    }
}

struct CuttlefishRuntime {
    page_size: Option<usize>,
    home_dir: PathBuf,
    images_dir: PathBuf,
    host_dir: PathBuf,
}

fn cleanup_cuttlefish_temp() {
    // Cuttlefish can leave stale vsock and instance sockets under /tmp if a previous run failed.
    // A subsequent launch may fail with "IsDirectoryEmpty test failed" unless these are removed.
    for path in ["/tmp/vsock_3_1000", "/tmp/cf_avd_1000"] {
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.is_dir() {
                if let Err(err) = std::fs::remove_dir_all(path) {
                    warn!("failed to remove stale dir {}: {}", path, err);
                }
            } else if let Err(err) = std::fs::remove_file(path) {
                warn!("failed to remove stale file {}: {}", path, err);
            }
        }
    }
}

async fn enable_guest_bluetooth(adb_serial: &str) {
    let cmds = [
        "cmd bluetooth_manager enable",
        "settings put global bluetooth_on 1",
    ];
    for cmd in cmds {
        let _ = adb_shell(adb_serial, cmd).await;
    }
}

async fn adb_list_devices(include_offline: bool) -> Result<Vec<Target>, AdbFailure> {
    let output = adb_output(&["devices", "-l"]).await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_adb_devices(&stdout, include_offline))
}

async fn adb_find_device_serial() -> Option<String> {
    let devices = adb_list_devices(false).await.ok()?;
    devices.first().map(|t| t.address.clone())
}

async fn wait_for_adb_device(max_attempts: usize, delay: std::time::Duration) -> Option<String> {
    for _ in 0..max_attempts {
        if let Some(serial) = adb_find_device_serial().await {
            return Some(serial);
        }
        tokio::time::sleep(delay).await;
    }
    None
}

fn should_recover_bluetooth(log: &str) -> bool {
    let lower = log.to_lowercase();
    lower.contains("bluetooth")
        && (lower.contains("boot_failed")
            || lower.contains("boot pending")
            || lower.contains("dependencies not ready"))
}

fn args_has_flag(args: &str, flag: &str) -> bool {
    let prefix = format!("{flag}=");
    args.split_whitespace()
        .any(|part| part == flag || part.starts_with(&prefix))
}

fn append_arg_once(mut command: String, arg: &str) -> String {
    if command.contains(arg) {
        return command;
    }
    if !command.is_empty() {
        command.push(' ');
    }
    command.push_str(arg);
    command
}

async fn cuttlefish_preflight(
    job_client: &mut JobServiceClient<Channel>,
    job_id: &str,
    require_kvm: bool,
    require_images: bool,
) -> Result<CuttlefishRuntime, ErrorDetail> {
    let page_size = host_page_size();
    let home_dir = cuttlefish_home_dir(page_size);
    let images_dir = cuttlefish_images_dir(page_size);
    let host_dir = cuttlefish_host_dir(page_size);

    if let Some(size) = page_size {
        let _ = publish_log(job_client, job_id, &format!("Host page size: {size}\n")).await;
        if require_images && size > 4096 {
            if cuttlefish_page_size_check_enabled() {
                if !cuttlefish_images_ready(&images_dir) {
                    return Err(job_error_detail(
                        ErrorCode::Unavailable,
                        "missing 16K Cuttlefish images",
                        format!(
                            "page_size={size}; run Install Cuttlefish or set AADK_CUTTLEFISH_IMAGES_DIR_16K"
                        ),
                        job_id,
                    ));
                }
            } else {
                let _ = publish_log(
                    job_client,
                    job_id,
                    "Skipping 16K image check (AADK_CUTTLEFISH_PAGE_SIZE_CHECK=0)\n",
                )
                .await;
            }
        }
    }

    if require_kvm {
        if cuttlefish_kvm_check_enabled() {
            let status = kvm_status();
            let _ = publish_log(
                job_client,
                job_id,
                &format!(
                    "KVM check: present={} accessible={}\n",
                    status.present, status.accessible
                ),
            )
            .await;
            if !status.present {
                return Err(job_error_detail(
                    ErrorCode::Unavailable,
                    "KVM not available",
                    "missing /dev/kvm; enable virtualization or nested virtualization".into(),
                    job_id,
                ));
            }
            if !status.accessible {
                let detail = status
                    .detail
                    .unwrap_or_else(|| "failed to open /dev/kvm".into());
                return Err(job_error_detail(
                    ErrorCode::PermissionDenied,
                    "KVM access denied",
                    format!(
                        "{}; add the user to the kvm group and re-login",
                        detail.trim()
                    ),
                    job_id,
                ));
            }
        } else {
            let _ = publish_log(
                job_client,
                job_id,
                "Skipping KVM check (AADK_CUTTLEFISH_KVM_CHECK=0)\n",
            )
            .await;
        }
    }

    if require_images && !cuttlefish_images_ready(&images_dir) {
        return Err(job_error_detail(
            ErrorCode::NotFound,
            "Cuttlefish images not found",
            format!(
                "missing images under {}; run Install Cuttlefish or set AADK_CUTTLEFISH_IMAGES_DIR",
                images_dir.display()
            ),
            job_id,
        ));
    }

    if cuttlefish_cvd_path().is_none() && cuttlefish_launch_path(page_size).is_none() {
        return Err(job_error_detail(
            ErrorCode::NotFound,
            "Cuttlefish host tools not found",
            "install cuttlefish-base/cuttlefish-user or set AADK_LAUNCH_CVD_BIN".into(),
            job_id,
        ));
    }

    Ok(CuttlefishRuntime {
        page_size,
        home_dir,
        images_dir,
        host_dir,
    })
}

fn cuttlefish_start_command(
    runtime: &CuttlefishRuntime,
    show_full_ui: bool,
    job_id: &str,
) -> Result<String, ErrorDetail> {
    if let Some(cmd) = read_env_trimmed("AADK_CUTTLEFISH_START_CMD") {
        return Ok(cmd);
    }

    let start_args = read_env_trimmed("AADK_CUTTLEFISH_START_ARGS");
    let mut extra_args = start_args.unwrap_or_default();
    if let Some(mode) = cuttlefish_gpu_mode() {
        if !args_has_flag(&extra_args, "--gpu_mode") {
            if !extra_args.is_empty() {
                extra_args.push(' ');
            }
            extra_args.push_str("--gpu_mode=");
            extra_args.push_str(&mode);
        }
    }
    if !args_has_flag(&extra_args, "--start_webrtc") {
        if !extra_args.is_empty() {
            extra_args.push(' ');
        }
        extra_args.push_str("--start_webrtc=");
        extra_args.push_str(if show_full_ui { "true" } else { "false" });
    }
    let include_usage_stats = !extra_args.contains("report_anonymous_usage_stats");
    if std::env::consts::ARCH == "aarch64" && !extra_args.contains("enable_host_bluetooth") {
        if !extra_args.is_empty() {
            extra_args.push(' ');
        }
        extra_args.push_str("--enable_host_bluetooth=true");
    }
    if let Some(launch_path) = cuttlefish_launch_path(runtime.page_size) {
        let mut command = format!(
            "{}{} --daemon",
            cuttlefish_home_env_prefix(&runtime.home_dir),
            shell_escape(&launch_path.display().to_string())
        );
        command.push_str(&format!(
            " --system_image_dir={}",
            shell_escape(&runtime.images_dir.display().to_string())
        ));
        if include_usage_stats {
            command.push_str(" --report_anonymous_usage_stats=n");
        }
        if !extra_args.is_empty() {
            command.push(' ');
            command.push_str(&extra_args);
        }
        return Ok(command);
    }

    if let Some(cvd_path) = cuttlefish_cvd_path() {
        let mut command = format!(
            "{}{} create --host_path={} --product_path={}",
            cuttlefish_home_env_prefix(&runtime.home_dir),
            shell_escape(&cvd_path.display().to_string()),
            shell_escape(&runtime.host_dir.display().to_string()),
            shell_escape(&runtime.images_dir.display().to_string())
        );
        if include_usage_stats {
            command.push_str(" --report_anonymous_usage_stats=n");
        }
        if !extra_args.is_empty() {
            command.push(' ');
            command.push_str(&extra_args);
        }
        return Ok(command);
    }

    Err(job_error_detail(
        ErrorCode::NotFound,
        "no cuttlefish start command available",
        "set AADK_CUTTLEFISH_START_CMD or install Cuttlefish host tools".into(),
        job_id,
    ))
}

fn cuttlefish_stop_command(
    runtime: &CuttlefishRuntime,
    job_id: &str,
) -> Result<String, ErrorDetail> {
    if let Some(cmd) = read_env_trimmed("AADK_CUTTLEFISH_STOP_CMD") {
        return Ok(cmd);
    }

    if let Some(stop_path) = cuttlefish_stop_path(runtime.page_size) {
        return Ok(format!(
            "{}{}",
            cuttlefish_home_env_prefix(&runtime.home_dir),
            shell_escape(&stop_path.display().to_string())
        ));
    }

    if let Some(cvd_path) = cuttlefish_cvd_path() {
        return Ok(format!(
            "{}{} stop",
            cuttlefish_home_env_prefix(&runtime.home_dir),
            shell_escape(&cvd_path.display().to_string())
        ));
    }

    Err(job_error_detail(
        ErrorCode::NotFound,
        "no cuttlefish stop command available",
        "set AADK_CUTTLEFISH_STOP_CMD or install Cuttlefish host tools".into(),
        job_id,
    ))
}

async fn run_cuttlefish_start_job(job_id: String, show_full_ui: bool) {
    let mut job_client = match connect_job().await {
        Ok(client) => client,
        Err(err) => {
            warn!("cuttlefish job {job_id}: failed to connect job service: {err}");
            return;
        }
    };

    let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;
    if job_is_cancelled(&mut job_client, &job_id).await {
        let _ = publish_log(&mut job_client, &job_id, "Cuttlefish start cancelled before launch\n").await;
        return;
    }

    let _ = publish_state(&mut job_client, &job_id, JobState::Running).await;
    let _ = publish_log(&mut job_client, &job_id, "Starting Cuttlefish\n").await;
    let mut need_start = true;
    let mut adb_serial = cuttlefish_adb_serial();
    let _ = publish_progress(
        &mut job_client,
        &job_id,
        5,
        "checking cuttlefish",
        vec![
            metric("show_full_ui", show_full_ui),
            metric("adb_serial_hint", &adb_serial),
        ],
    )
    .await;
    match cuttlefish_status().await {
        Ok(status) => {
            if !status.adb_serial.is_empty() {
                adb_serial = status.adb_serial;
            }
            if status.running {
                need_start = false;
                let _ = publish_log(&mut job_client, &job_id, "Cuttlefish already running\n").await;
            }
        }
        Err(CuttlefishStatusError::NotInstalled) => {
            let detail = job_error_detail(
                ErrorCode::NotFound,
                "cuttlefish not installed",
                "install cuttlefish-base/cuttlefish-user or run Install Cuttlefish".into(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
        Err(CuttlefishStatusError::Failed(err)) => {
            let _ = publish_log(
                &mut job_client,
                &job_id,
                &format!("cuttlefish status failed (continuing): {err}\n"),
            )
            .await;
        }
    }
    if need_start && !adb_serial.trim().is_empty() {
        if adb_serial.contains(':') {
            let _ = adb_connect(&adb_serial).await;
        }
        if let Ok(state) = adb_get_state(&adb_serial).await {
            let normalized = state.trim();
            if normalized == "device" {
                need_start = false;
                let _ = publish_log(
                    &mut job_client,
                    &job_id,
                    &format!("adb state={normalized}; skipping start\n"),
                )
                .await;
            }
        }
    }

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Cuttlefish start cancelled\n").await;
        return;
    }

    let runtime = match cuttlefish_preflight(&mut job_client, &job_id, true, true).await {
        Ok(runtime) => runtime,
        Err(detail) => {
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Cuttlefish start cancelled\n").await;
        return;
    }

    let mut start_cmd = String::new();
    if need_start {
        cleanup_cuttlefish_temp();
        let command = match cuttlefish_start_command(&runtime, show_full_ui, &job_id) {
            Ok(command) => command,
            Err(detail) => {
                let _ = publish_failed(&mut job_client, &job_id, detail).await;
                return;
            }
        };
        start_cmd = command.clone();
        if cancel_requested(&cancel_rx) {
            let _ = publish_log(&mut job_client, &job_id, "Cuttlefish start cancelled\n").await;
            return;
        }
        let outcome = match run_cuttlefish_command(
            &mut job_client,
            &job_id,
            &command,
            "starting",
            40,
            Some(&runtime.host_dir),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(detail) => {
                let _ = publish_failed(&mut job_client, &job_id, detail).await;
                return;
            }
        };

        if !outcome.success {
            let mut recovered = false;
            if should_recover_bluetooth(&outcome.log) && command.contains("launch_cvd") {
                let _ = publish_log(
                    &mut job_client,
                    &job_id,
                    "Cuttlefish boot blocked by Bluetooth; attempting recovery\n",
                )
                .await;
                let recovery_cmd = append_arg_once(command.clone(), "--fail_fast=false");
                let _ = run_cuttlefish_command(
                    &mut job_client,
                    &job_id,
                    &recovery_cmd,
                    "recovering",
                    45,
                    Some(&runtime.host_dir),
                )
                .await;

                if let Some(serial) = wait_for_adb_device(60, std::time::Duration::from_secs(2)).await
                {
                    let _ = publish_log(
                        &mut job_client,
                        &job_id,
                        &format!("Enabling Bluetooth in guest via {serial}\n"),
                    )
                    .await;
                    enable_guest_bluetooth(&serial).await;
                } else {
                    let _ = publish_log(
                        &mut job_client,
                        &job_id,
                        "Bluetooth recovery could not find an ADB device\n",
                    )
                    .await;
                }

                if let Ok(stop_cmd) = cuttlefish_stop_command(&runtime, &job_id) {
                    let _ = run_cuttlefish_command(
                        &mut job_client,
                        &job_id,
                        &stop_cmd,
                        "stopping",
                        55,
                        Some(&runtime.host_dir),
                    )
                    .await;
                }

                cleanup_cuttlefish_temp();
                let retry_outcome = run_cuttlefish_command(
                    &mut job_client,
                    &job_id,
                    &command,
                    "restarting",
                    60,
                    Some(&runtime.host_dir),
                )
                .await;
                recovered = matches!(retry_outcome, Ok(outcome) if outcome.success);
            }

            if recovered {
                let _ = publish_log(&mut job_client, &job_id, "Bluetooth recovery succeeded\n").await;
            } else {
            let mut detail = if outcome.log.is_empty() {
                format!("exit_code={}", outcome.exit_code)
            } else {
                format!("exit_code={}\n{}", outcome.exit_code, outcome.log)
            };
            append_cuttlefish_diagnostics(&mut detail).await;
            let error = job_error_detail(ErrorCode::Internal, "cuttlefish start failed", detail, &job_id);
            let _ = publish_failed(&mut job_client, &job_id, error).await;
            return;
            }
        }
    }

    if adb_serial.contains(':') {
        let _ = adb_connect(&adb_serial).await;
    }

    let mut running = false;
    let max_attempts = 40; // ~80s total
    for attempt in 0..max_attempts {
        if let Some(serial) = adb_find_device_serial().await {
            adb_serial = serial;
            running = true;
            enable_guest_bluetooth(&adb_serial).await;
            break;
        }
        match adb_get_state(&adb_serial).await {
            Ok(state) => {
                let normalized = state.trim();
                if normalized == "device" {
                    running = true;
                    // Try to force-enable Bluetooth as soon as the guest is reachable.
                    enable_guest_bluetooth(&adb_serial).await;
                    break;
                }
                let _ = publish_log(
                    &mut job_client,
                    &job_id,
                    &format!("adb state={normalized} (attempt {})\n", attempt + 1),
                )
                .await;
            }
            Err(err) => {
                let _ = publish_log(
                    &mut job_client,
                    &job_id,
                    &format!("adb get-state failed: {}\n", adb_failure_message(&err)),
                )
                .await;
            }
        }

        if attempt < max_attempts - 1 {
            let _ = publish_progress(
                &mut job_client,
                &job_id,
                70,
                "waiting for device",
                vec![
                    metric("attempt", attempt + 1),
                    metric("max_attempts", max_attempts),
                    metric("adb_serial", &adb_serial),
                ],
            )
            .await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    if !running {
        let mut detail = format!("adb_serial={adb_serial}");
        append_cuttlefish_diagnostics(&mut detail).await;
        let error = job_error_detail(
            ErrorCode::TargetNotReachable,
            "cuttlefish not reachable via adb",
            detail,
            &job_id,
        );
        let _ = publish_failed(&mut job_client, &job_id, error).await;
        return;
    }

    let mut outputs = Vec::new();
    outputs.push(KeyValue { key: "adb_serial".into(), value: adb_serial });
    outputs.push(KeyValue { key: "show_full_ui".into(), value: show_full_ui.to_string() });
    outputs.push(KeyValue { key: "webrtc_url".into(), value: cuttlefish_web_url() });
    outputs.push(KeyValue { key: "env_url".into(), value: cuttlefish_env_url() });
    outputs.push(KeyValue { key: "home_dir".into(), value: runtime.home_dir.display().to_string() });
    outputs.push(KeyValue { key: "images_dir".into(), value: runtime.images_dir.display().to_string() });
    outputs.push(KeyValue { key: "host_dir".into(), value: runtime.host_dir.display().to_string() });
    if let Some(mode) = cuttlefish_gpu_mode() {
        outputs.push(KeyValue { key: "gpu_mode".into(), value: mode });
    }
    if !start_cmd.is_empty() {
        outputs.push(KeyValue { key: "start_command".into(), value: start_cmd });
    }

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Cuttlefish start cancelled\n").await;
        return;
    }

    let _ = publish_completed(&mut job_client, &job_id, "Cuttlefish ready", outputs).await;
}

async fn run_cuttlefish_stop_job(job_id: String) {
    let mut job_client = match connect_job().await {
        Ok(client) => client,
        Err(err) => {
            warn!("cuttlefish stop {job_id}: failed to connect job service: {err}");
            return;
        }
    };

    let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;
    if job_is_cancelled(&mut job_client, &job_id).await {
        let _ = publish_log(&mut job_client, &job_id, "Cuttlefish stop cancelled before start\n").await;
        return;
    }

    let _ = publish_state(&mut job_client, &job_id, JobState::Running).await;
    let _ = publish_log(&mut job_client, &job_id, "Stopping Cuttlefish\n").await;
    let adb_serial = cuttlefish_adb_serial();
    let _ = publish_progress(
        &mut job_client,
        &job_id,
        10,
        "stopping",
        vec![metric("adb_serial_hint", adb_serial)],
    )
    .await;

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Cuttlefish stop cancelled\n").await;
        return;
    }

    let runtime = match cuttlefish_preflight(&mut job_client, &job_id, false, false).await {
        Ok(runtime) => runtime,
        Err(detail) => {
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Cuttlefish stop cancelled\n").await;
        return;
    }

    let command = match cuttlefish_stop_command(&runtime, &job_id) {
        Ok(command) => command,
        Err(detail) => {
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Cuttlefish stop cancelled\n").await;
        return;
    }

    let outcome = match run_cuttlefish_command(
        &mut job_client,
        &job_id,
        &command,
        "stopping",
        40,
        Some(&runtime.host_dir),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(detail) => {
            let _ = publish_failed(&mut job_client, &job_id, detail).await;
            return;
        }
    };

    if !outcome.success {
        let mut detail = if outcome.log.is_empty() {
            format!("exit_code={}", outcome.exit_code)
        } else {
            format!("exit_code={}\n{}", outcome.exit_code, outcome.log)
        };
        append_cuttlefish_diagnostics(&mut detail).await;
        let error = job_error_detail(ErrorCode::Internal, "cuttlefish stop failed", detail, &job_id);
        let _ = publish_failed(&mut job_client, &job_id, error).await;
        return;
    }

    let mut outputs = Vec::new();
    outputs.push(KeyValue { key: "stop_command".into(), value: command });
    outputs.push(KeyValue { key: "home_dir".into(), value: runtime.home_dir.display().to_string() });

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Cuttlefish stop cancelled\n").await;
        return;
    }

    let _ = publish_completed(&mut job_client, &job_id, "Cuttlefish stopped", outputs).await;
}

async fn run_cuttlefish_install_job(job_id: String, options: CuttlefishInstallOptions) {
    let mut job_client = match connect_job().await {
        Ok(client) => client,
        Err(err) => {
            warn!("cuttlefish install {job_id}: failed to connect job service: {err}");
            return;
        }
    };

    let cancel_rx = spawn_cancel_watcher(job_id.clone()).await;
    if job_is_cancelled(&mut job_client, &job_id).await {
        let _ = publish_log(&mut job_client, &job_id, "Cuttlefish install cancelled before start\n").await;
        return;
    }

    let _ = publish_state(&mut job_client, &job_id, JobState::Running).await;
    let _ = publish_log(&mut job_client, &job_id, "Installing Cuttlefish\n").await;

    let page_size = host_page_size();
    let home_dir = cuttlefish_home_dir(page_size);
    let images_dir = cuttlefish_images_dir(page_size);
    let host_dir = cuttlefish_host_dir(page_size);

    let host_installed = cuttlefish_cvd_path().is_some() || cuttlefish_launch_path(page_size).is_some();
    let images_ready = cuttlefish_images_ready(&images_dir);
    let kvm_status = kvm_status();
    let _ = publish_progress(
        &mut job_client,
        &job_id,
        5,
        "checking environment",
        vec![
            metric("page_size", page_size.unwrap_or_default()),
            metric("home_dir", home_dir.display()),
            metric("images_dir", images_dir.display()),
            metric("host_dir", host_dir.display()),
            metric("host_installed", host_installed),
            metric("images_ready", images_ready),
            metric("kvm_present", kvm_status.present),
            metric("kvm_accessible", kvm_status.accessible),
        ],
    )
    .await;

    if cuttlefish_kvm_check_enabled() {
        let _ = publish_log(
            &mut job_client,
            &job_id,
            &format!(
                "KVM check: present={} accessible={}\n",
                kvm_status.present, kvm_status.accessible
            ),
        )
        .await;
        if !kvm_status.present {
            let _ = publish_log(
                &mut job_client,
                &job_id,
                "KVM device not found (/dev/kvm). Cuttlefish will not run until virtualization is enabled.\n",
            )
            .await;
        } else if !kvm_status.accessible {
            let detail = kvm_status
                .detail
                .as_deref()
                .unwrap_or("failed to open /dev/kvm");
            let _ = publish_log(
                &mut job_client,
                &job_id,
                &format!(
                    "KVM access denied: {detail}. Add the user to the kvm group and re-login.\n"
                ),
            )
            .await;
        }
    } else {
        let _ = publish_log(
            &mut job_client,
            &job_id,
            "Skipping KVM check (AADK_CUTTLEFISH_KVM_CHECK=0)\n",
        )
        .await;
    }

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Cuttlefish install cancelled\n").await;
        return;
    }

    let install_host = match std::env::var("AADK_CUTTLEFISH_INSTALL_HOST") {
        Ok(val) => !(val == "0" || val.eq_ignore_ascii_case("false")),
        Err(_) => true,
    };
    let install_images = match std::env::var("AADK_CUTTLEFISH_INSTALL_IMAGES") {
        Ok(val) => !(val == "0" || val.eq_ignore_ascii_case("false")),
        Err(_) => true,
    };
    let add_groups = match std::env::var("AADK_CUTTLEFISH_ADD_GROUPS") {
        Ok(val) => !(val == "0" || val.eq_ignore_ascii_case("false")),
        Err(_) => true,
    };

    let sudo_prefix = sudo_prefix();

    if install_host && (!host_installed || options.force) {
        let install_cmd = if let Some(cmd) = read_env_trimmed("AADK_CUTTLEFISH_INSTALL_CMD") {
            cmd
        } else {
            let installer_path = if let Some(path) = find_command("apt-get") {
                path
            } else if let Some(path) = find_command("apt") {
                path
            } else {
                let detail = job_error_detail(
                    ErrorCode::NotFound,
                    "no supported package manager found",
                    "install cuttlefish manually or set AADK_CUTTLEFISH_INSTALL_CMD".into(),
                    &job_id,
                );
                let _ = publish_failed(&mut job_client, &job_id, detail).await;
                return;
            };

            let sudo_prefix = match sudo_prefix.as_ref() {
                Some(prefix) => prefix.as_str(),
                None => {
                    let detail = job_error_detail(
                        ErrorCode::PermissionDenied,
                        "sudo required for cuttlefish install",
                        "install sudo or run aadk-targets as root (or set AADK_CUTTLEFISH_INSTALL_CMD)".into(),
                        &job_id,
                    );
                    let _ = publish_failed(&mut job_client, &job_id, detail).await;
                    return;
                }
            };

            let installer = installer_path.display();
            let repo_key = "https://us-apt.pkg.dev/doc/repo-signing-key.gpg";
            let repo_line = "deb https://us-apt.pkg.dev/projects/android-cuttlefish-artifacts android-cuttlefish main";
            format!(
                "{sudo_prefix}{installer} update && {sudo_prefix}{installer} install -y curl ca-certificates unzip tar python3 && {sudo_prefix}curl -fsSL {repo_key} -o /etc/apt/trusted.gpg.d/artifact-registry.asc && {sudo_prefix}chmod a+r /etc/apt/trusted.gpg.d/artifact-registry.asc && {sudo_prefix}sh -lc \"echo '{repo_line}' > /etc/apt/sources.list.d/artifact-registry.list\" && {sudo_prefix}{installer} update && {sudo_prefix}{installer} install -y cuttlefish-base cuttlefish-user"
            )
        };

        let _ = publish_log(&mut job_client, &job_id, &format!("Install command: {install_cmd}\n")).await;
        let _ = publish_progress(
            &mut job_client,
            &job_id,
            30,
            "installing host tools",
            vec![
                metric("force", options.force),
                metric("install_cmd", &install_cmd),
            ],
        )
        .await;

        if cancel_requested(&cancel_rx) {
            let _ = publish_log(&mut job_client, &job_id, "Cuttlefish install cancelled\n").await;
            return;
        }

        match run_shell_command(&install_cmd).await {
            Ok((true, _, log)) => {
                if !log.is_empty() {
                    let _ = publish_log(&mut job_client, &job_id, &log).await;
                }
            }
            Ok((false, code, log)) => {
                if !log.is_empty() {
                    let _ = publish_log(&mut job_client, &job_id, &log).await;
                }
                let detail = if log.is_empty() {
                    format!("exit_code={code}")
                } else {
                    format!("exit_code={code}\n{log}")
                };
                let error = job_error_detail(classify_install_error(&detail), "Cuttlefish install failed", detail, &job_id);
                let _ = publish_failed(&mut job_client, &job_id, error).await;
                return;
            }
            Err(err) => {
                let error = job_error_detail(ErrorCode::Internal, "failed to run install command", err.to_string(), &job_id);
                let _ = publish_failed(&mut job_client, &job_id, error).await;
                return;
            }
        }
    } else if !install_host {
        let _ = publish_log(&mut job_client, &job_id, "Host install disabled (AADK_CUTTLEFISH_INSTALL_HOST=0)\n").await;
    } else {
        let _ = publish_log(&mut job_client, &job_id, "Host tools already installed; skipping host install\n").await;
    }

    let required_groups = ["kvm", "cvdnetwork", "render"];
    let missing = match current_user_groups().await {
        Some(groups) => {
            let missing = missing_groups(&groups, &required_groups);
            if missing.is_empty() {
                let _ = publish_log(
                    &mut job_client,
                    &job_id,
                    "User already in kvm/cvdnetwork/render groups\n",
                )
                .await;
            } else {
                let _ = publish_log(
                    &mut job_client,
                    &job_id,
                    &format!("Missing groups: {}\n", missing.join(",")),
                )
                .await;
            }
            missing
        }
        None => {
            let _ = publish_log(
                &mut job_client,
                &job_id,
                "Unable to determine user groups; assuming group setup is needed\n",
            )
            .await;
            required_groups.iter().map(|g| g.to_string()).collect()
        }
    };

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Cuttlefish install cancelled\n").await;
        return;
    }

    if add_groups {
        if missing.is_empty() {
            let _ = publish_log(
                &mut job_client,
                &job_id,
                "Group setup not required; skipping usermod\n",
            )
            .await;
        } else if let (Some(prefix), Ok(user)) = (sudo_prefix.as_ref(), std::env::var("USER")) {
            if !user.trim().is_empty() {
                let group_cmd = format!("{prefix}usermod -aG kvm,cvdnetwork,render {user}");
                let _ = publish_log(
                    &mut job_client,
                    &job_id,
                    "Adding user to kvm/cvdnetwork/render groups\n",
                )
                .await;
                let _ = run_shell_command(&group_cmd).await;
                let _ = publish_log(
                    &mut job_client,
                    &job_id,
                    "Re-login or reboot may be required for group changes to take effect\n",
                )
                .await;
            }
        } else {
            let _ = publish_log(
                &mut job_client,
                &job_id,
                "Skipping group setup; sudo unavailable\n",
            )
            .await;
        }
    } else if !missing.is_empty() {
        let _ = publish_log(
            &mut job_client,
            &job_id,
            "Group setup disabled (AADK_CUTTLEFISH_ADD_GROUPS=0)\n",
        )
        .await;
    }

    if install_images && (!images_ready || options.force) {
        if cancel_requested(&cancel_rx) {
            let _ = publish_log(&mut job_client, &job_id, "Cuttlefish install cancelled\n").await;
            return;
        }
        let config = resolve_cuttlefish_request_config(
            page_size,
            options.branch.clone(),
            options.target.clone(),
            options.build_id.clone(),
        );
        let branch = config.branch;
        let target = config.target;
        let branch_override = config.has_branch_override;
        let target_override = config.has_target_override;
        let build_id_override = config.build_id_override;

        let mut candidates = vec![(branch.clone(), target.clone())];
        if build_id_override.is_none() && !branch_override && !target_override {
            if let Some((fallback_branch, fallback_target)) =
                cuttlefish_fallback_branch_target(page_size)
            {
                if fallback_branch != branch || fallback_target != target {
                    candidates.push((fallback_branch, fallback_target));
                }
            }
        }

        let mut resolved_branch = None;
        let mut resolved_target = None;
        let mut build_info = None;

        for (candidate_branch, candidate_target) in candidates {
            match resolve_build_info(
                &candidate_branch,
                &candidate_target,
                build_id_override.clone(),
            )
            .await
            {
                Ok(info) => {
                    resolved_branch = Some(candidate_branch);
                    resolved_target = Some(candidate_target);
                    build_info = Some(info);
                    break;
                }
                Err(err) => {
                    let _ = publish_log(
                        &mut job_client,
                        &job_id,
                        &format!(
                            "Cuttlefish build not available for branch={candidate_branch} target={candidate_target}: {err}\n"
                        ),
                    )
                    .await;
                }
            }
        }

        let Some(build_info) = build_info else {
            let error = job_error_detail(
                ErrorCode::Internal,
                "failed to resolve Cuttlefish build",
                "no viable build artifacts found".to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, error).await;
            return;
        };

        let branch = resolved_branch.unwrap_or(branch);
        let target = resolved_target.unwrap_or(target);

        let build_id = build_info.build_id;
        let product = build_info.product;

        let _ = publish_log(
            &mut job_client,
            &job_id,
            &format!(
                "Resolved build: branch={branch} target={target} build_id={build_id} product={product}\n"
            ),
        )
        .await;

        let img_candidates = cuttlefish_image_artifact_candidates(&product, &target, &build_id);
        let host_candidates = cuttlefish_host_artifact_candidates(&build_id);
        let target_paths = candidate_target_paths(&target, &product);

        let img_url = match resolve_artifact_url_for_targets(
            &build_id,
            &target_paths,
            &img_candidates,
        )
        .await
        {
            Ok(url) => url,
            Err(err) => {
                let error = job_error_detail(
                    ErrorCode::NotFound,
                    "failed to locate Cuttlefish image artifact",
                    err,
                    &job_id,
                );
                let _ = publish_failed(&mut job_client, &job_id, error).await;
                return;
            }
        };

        let host_url = match resolve_artifact_url_for_targets(
            &build_id,
            &target_paths,
            &host_candidates,
        )
        .await
        {
            Ok(url) => url,
            Err(err) => {
                let error = job_error_detail(
                    ErrorCode::NotFound,
                    "failed to locate Cuttlefish host package",
                    err,
                    &job_id,
                );
                let _ = publish_failed(&mut job_client, &job_id, error).await;
                return;
            }
        };

        let _ = publish_log(
            &mut job_client,
            &job_id,
            &format!("Image artifact: {img_url}\nHost artifact: {host_url}\n"),
        )
        .await;

        let downloads_dir = data_dir().join("cuttlefish").join("downloads").join(&build_id);
        let _ = std::fs::create_dir_all(&downloads_dir);

        let img_artifact = img_url
            .split('/')
            .last()
            .unwrap_or("cuttlefish-img.zip")
            .to_string();
        let host_artifact = host_url
            .split('/')
            .last()
            .unwrap_or("cvd-host_package.tar.gz")
            .to_string();

        let img_path = downloads_dir.join(&img_artifact);
        let host_path = downloads_dir.join(&host_artifact);

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            55,
            "downloading images",
            vec![
                metric("branch", &branch),
                metric("target", &target),
                metric("build_id", &build_id),
                metric("product", &product),
                metric("image_url", &img_url),
                metric("image_path", img_path.display()),
            ],
        )
        .await;
        let img_cmd = format!(
            "curl -fL {} -o {}",
            shell_escape(&img_url),
            shell_escape(&img_path.display().to_string())
        );
        if cancel_requested(&cancel_rx) {
            let _ = publish_log(&mut job_client, &job_id, "Cuttlefish install cancelled\n").await;
            return;
        }
        if let Err(err) = run_shell_command(&img_cmd).await {
            let error = job_error_detail(
                ErrorCode::Internal,
                "failed to download system images",
                err.to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, error).await;
            return;
        }

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            60,
            "downloading host package",
            vec![
                metric("branch", &branch),
                metric("target", &target),
                metric("build_id", &build_id),
                metric("product", &product),
                metric("host_url", &host_url),
                metric("host_path", host_path.display()),
            ],
        )
        .await;
        let host_cmd = format!(
            "curl -fL {} -o {}",
            shell_escape(&host_url),
            shell_escape(&host_path.display().to_string())
        );
        if cancel_requested(&cancel_rx) {
            let _ = publish_log(&mut job_client, &job_id, "Cuttlefish install cancelled\n").await;
            return;
        }
        if let Err(err) = run_shell_command(&host_cmd).await {
            let error = job_error_detail(
                ErrorCode::Internal,
                "failed to download host package",
                err.to_string(),
                &job_id,
            );
            let _ = publish_failed(&mut job_client, &job_id, error).await;
            return;
        }

        let _ = std::fs::create_dir_all(&images_dir);
        let _ = std::fs::create_dir_all(&host_dir);

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            70,
            "extracting images",
            vec![
                metric("image_path", img_path.display()),
                metric("images_dir", images_dir.display()),
            ],
        )
        .await;
        let unzip_cmd = format!(
            "unzip -o {} -d {}",
            shell_escape(&img_path.display().to_string()),
            shell_escape(&images_dir.display().to_string())
        );
        if cancel_requested(&cancel_rx) {
            let _ = publish_log(&mut job_client, &job_id, "Cuttlefish install cancelled\n").await;
            return;
        }
        match run_shell_command(&unzip_cmd).await {
            Ok((true, _, log)) => {
                if !log.is_empty() {
                    let _ = publish_log(&mut job_client, &job_id, &log).await;
                }
            }
            Ok((false, code, log)) => {
                let detail = if log.is_empty() {
                    format!("exit_code={code}")
                } else {
                    format!("exit_code={code}\n{log}")
                };
                let error = job_error_detail(ErrorCode::Internal, "failed to extract images", detail, &job_id);
                let _ = publish_failed(&mut job_client, &job_id, error).await;
                return;
            }
            Err(err) => {
                let error = job_error_detail(ErrorCode::Internal, "failed to extract images", err.to_string(), &job_id);
                let _ = publish_failed(&mut job_client, &job_id, error).await;
                return;
            }
        }

        let _ = publish_progress(
            &mut job_client,
            &job_id,
            80,
            "extracting host tools",
            vec![
                metric("host_path", host_path.display()),
                metric("host_dir", host_dir.display()),
            ],
        )
        .await;
        let tar_cmd = format!(
            "tar -xzf {} -C {}",
            shell_escape(&host_path.display().to_string()),
            shell_escape(&host_dir.display().to_string())
        );
        if cancel_requested(&cancel_rx) {
            let _ = publish_log(&mut job_client, &job_id, "Cuttlefish install cancelled\n").await;
            return;
        }
        match run_shell_command(&tar_cmd).await {
            Ok((true, _, log)) => {
                if !log.is_empty() {
                    let _ = publish_log(&mut job_client, &job_id, &log).await;
                }
            }
            Ok((false, code, log)) => {
                let detail = if log.is_empty() {
                    format!("exit_code={code}")
                } else {
                    format!("exit_code={code}\n{log}")
                };
                let error = job_error_detail(ErrorCode::Internal, "failed to extract host tools", detail, &job_id);
                let _ = publish_failed(&mut job_client, &job_id, error).await;
                return;
            }
            Err(err) => {
                let error = job_error_detail(ErrorCode::Internal, "failed to extract host tools", err.to_string(), &job_id);
                let _ = publish_failed(&mut job_client, &job_id, error).await;
                return;
            }
        }
    } else if !install_images {
        let _ = publish_log(&mut job_client, &job_id, "Image install disabled (AADK_CUTTLEFISH_INSTALL_IMAGES=0)\n").await;
    } else {
        let _ = publish_log(&mut job_client, &job_id, "Images already present; skipping download\n").await;
    }

    if cancel_requested(&cancel_rx) {
        let _ = publish_log(&mut job_client, &job_id, "Cuttlefish install cancelled\n").await;
        return;
    }

    let mut outputs = Vec::new();
    outputs.push(KeyValue { key: "force".into(), value: options.force.to_string() });
    outputs.push(KeyValue { key: "home_dir".into(), value: home_dir.display().to_string() });
    outputs.push(KeyValue { key: "images_dir".into(), value: images_dir.display().to_string() });
    outputs.push(KeyValue { key: "host_dir".into(), value: host_dir.display().to_string() });
    outputs.push(KeyValue { key: "install_host".into(), value: install_host.to_string() });
    outputs.push(KeyValue { key: "install_images".into(), value: install_images.to_string() });
    outputs.push(KeyValue { key: "kvm_present".into(), value: kvm_status.present.to_string() });
    outputs.push(KeyValue { key: "kvm_access".into(), value: kvm_status.accessible.to_string() });
    if let Some(detail) = kvm_status.detail {
        outputs.push(KeyValue { key: "kvm_detail".into(), value: detail });
    }

    let _ = publish_completed(&mut job_client, &job_id, "Cuttlefish installed", outputs).await;
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
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

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
            target_id: Some(Id { value: target_id.to_string() }),
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
                .find(|entry| normalize_target_id_for_compare(&entry.target.target_id) == target_key)
                .map(|entry| entry.to_target(Some("offline")));
        }
        let Some(chosen) = chosen else {
            return Ok(Response::new(SetDefaultTargetResponse { ok: false }));
        };
        st.default_target = Some(chosen);
        if let Some(target) = st.default_target.clone() {
            upsert_inventory_entries(&mut st.inventory, std::slice::from_ref(&target), now_millis());
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
                    upsert_inventory_entries(&mut st.inventory, std::slice::from_ref(&found), now_millis());
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
                    .find(|entry| normalize_target_id_for_compare(&entry.target.target_id) == target_key)
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
            .unwrap_or_else(String::new);
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "targets.install",
                vec![KeyValue { key: "apk_path".into(), value: apk_path.clone() }],
                req.project_id,
                Some(Id { value: target_id.clone() }),
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
            .unwrap_or_else(String::new);
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "targets.launch",
                vec![KeyValue { key: "application_id".into(), value: application_id.clone() }],
                None,
                Some(Id { value: target_id.clone() }),
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
            .unwrap_or_else(String::new);
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "targets.stop",
                vec![KeyValue { key: "application_id".into(), value: application_id.clone() }],
                None,
                Some(Id { value: target_id.clone() }),
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

        let branch_override = if branch.is_empty() { None } else { Some(branch) };
        let target_override = if target.is_empty() { None } else { Some(target) };
        let build_id_override = if build_id.is_empty() { None } else { Some(build_id) };

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
            .unwrap_or_else(String::new);
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "targets.cuttlefish.install",
                params,
                None,
                None,
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
            .unwrap_or_else(String::new);
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
            .unwrap_or_else(String::new);
        let job_id = if job_id.is_empty() {
            start_job(
                &mut job_client,
                "targets.cuttlefish.stop",
                vec![],
                None,
                None,
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
                    details.push(KeyValue { key: format!("cuttlefish_{key}"), value });
                }
                if !status.raw.is_empty() {
                    details.push(KeyValue { key: "cuttlefish_status_raw".into(), value: status.raw });
                }
                if status.running {
                    "running".to_string()
                } else {
                    "stopped".to_string()
                }
            }
            Err(CuttlefishStatusError::NotInstalled) => "not_installed".to_string(),
            Err(CuttlefishStatusError::Failed(err)) => {
                details.push(KeyValue { key: "cuttlefish_status_error".into(), value: err });
                "error".to_string()
            }
        };

        let adb_serial = normalize_target_id(&adb_serial);
        let adb_serial_canonical = canonicalize_adb_serial(&adb_serial);
        if !adb_serial.is_empty() {
            match adb_get_state(&adb_serial_canonical).await {
                Ok(adb_state) => details.push(KeyValue { key: "adb_state".into(), value: adb_state }),
                Err(err) => details.push(KeyValue { key: "adb_state_error".into(), value: adb_failure_message(&err) }),
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let addr_str =
        std::env::var("AADK_TARGETS_ADDR").unwrap_or_else(|_| "127.0.0.1:50055".to_string());
    let addr: SocketAddr = addr_str.parse()?;

    let svc = Svc::default();
    info!("aadk-targets listening on {}", addr);

    tonic::transport::Server::builder()
        .add_service(TargetServiceServer::new(svc))
        .serve(addr)
        .await?;

    Ok(())
}
