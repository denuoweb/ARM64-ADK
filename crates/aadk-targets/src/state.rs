use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

use aadk_proto::aadk::v1::{Id, KeyValue, Target};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::ids::{normalize_target_address, normalize_target_id, normalize_target_id_for_compare};

const STATE_FILE_NAME: &str = "targets.json";

#[derive(Default)]
pub(crate) struct State {
    pub(crate) default_target: Option<Target>,
    pub(crate) inventory: Vec<TargetInventoryEntry>,
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
pub(crate) struct TargetInventoryEntry {
    target: PersistedTarget,
    last_seen_unix_millis: i64,
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
            target_id: Some(Id {
                value: self.target_id,
            }),
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
    pub(crate) fn target_id(&self) -> &str {
        &self.target.target_id
    }

    pub(crate) fn from_target(target: &Target, last_seen_unix_millis: i64) -> Option<Self> {
        let target = PersistedTarget::from_proto(target)?;
        Some(Self {
            target,
            last_seen_unix_millis,
        })
    }

    pub(crate) fn to_target(&self, state_override: Option<&str>) -> Target {
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

pub(crate) fn data_dir() -> PathBuf {
    aadk_util::data_dir()
}

pub(crate) fn load_state() -> State {
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
                    if !inventory.iter().any(|entry| {
                        normalize_target_id_for_compare(&entry.target.target_id) == key
                    }) {
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

pub(crate) fn save_state(state: &State) -> io::Result<()> {
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

pub(crate) fn save_state_best_effort(state: &State) {
    if let Err(err) = save_state(state) {
        warn!("Failed to persist target state: {}", err);
    }
}

pub(crate) fn upsert_inventory_entries(
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

pub(crate) fn merge_inventory_targets(
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

fn state_file_path() -> PathBuf {
    aadk_util::state_file_path(STATE_FILE_NAME)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    aadk_util::write_json_atomic(path, value)
}
