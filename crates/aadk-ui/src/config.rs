use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const UI_CONFIG_FILE: &str = "ui-config.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AppConfig {
    pub(crate) job_addr: String,
    pub(crate) toolchain_addr: String,
    pub(crate) project_addr: String,
    pub(crate) build_addr: String,
    pub(crate) targets_addr: String,
    pub(crate) observe_addr: String,
    pub(crate) workflow_addr: String,
    pub(crate) last_job_type: String,
    pub(crate) last_job_params: String,
    pub(crate) last_job_project_id: String,
    pub(crate) last_job_target_id: String,
    pub(crate) last_job_toolchain_set_id: String,
    pub(crate) last_job_id: String,
    pub(crate) last_correlation_id: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            job_addr: std::env::var("AADK_JOB_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".into()),
            toolchain_addr: std::env::var("AADK_TOOLCHAIN_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:50052".into()),
            project_addr: std::env::var("AADK_PROJECT_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:50053".into()),
            build_addr: std::env::var("AADK_BUILD_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:50054".into()),
            targets_addr: std::env::var("AADK_TARGETS_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:50055".into()),
            observe_addr: std::env::var("AADK_OBSERVE_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:50056".into()),
            workflow_addr: std::env::var("AADK_WORKFLOW_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:50057".into()),
            last_job_type: String::new(),
            last_job_params: String::new(),
            last_job_project_id: String::new(),
            last_job_target_id: String::new(),
            last_job_toolchain_set_id: String::new(),
            last_job_id: String::new(),
            last_correlation_id: String::new(),
        }
    }
}

impl AppConfig {
    pub(crate) fn load() -> Self {
        let mut cfg = AppConfig::default();
        let path = ui_config_path();
        match fs::read_to_string(&path) {
            Ok(data) => match serde_json::from_str::<AppConfig>(&data) {
                Ok(file_cfg) => {
                    if std::env::var("AADK_JOB_ADDR").is_err() && !file_cfg.job_addr.is_empty() {
                        cfg.job_addr = file_cfg.job_addr;
                    }
                    if std::env::var("AADK_TOOLCHAIN_ADDR").is_err()
                        && !file_cfg.toolchain_addr.is_empty()
                    {
                        cfg.toolchain_addr = file_cfg.toolchain_addr;
                    }
                    if std::env::var("AADK_PROJECT_ADDR").is_err()
                        && !file_cfg.project_addr.is_empty()
                    {
                        cfg.project_addr = file_cfg.project_addr;
                    }
                    if std::env::var("AADK_BUILD_ADDR").is_err() && !file_cfg.build_addr.is_empty()
                    {
                        cfg.build_addr = file_cfg.build_addr;
                    }
                    if std::env::var("AADK_TARGETS_ADDR").is_err()
                        && !file_cfg.targets_addr.is_empty()
                    {
                        cfg.targets_addr = file_cfg.targets_addr;
                    }
                    if std::env::var("AADK_OBSERVE_ADDR").is_err()
                        && !file_cfg.observe_addr.is_empty()
                    {
                        cfg.observe_addr = file_cfg.observe_addr;
                    }
                    if std::env::var("AADK_WORKFLOW_ADDR").is_err()
                        && !file_cfg.workflow_addr.is_empty()
                    {
                        cfg.workflow_addr = file_cfg.workflow_addr;
                    }
                    cfg.last_job_type = file_cfg.last_job_type;
                    cfg.last_job_params = file_cfg.last_job_params;
                    cfg.last_job_project_id = file_cfg.last_job_project_id;
                    cfg.last_job_target_id = file_cfg.last_job_target_id;
                    cfg.last_job_toolchain_set_id = file_cfg.last_job_toolchain_set_id;
                    cfg.last_job_id = file_cfg.last_job_id;
                    cfg.last_correlation_id = file_cfg.last_correlation_id;
                }
                Err(err) => {
                    eprintln!("Failed to parse {}: {err}", path.display());
                }
            },
            Err(err) => {
                if err.kind() != io::ErrorKind::NotFound {
                    eprintln!("Failed to read {}: {err}", path.display());
                }
            }
        }
        cfg
    }

    pub(crate) fn save(&self) -> io::Result<()> {
        write_json_atomic(&ui_config_path(), self)
    }
}

pub(crate) fn data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/aadk")
    } else {
        PathBuf::from("/tmp/aadk")
    }
}

fn ui_config_path() -> PathBuf {
    data_dir().join("state").join(UI_CONFIG_FILE)
}

pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}
