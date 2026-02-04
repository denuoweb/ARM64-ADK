use std::{fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::data_dir;

const UI_STATE_FILE: &str = "ui-state.json";
const LOG_MAX_CHARS: usize = 200_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct UiState {
    pub(crate) home: HomeState,
    pub(crate) workflow: WorkflowState,
    pub(crate) toolchains: ToolchainsState,
    pub(crate) projects: ProjectsState,
    pub(crate) targets: TargetsState,
    pub(crate) build: BuildState,
    pub(crate) jobs: JobsHistoryState,
    pub(crate) evidence: EvidenceState,
    pub(crate) settings: SettingsState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct HomeState {
    pub(crate) log: String,
    pub(crate) job_type: String,
    pub(crate) job_params: String,
    pub(crate) project_id: String,
    pub(crate) target_id: String,
    pub(crate) toolchain_set_id: String,
    pub(crate) correlation_id: String,
    pub(crate) watch_job_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WorkflowState {
    pub(crate) log: String,
    pub(crate) run_id: String,
    pub(crate) correlation_id: String,
    pub(crate) use_job_id: bool,
    pub(crate) job_id: String,
    pub(crate) include_history: bool,
    pub(crate) template_id: String,
    pub(crate) project_path: String,
    pub(crate) project_name: String,
    pub(crate) project_id: String,
    pub(crate) toolchain_id: String,
    pub(crate) toolchain_set_id: String,
    pub(crate) target_id: String,
    pub(crate) build_variant_index: u32,
    pub(crate) variant_name: String,
    pub(crate) module: String,
    pub(crate) tasks: String,
    pub(crate) apk_path: String,
    pub(crate) application_id: String,
    pub(crate) activity: String,
    pub(crate) auto_infer_steps: bool,
    pub(crate) step_create: bool,
    pub(crate) step_open: bool,
    pub(crate) step_verify: bool,
    pub(crate) step_build: bool,
    pub(crate) step_install: bool,
    pub(crate) step_launch: bool,
    pub(crate) step_support: bool,
    pub(crate) step_evidence: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ToolchainsState {
    pub(crate) log: String,
    pub(crate) use_job_id: bool,
    pub(crate) job_id: String,
    pub(crate) correlation_id: String,
    pub(crate) sdk_version: String,
    pub(crate) ndk_version: String,
    pub(crate) toolchain_id: String,
    pub(crate) update_version: String,
    pub(crate) verify_update: bool,
    pub(crate) remove_cached: bool,
    pub(crate) force_uninstall: bool,
    pub(crate) dry_run: bool,
    pub(crate) remove_all: bool,
    pub(crate) sdk_set_id: String,
    pub(crate) ndk_set_id: String,
    pub(crate) display_name: String,
    pub(crate) active_set_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ProjectsState {
    pub(crate) log: String,
    pub(crate) use_job_id: bool,
    pub(crate) job_id: String,
    pub(crate) correlation_id: String,
    pub(crate) template_id: String,
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) project_id: String,
    pub(crate) toolchain_set_id: String,
    pub(crate) default_target_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct TargetsState {
    pub(crate) log: String,
    pub(crate) use_job_id: bool,
    pub(crate) job_id: String,
    pub(crate) correlation_id: String,
    pub(crate) cuttlefish_branch: String,
    pub(crate) cuttlefish_target: String,
    pub(crate) cuttlefish_build_id: String,
    pub(crate) target_id: String,
    pub(crate) apk_path: String,
    pub(crate) application_id: String,
    pub(crate) activity: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct BuildState {
    pub(crate) log: String,
    pub(crate) project_ref: String,
    pub(crate) module: String,
    pub(crate) variant_index: u32,
    pub(crate) variant_name: String,
    pub(crate) tasks: String,
    pub(crate) gradle_args: String,
    pub(crate) clean_first: bool,
    pub(crate) use_job_id: bool,
    pub(crate) job_id: String,
    pub(crate) correlation_id: String,
    pub(crate) artifact_modules: String,
    pub(crate) artifact_variant: String,
    pub(crate) artifact_types: String,
    pub(crate) artifact_name: String,
    pub(crate) artifact_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct JobsHistoryState {
    pub(crate) log: String,
    pub(crate) job_types: String,
    pub(crate) states: String,
    pub(crate) created_after: String,
    pub(crate) created_before: String,
    pub(crate) finished_after: String,
    pub(crate) finished_before: String,
    pub(crate) correlation_id: String,
    pub(crate) page_size: String,
    pub(crate) page_token: String,
    pub(crate) job_id: String,
    pub(crate) kinds: String,
    pub(crate) after: String,
    pub(crate) before: String,
    pub(crate) history_page_size: String,
    pub(crate) history_page_token: String,
    pub(crate) output_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct EvidenceState {
    pub(crate) log: String,
    pub(crate) use_job_id: bool,
    pub(crate) job_id: String,
    pub(crate) correlation_id: String,
    pub(crate) job_log_output_path: String,
    pub(crate) run_id: String,
    pub(crate) output_kind_index: u32,
    pub(crate) output_type: String,
    pub(crate) output_path: String,
    pub(crate) output_label: String,
    pub(crate) recent_limit: String,
    pub(crate) include_history: bool,
    pub(crate) include_logs: bool,
    pub(crate) include_config: bool,
    pub(crate) include_toolchain: bool,
    pub(crate) include_recent: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SettingsState {
    pub(crate) log: String,
    pub(crate) exclude_downloads: bool,
    pub(crate) exclude_toolchains: bool,
    pub(crate) exclude_bundles: bool,
    pub(crate) exclude_telemetry: bool,
    pub(crate) save_path: String,
    pub(crate) open_path: String,
}

impl Default for HomeState {
    fn default() -> Self {
        Self {
            log: String::new(),
            job_type: "workflow.pipeline".into(),
            job_params: String::new(),
            project_id: String::new(),
            target_id: String::new(),
            toolchain_set_id: String::new(),
            correlation_id: String::new(),
            watch_job_id: String::new(),
        }
    }
}

impl Default for WorkflowState {
    fn default() -> Self {
        Self {
            log: String::new(),
            run_id: String::new(),
            correlation_id: String::new(),
            use_job_id: false,
            job_id: String::new(),
            include_history: true,
            template_id: String::new(),
            project_path: String::new(),
            project_name: String::new(),
            project_id: String::new(),
            toolchain_id: String::new(),
            toolchain_set_id: String::new(),
            target_id: String::new(),
            build_variant_index: 0,
            variant_name: String::new(),
            module: String::new(),
            tasks: String::new(),
            apk_path: String::new(),
            application_id: String::new(),
            activity: String::new(),
            auto_infer_steps: true,
            step_create: false,
            step_open: false,
            step_verify: false,
            step_build: false,
            step_install: false,
            step_launch: false,
            step_support: false,
            step_evidence: false,
        }
    }
}

impl Default for ToolchainsState {
    fn default() -> Self {
        Self {
            log: String::new(),
            use_job_id: false,
            job_id: String::new(),
            correlation_id: String::new(),
            sdk_version: "36.0.0".into(),
            ndk_version: "r29".into(),
            toolchain_id: String::new(),
            update_version: String::new(),
            verify_update: true,
            remove_cached: false,
            force_uninstall: false,
            dry_run: true,
            remove_all: false,
            sdk_set_id: String::new(),
            ndk_set_id: String::new(),
            display_name: String::new(),
            active_set_id: String::new(),
        }
    }
}

impl Default for ProjectsState {
    fn default() -> Self {
        Self {
            log: String::new(),
            use_job_id: false,
            job_id: String::new(),
            correlation_id: String::new(),
            template_id: String::new(),
            name: String::new(),
            path: String::new(),
            project_id: String::new(),
            toolchain_set_id: "none".into(),
            default_target_id: "none".into(),
        }
    }
}

impl Default for TargetsState {
    fn default() -> Self {
        let target_id =
            std::env::var("AADK_CUTTLEFISH_ADB_SERIAL").unwrap_or_else(|_| "127.0.0.1:6520".into());
        let cuttlefish_branch = std::env::var("AADK_CUTTLEFISH_BRANCH").unwrap_or_default();
        let cuttlefish_target = std::env::var("AADK_CUTTLEFISH_TARGET").unwrap_or_default();
        let cuttlefish_build_id = std::env::var("AADK_CUTTLEFISH_BUILD_ID").unwrap_or_default();
        Self {
            log: String::new(),
            use_job_id: false,
            job_id: String::new(),
            correlation_id: String::new(),
            cuttlefish_branch,
            cuttlefish_target,
            cuttlefish_build_id,
            target_id,
            apk_path: String::new(),
            application_id: String::new(),
            activity: ".MainActivity".into(),
        }
    }
}

impl Default for JobsHistoryState {
    fn default() -> Self {
        Self {
            log: String::new(),
            job_types: String::new(),
            states: String::new(),
            created_after: String::new(),
            created_before: String::new(),
            finished_after: String::new(),
            finished_before: String::new(),
            correlation_id: String::new(),
            page_size: "50".into(),
            page_token: String::new(),
            job_id: String::new(),
            kinds: String::new(),
            after: String::new(),
            before: String::new(),
            history_page_size: "200".into(),
            history_page_token: String::new(),
            output_path: String::new(),
        }
    }
}

impl Default for EvidenceState {
    fn default() -> Self {
        Self {
            log: String::new(),
            use_job_id: false,
            job_id: String::new(),
            correlation_id: String::new(),
            job_log_output_path: String::new(),
            run_id: String::new(),
            output_kind_index: 0,
            output_type: String::new(),
            output_path: String::new(),
            output_label: String::new(),
            recent_limit: "10".into(),
            include_history: true,
            include_logs: true,
            include_config: true,
            include_toolchain: true,
            include_recent: true,
        }
    }
}

impl UiState {
    pub(crate) fn load_with_status() -> (Self, bool) {
        let path = ui_state_path();
        match fs::read_to_string(&path) {
            Ok(data) => match serde_json::from_str::<UiState>(&data) {
                Ok(state) => (state, true),
                Err(err) => {
                    eprintln!("Failed to parse {}: {err}", path.display());
                    (UiState::default(), false)
                }
            },
            Err(err) => {
                if err.kind() != io::ErrorKind::NotFound {
                    eprintln!("Failed to read {}: {err}", path.display());
                }
                (UiState::default(), false)
            }
        }
    }

    pub(crate) fn save(&self) -> io::Result<()> {
        let mut trimmed = self.clone();
        trimmed.home.log = trim_log(&trimmed.home.log);
        trimmed.workflow.log = trim_log(&trimmed.workflow.log);
        trimmed.toolchains.log = trim_log(&trimmed.toolchains.log);
        trimmed.projects.log = trim_log(&trimmed.projects.log);
        trimmed.targets.log = trim_log(&trimmed.targets.log);
        trimmed.build.log = trim_log(&trimmed.build.log);
        trimmed.jobs.log = trim_log(&trimmed.jobs.log);
        trimmed.evidence.log = trim_log(&trimmed.evidence.log);
        trimmed.settings.log = trim_log(&trimmed.settings.log);
        aadk_util::write_json_atomic(&ui_state_path(), &trimmed)
    }

    pub(crate) fn append_log(&mut self, page: &str, line: &str) {
        let target = match page {
            "home" => &mut self.home.log,
            "workflow" => &mut self.workflow.log,
            "toolchains" => &mut self.toolchains.log,
            "projects" => &mut self.projects.log,
            "targets" => &mut self.targets.log,
            "console" => &mut self.build.log,
            "jobs" => &mut self.jobs.log,
            "evidence" => &mut self.evidence.log,
            "settings" => &mut self.settings.log,
            _ => return,
        };
        target.push_str(line);
        if target.len() > LOG_MAX_CHARS {
            *target = trim_log(target);
        }
    }

    pub(crate) fn clear_file() -> io::Result<()> {
        let path = ui_state_path();
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }
}

fn ui_state_path() -> PathBuf {
    data_dir().join("state").join(UI_STATE_FILE)
}

fn trim_log(text: &str) -> String {
    let char_count = text.chars().count();
    if char_count <= LOG_MAX_CHARS {
        return text.to_string();
    }
    text.chars().skip(char_count - LOG_MAX_CHARS).collect()
}
