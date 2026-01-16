use std::{
    fs, io,
    path::{Path, PathBuf},
};

use aadk_proto::aadk::v1::{
    Id, InstalledToolchain, Timestamp, ToolchainKind, ToolchainProvider, ToolchainSet,
    ToolchainVersion,
};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct State {
    pub(crate) active_set_id: Option<String>,
    pub(crate) toolchain_sets: Vec<ToolchainSet>,
    pub(crate) installed: Vec<InstalledToolchain>,
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
            toolchain_id: item
                .toolchain_id
                .as_ref()
                .map(|i| i.value.clone())
                .unwrap_or_default(),
            provider_id: provider.provider_id.map(|i| i.value).unwrap_or_default(),
            provider_name: provider.name,
            provider_kind: provider.kind,
            version: version.version,
            channel: version.channel,
            notes: version.notes,
            install_path: item.install_path.clone(),
            verified: item.verified,
            installed_at_unix_millis: item
                .installed_at
                .as_ref()
                .map(|t| t.unix_millis)
                .unwrap_or_default(),
            source_url: item.source_url.clone(),
            sha256: item.sha256.clone(),
        }
    }

    fn into_installed(self) -> InstalledToolchain {
        InstalledToolchain {
            toolchain_id: Some(Id {
                value: self.toolchain_id,
            }),
            provider: Some(ToolchainProvider {
                provider_id: Some(Id {
                    value: self.provider_id,
                }),
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
            installed_at: Some(Timestamp {
                unix_millis: self.installed_at_unix_millis,
            }),
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

pub(crate) fn data_dir() -> PathBuf {
    aadk_util::data_dir()
}

pub(crate) fn default_install_root() -> PathBuf {
    data_dir().join("toolchains")
}

pub(crate) fn install_root_for_entry(entry: &InstalledToolchain) -> PathBuf {
    let install_path = Path::new(&entry.install_path);
    if let Some(provider_dir) = install_path.parent() {
        if let Some(root) = provider_dir.parent() {
            return root.to_path_buf();
        }
    }
    default_install_root()
}

pub(crate) fn provider_id_from_installed(entry: &InstalledToolchain) -> Option<String> {
    entry
        .provider
        .as_ref()
        .and_then(|provider| provider.provider_id.as_ref().map(|id| id.value.clone()))
        .filter(|value| !value.trim().is_empty())
}

pub(crate) fn provider_name_from_installed(entry: &InstalledToolchain) -> Option<String> {
    entry
        .provider
        .as_ref()
        .map(|provider| provider.name.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn version_from_installed(entry: &InstalledToolchain) -> Option<String> {
    entry
        .version
        .as_ref()
        .map(|version| version.version.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn expand_user(path: &str) -> PathBuf {
    aadk_util::expand_user(path)
}

pub(crate) fn load_state() -> State {
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
            set.toolchain_set_id.as_ref().map(|id| id.value.as_str()) == Some(active_id.as_str())
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

pub(crate) fn save_state_best_effort(state: &State) {
    if let Err(e) = save_state(state) {
        warn!("Failed to persist toolchain state: {}", e);
    }
}

pub(crate) fn normalize_id(id: Option<Id>) -> Option<String> {
    id.map(|value| value.value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn installed_toolchain_matches(
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

pub(crate) fn toolchain_set_id(set: &ToolchainSet) -> Option<&str> {
    set.toolchain_set_id.as_ref().map(|id| id.value.as_str())
}

pub(crate) fn toolchain_referenced_by_sets(sets: &[ToolchainSet], toolchain_id: &str) -> bool {
    sets.iter().any(|set| {
        set.sdk_toolchain_id.as_ref().map(|id| id.value.as_str()) == Some(toolchain_id)
            || set.ndk_toolchain_id.as_ref().map(|id| id.value.as_str()) == Some(toolchain_id)
    })
}

pub(crate) fn scrub_toolchain_from_sets(
    sets: &mut Vec<ToolchainSet>,
    toolchain_id: &str,
) -> Vec<String> {
    let mut removed_sets = Vec::new();
    for set in sets.iter_mut() {
        if set.sdk_toolchain_id.as_ref().map(|id| id.value.as_str()) == Some(toolchain_id) {
            set.sdk_toolchain_id = None;
        }
        if set.ndk_toolchain_id.as_ref().map(|id| id.value.as_str()) == Some(toolchain_id) {
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

pub(crate) fn replace_toolchain_in_sets(
    sets: &mut Vec<ToolchainSet>,
    old_id: &str,
    new_id: &str,
) -> usize {
    let mut updated = 0usize;
    for set in sets {
        if set.sdk_toolchain_id.as_ref().map(|id| id.value.as_str()) == Some(old_id) {
            set.sdk_toolchain_id = Some(Id {
                value: new_id.to_string(),
            });
            updated += 1;
        }
        if set.ndk_toolchain_id.as_ref().map(|id| id.value.as_str()) == Some(old_id) {
            set.ndk_toolchain_id = Some(Id {
                value: new_id.to_string(),
            });
            updated += 1;
        }
    }
    updated
}

fn state_file_path() -> PathBuf {
    aadk_util::state_file_path("toolchains.json")
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
    let payload = serde_json::to_vec_pretty(&persist).map_err(io::Error::other)?;
    let path = state_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    fs::write(&tmp, payload)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}
