use std::{
    fs,
    path::{Path, PathBuf},
};

use aadk_proto::aadk::v1::{
    AvailableToolchain, Id, ToolchainArtifact, ToolchainKind, ToolchainProvider, ToolchainVersion,
};
use serde::Deserialize;
use tracing::warn;

use crate::hashing::sha256_file;

const FIXTURES_ENV_DIR: &str = "AADK_TOOLCHAIN_FIXTURES_DIR";
const CATALOG_ENV: &str = "AADK_TOOLCHAIN_CATALOG";
const HOST_OVERRIDE_ENV: &str = "AADK_TOOLCHAIN_HOST";
const HOST_FALLBACK_ENV: &str = "AADK_TOOLCHAIN_HOST_FALLBACK";

#[derive(Clone, Deserialize)]
#[serde(default)]
pub(crate) struct Catalog {
    pub(crate) schema_version: u32,
    pub(crate) providers: Vec<CatalogProvider>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct CatalogProvider {
    pub(crate) provider_id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) description: String,
    pub(crate) versions: Vec<CatalogVersion>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct CatalogVersion {
    pub(crate) version: String,
    pub(crate) channel: String,
    pub(crate) notes: String,
    pub(crate) artifacts: Vec<CatalogArtifact>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct CatalogArtifact {
    pub(crate) host: String,
    pub(crate) url: String,
    pub(crate) sha256: String,
    pub(crate) size_bytes: u64,
    pub(crate) signature: String,
    pub(crate) signature_url: String,
    pub(crate) signature_public_key: String,
    pub(crate) transparency_log_entry: String,
    pub(crate) transparency_log_entry_url: String,
    pub(crate) transparency_log_public_key: String,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            schema_version: 1,
            providers: Vec::new(),
        }
    }
}

pub(crate) fn fixtures_dir() -> Option<PathBuf> {
    std::env::var(FIXTURES_ENV_DIR).ok().map(PathBuf::from)
}

pub(crate) fn load_catalog() -> Catalog {
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

pub(crate) fn host_key() -> Option<String> {
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

pub(crate) fn available_for_provider(
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

pub(crate) fn find_available(
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

pub(crate) fn catalog_version_hosts(
    catalog: &Catalog,
    provider_id: &str,
    version: &str,
) -> Option<Vec<String>> {
    let provider = catalog
        .providers
        .iter()
        .find(|item| item.provider_id == provider_id)?;
    let entry = provider
        .versions
        .iter()
        .find(|item| item.version == version)?;
    let mut hosts = entry
        .artifacts
        .iter()
        .filter(|item| !item.url.trim().is_empty())
        .map(|item| item.host.clone())
        .collect::<Vec<_>>();
    hosts.sort();
    hosts.dedup();
    Some(hosts)
}

pub(crate) fn provider_from_catalog(provider: &CatalogProvider) -> ToolchainProvider {
    ToolchainProvider {
        provider_id: Some(Id {
            value: provider.provider_id.clone(),
        }),
        name: provider.name.clone(),
        kind: parse_kind(&provider.kind) as i32,
        description: provider.description.clone(),
    }
}

pub(crate) fn catalog_artifact_for_provenance(
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
    let version_entry = provider
        .versions
        .iter()
        .find(|item| item.version == version)?;
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

fn host_aliases(host: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    match host {
        "linux-aarch64" => {
            aliases.push("aarch64-linux-musl".into());
            aliases.push("aarch64-linux-android".into());
            aliases.push("aarch64_be-linux-musl".into());
        }
        "linux-aarch64-android" => aliases.push("aarch64-linux-android".into()),
        "linux-aarch64-be" => aliases.push("aarch64_be-linux-musl".into()),
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
