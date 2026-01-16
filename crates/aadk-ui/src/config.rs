use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::models::ActiveContext;

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
    pub(crate) active_project_id: String,
    pub(crate) active_project_path: String,
    pub(crate) active_target_id: String,
    pub(crate) active_toolchain_set_id: String,
    pub(crate) active_run_id: String,
    pub(crate) telemetry_usage_enabled: bool,
    pub(crate) telemetry_crash_enabled: bool,
    pub(crate) telemetry_install_id: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            job_addr: std::env::var("AADK_JOB_ADDR")
                .unwrap_or_else(|_| aadk_util::DEFAULT_JOB_ADDR.into()),
            toolchain_addr: std::env::var("AADK_TOOLCHAIN_ADDR")
                .unwrap_or_else(|_| aadk_util::DEFAULT_TOOLCHAIN_ADDR.into()),
            project_addr: std::env::var("AADK_PROJECT_ADDR")
                .unwrap_or_else(|_| aadk_util::DEFAULT_PROJECT_ADDR.into()),
            build_addr: std::env::var("AADK_BUILD_ADDR")
                .unwrap_or_else(|_| aadk_util::DEFAULT_BUILD_ADDR.into()),
            targets_addr: std::env::var("AADK_TARGETS_ADDR")
                .unwrap_or_else(|_| aadk_util::DEFAULT_TARGETS_ADDR.into()),
            observe_addr: std::env::var("AADK_OBSERVE_ADDR")
                .unwrap_or_else(|_| aadk_util::DEFAULT_OBSERVE_ADDR.into()),
            workflow_addr: std::env::var("AADK_WORKFLOW_ADDR")
                .unwrap_or_else(|_| aadk_util::DEFAULT_WORKFLOW_ADDR.into()),
            last_job_type: String::new(),
            last_job_params: String::new(),
            last_job_project_id: String::new(),
            last_job_target_id: String::new(),
            last_job_toolchain_set_id: String::new(),
            last_job_id: String::new(),
            last_correlation_id: String::new(),
            active_project_id: String::new(),
            active_project_path: String::new(),
            active_target_id: String::new(),
            active_toolchain_set_id: String::new(),
            active_run_id: String::new(),
            telemetry_usage_enabled: false,
            telemetry_crash_enabled: false,
            telemetry_install_id: String::new(),
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
                    cfg.active_project_id = file_cfg.active_project_id;
                    cfg.active_project_path = file_cfg.active_project_path;
                    cfg.active_target_id = file_cfg.active_target_id;
                    cfg.active_toolchain_set_id = file_cfg.active_toolchain_set_id;
                    cfg.active_run_id = file_cfg.active_run_id;
                    cfg.telemetry_usage_enabled = file_cfg.telemetry_usage_enabled;
                    cfg.telemetry_crash_enabled = file_cfg.telemetry_crash_enabled;
                    cfg.telemetry_install_id = file_cfg.telemetry_install_id;
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

    pub(crate) fn clear_cached_state(&mut self) {
        self.last_job_type.clear();
        self.last_job_params.clear();
        self.last_job_project_id.clear();
        self.last_job_target_id.clear();
        self.last_job_toolchain_set_id.clear();
        self.last_job_id.clear();
        self.last_correlation_id.clear();
        self.active_project_id.clear();
        self.active_project_path.clear();
        self.active_target_id.clear();
        self.active_toolchain_set_id.clear();
        self.active_run_id.clear();
        self.telemetry_install_id.clear();
    }

    pub(crate) fn active_context(&self) -> ActiveContext {
        ActiveContext {
            run_id: self.active_run_id.clone(),
            project_id: self.active_project_id.clone(),
            project_path: self.active_project_path.clone(),
            toolchain_set_id: self.active_toolchain_set_id.clone(),
            target_id: self.active_target_id.clone(),
        }
    }
}

pub(crate) fn data_dir() -> PathBuf {
    aadk_util::data_dir()
}

fn ui_config_path() -> PathBuf {
    data_dir().join("state").join(UI_CONFIG_FILE)
}

pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    aadk_util::write_json_atomic(path, value)
}
