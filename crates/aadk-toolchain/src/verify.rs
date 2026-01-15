use std::{fs, io, path::Path};

use aadk_proto::aadk::v1::{InstalledToolchain, ToolchainKind};
use base64::engine::general_purpose;
use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use reqwest::Client;
use serde::Deserialize;

use crate::artifacts::{is_remote_url, local_artifact_path};
use crate::catalog::{catalog_artifact_for_provenance, Catalog, CatalogArtifact};
use crate::hashing::sha256_file_bytes;
use crate::provenance::Provenance;
use crate::state::{
    provider_id_from_installed, provider_name_from_installed, version_from_installed,
};

#[derive(Clone, Default)]
pub(crate) struct SignatureRecord {
    pub(crate) signature: String,
    pub(crate) signature_url: String,
    pub(crate) public_key: String,
}

#[derive(Clone, Default)]
pub(crate) struct TransparencyLogRecord {
    pub(crate) entry: String,
    pub(crate) entry_url: String,
    pub(crate) public_key: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct TransparencyLogEntryJson {
    sha256: String,
    signature: String,
    log_id: String,
    log_index: Option<u64>,
    timestamp_unix_millis: Option<i64>,
}

#[derive(Clone, Default)]
pub(crate) struct TransparencyLogEntry {
    pub(crate) sha256: String,
    pub(crate) signature: String,
    pub(crate) log_id: String,
    pub(crate) log_index: Option<u64>,
    pub(crate) timestamp_unix_millis: Option<i64>,
}

pub(crate) fn normalize_signature_value(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

pub(crate) fn signature_record_from_catalog(artifact: &CatalogArtifact) -> Option<SignatureRecord> {
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

pub(crate) fn signature_record_from_provenance(prov: &Provenance) -> Option<SignatureRecord> {
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

pub(crate) fn transparency_record_from_catalog(
    artifact: &CatalogArtifact,
) -> Option<TransparencyLogRecord> {
    let entry = artifact.transparency_log_entry.trim().to_string();
    let entry_url = artifact.transparency_log_entry_url.trim().to_string();
    let public_key = artifact.transparency_log_public_key.trim().to_string();
    if entry.is_empty() && entry_url.is_empty() {
        return None;
    }
    Some(TransparencyLogRecord {
        entry,
        entry_url,
        public_key,
    })
}

pub(crate) fn transparency_record_from_provenance(
    prov: &Provenance,
) -> Option<TransparencyLogRecord> {
    let entry = prov.transparency_log_entry.trim().to_string();
    let entry_url = prov.transparency_log_entry_url.trim().to_string();
    let public_key = prov.transparency_log_public_key.trim().to_string();
    if entry.is_empty() && entry_url.is_empty() {
        return None;
    }
    Some(TransparencyLogRecord {
        entry,
        entry_url,
        public_key,
    })
}

pub(crate) async fn verify_transparency_log(
    record: &TransparencyLogRecord,
    expected_sha256: &str,
) -> Result<(TransparencyLogEntry, String), String> {
    let raw = resolve_transparency_entry(record).await?;
    let normalized = raw.trim().to_string();
    let entry = parse_transparency_entry(&normalized)?;
    let expected = normalize_digest_value(expected_sha256);
    if !expected.is_empty() && entry.sha256 != expected {
        return Err(format!(
            "transparency log sha256 mismatch (expected {expected}, got {})",
            entry.sha256
        ));
    }
    let has_signature = !entry.signature.trim().is_empty() || !record.public_key.trim().is_empty();
    if has_signature {
        if entry.signature.trim().is_empty() {
            return Err("transparency log signature missing".into());
        }
        if record.public_key.trim().is_empty() {
            return Err("transparency log public key missing".into());
        }
        let digest_bytes = decode_hex(&entry.sha256)
            .map_err(|e| format!("transparency log digest invalid: {e}"))?;
        verify_ed25519_signature_bytes(&digest_bytes, &entry.signature, &record.public_key)?;
    }
    Ok((entry, normalized))
}

pub(crate) async fn verify_signature_if_configured(
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

pub(crate) fn verify_provenance_entry(
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
    let entry_version = version_from_installed(entry)
        .ok_or_else(|| "installed entry missing version".to_string())?;
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
    if !prov.sha256.trim().is_empty()
        && !entry.sha256.trim().is_empty()
        && prov.sha256 != entry.sha256
    {
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

pub(crate) fn validate_toolchain_layout(kind: ToolchainKind, root: &Path) -> Result<(), String> {
    match kind {
        ToolchainKind::Sdk => validate_sdk_layout(root),
        ToolchainKind::Ndk => validate_ndk_layout(root),
        _ => Ok(()),
    }
}

async fn fetch_transparency_entry_value(entry_url: &str) -> Result<String, String> {
    if entry_url.trim().is_empty() {
        return Err("transparency log entry url is empty".into());
    }
    if is_remote_url(entry_url) {
        let client = Client::builder()
            .user_agent("aadk-toolchain")
            .build()
            .map_err(|e| format!("failed to build http client: {e}"))?;
        let resp = client
            .get(entry_url)
            .send()
            .await
            .map_err(|e| format!("transparency log entry download failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "transparency log entry download failed with status {}",
                resp.status()
            ));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| format!("transparency log entry read failed: {e}"))?;
        return Ok(body);
    }
    let path = local_artifact_path(entry_url)
        .ok_or_else(|| "transparency log entry url is not a local file".to_string())?;
    fs::read_to_string(&path).map_err(|e| {
        format!(
            "failed to read transparency log entry file {}: {e}",
            path.display()
        )
    })
}

async fn resolve_transparency_entry(record: &TransparencyLogRecord) -> Result<String, String> {
    if !record.entry.trim().is_empty() {
        return Ok(record.entry.trim().to_string());
    }
    fetch_transparency_entry_value(&record.entry_url).await
}

fn parse_transparency_entry(raw: &str) -> Result<TransparencyLogEntry, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("transparency log entry is empty".into());
    }
    if let Ok(parsed) = serde_json::from_str::<TransparencyLogEntryJson>(trimmed) {
        if parsed.sha256.trim().is_empty() {
            return Err("transparency log entry missing sha256".into());
        }
        return Ok(TransparencyLogEntry {
            sha256: normalize_digest_value(&parsed.sha256),
            signature: parsed.signature.trim().to_string(),
            log_id: parsed.log_id.trim().to_string(),
            log_index: parsed.log_index,
            timestamp_unix_millis: parsed.timestamp_unix_millis,
        });
    }
    Ok(TransparencyLogEntry {
        sha256: normalize_digest_value(trimmed),
        signature: String::new(),
        log_id: String::new(),
        log_index: None,
        timestamp_unix_millis: None,
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

fn verify_ed25519_signature(
    artifact_path: &Path,
    signature_value: &str,
    public_key_value: &str,
) -> Result<(), String> {
    let message =
        sha256_file_bytes(artifact_path).map_err(|e| format!("failed to hash artifact: {e}"))?;
    verify_ed25519_signature_bytes(&message, signature_value, public_key_value)
}

fn verify_ed25519_signature_bytes(
    message: &[u8],
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

    let key =
        VerifyingKey::from_bytes(&public_key).map_err(|e| format!("invalid public key: {e}"))?;
    let signature = Signature::from_bytes(&signature);

    key.verify_strict(message, &signature)
        .map_err(|e| format!("signature verification failed: {e}"))
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

fn normalize_digest_value(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(rest) = trimmed.strip_prefix("sha256:") {
        rest.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    let value = value.trim().trim_start_matches("0x");
    if !value.len().is_multiple_of(2) {
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

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
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

    let sdkmanager = root
        .join("cmdline-tools")
        .join("latest")
        .join("bin")
        .join("sdkmanager");
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
