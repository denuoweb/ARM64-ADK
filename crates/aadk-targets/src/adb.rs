use std::{io, path::PathBuf, process::Output};

use aadk_proto::aadk::v1::{Id, KeyValue, Target, TargetKind};
use tokio::process::Command;

use crate::ids::{canonicalize_adb_serial, normalize_target_id};

#[derive(Debug)]
pub(crate) enum AdbFailure {
    NotFound,
    Io(String),
    Exit {
        status: i32,
        stdout: String,
        stderr: String,
    },
}

pub(crate) fn adb_path() -> PathBuf {
    if let Ok(path) = std::env::var("AADK_ADB_PATH") {
        return PathBuf::from(path);
    }
    if let Ok(path) = std::env::var("ADB_PATH") {
        return PathBuf::from(path);
    }
    if let Ok(sdk_root) =
        std::env::var("ANDROID_SDK_ROOT").or_else(|_| std::env::var("ANDROID_HOME"))
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
    let candidate = crate::cuttlefish::cuttlefish_host_dir(crate::cuttlefish::host_page_size())
        .join("bin")
        .join("adb");
    if candidate.is_file() {
        return candidate;
    }
    PathBuf::from("adb")
}

pub(crate) async fn adb_output(args: &[&str]) -> Result<Output, AdbFailure> {
    let mut cmd = Command::new(adb_path());
    cmd.args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
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

pub(crate) fn format_adb_output(stdout: &str, stderr: &str) -> String {
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

pub(crate) fn adb_failure_message(err: &AdbFailure) -> String {
    match err {
        AdbFailure::NotFound => "adb not found (set AADK_ADB_PATH or ANDROID_SDK_ROOT)".into(),
        AdbFailure::Io(msg) => msg.clone(),
        AdbFailure::Exit {
            status,
            stdout,
            stderr,
        } => format_adb_failure_message(*status, stdout, stderr),
    }
}

pub(crate) fn adb_failure_status(err: AdbFailure) -> tonic::Status {
    match err {
        AdbFailure::NotFound => tonic::Status::failed_precondition(
            "adb not found (set AADK_ADB_PATH or ANDROID_SDK_ROOT)",
        ),
        AdbFailure::Io(msg) => tonic::Status::internal(format!("adb failed: {msg}")),
        AdbFailure::Exit {
            status,
            stdout,
            stderr,
        } => tonic::Status::unavailable(format_adb_failure_message(status, &stdout, &stderr)),
    }
}

pub(crate) async fn adb_get_state(serial: &str) -> Result<String, AdbFailure> {
    let args = ["-s", serial, "get-state"];
    let output = adb_output(&args).await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) fn classify_target_kind(serial: &str) -> TargetKind {
    if serial.starts_with("emulator-") {
        TargetKind::Emulatorlike
    } else if serial.contains(':') {
        TargetKind::Remote
    } else {
        TargetKind::Device
    }
}

pub(crate) fn health_state_from_adb_state(state: &str) -> &'static str {
    match state {
        "device" => "online",
        "unauthorized" => "unauthorized",
        "offline" => "offline",
        "recovery" => "recovery",
        "bootloader" => "bootloader",
        _ => "unknown",
    }
}

pub(crate) async fn adb_connect(addr: &str) -> Option<String> {
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

pub(crate) async fn adb_get_prop(serial: &str, prop: &str) -> Result<String, AdbFailure> {
    let args = ["-s", serial, "shell", "getprop", prop];
    let output = adb_output(&args).await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) async fn adb_shell(serial: &str, cmd: &str) -> Result<(), AdbFailure> {
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

pub(crate) async fn adb_get_prop_timeout(serial: &str, prop: &str) -> Option<String> {
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
pub(crate) struct TargetProps {
    pub(crate) api_level: Option<String>,
    pub(crate) release: Option<String>,
    pub(crate) abi: Option<String>,
    pub(crate) abi_list: Option<String>,
    pub(crate) manufacturer: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) device: Option<String>,
    pub(crate) product: Option<String>,
    pub(crate) boot_completed: Option<String>,
}

pub(crate) async fn adb_collect_props(serial: &str) -> TargetProps {
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
            details.push(KeyValue {
                key: "product".into(),
                value,
            });
        }
        if let Some(value) = model.clone() {
            details.push(KeyValue {
                key: "model".into(),
                value,
            });
        }
        if let Some(value) = device {
            details.push(KeyValue {
                key: "device".into(),
                value,
            });
        }
        if let Some(value) = transport_id {
            details.push(KeyValue {
                key: "transport_id".into(),
                value,
            });
        }
        for (key, value) in extra {
            details.push(KeyValue { key, value });
        }
        details.push(KeyValue {
            key: "adb_state".into(),
            value: state.into(),
        });

        targets.push(Target {
            target_id: Some(Id {
                value: normalized.clone(),
            }),
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

pub(crate) async fn enrich_adb_targets(targets: &mut [Target]) {
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

pub(crate) async fn list_adb_targets(include_offline: bool) -> Result<Vec<Target>, tonic::Status> {
    let output = adb_output(&["devices", "-l"])
        .await
        .map_err(adb_failure_status)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut targets = parse_adb_devices(&stdout, include_offline);
    enrich_adb_targets(&mut targets).await;
    Ok(targets)
}

pub(crate) async fn adb_list_devices(include_offline: bool) -> Result<Vec<Target>, AdbFailure> {
    let output = adb_output(&["devices", "-l"]).await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_adb_devices(&stdout, include_offline))
}

pub(crate) async fn adb_find_device_serial() -> Option<String> {
    let devices = adb_list_devices(false).await.ok()?;
    devices.first().map(|t| t.address.clone())
}

pub(crate) async fn wait_for_adb_device(
    max_attempts: usize,
    delay: std::time::Duration,
) -> Option<String> {
    for _ in 0..max_attempts {
        if let Some(serial) = adb_find_device_serial().await {
            return Some(serial);
        }
        tokio::time::sleep(delay).await;
    }
    None
}
