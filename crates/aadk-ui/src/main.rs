use std::{
    collections::BTreeMap,
    fs,
    io,
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    thread,
    time::{Duration, SystemTime},
};

use aadk_proto::aadk::v1::{
    build_service_client::BuildServiceClient,
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    observe_service_client::ObserveServiceClient,
    project_service_client::ProjectServiceClient,
    target_service_client::TargetServiceClient,
    toolchain_service_client::ToolchainServiceClient,
    Artifact, ArtifactFilter, ArtifactType, BuildRequest, BuildVariant, CancelJobRequest,
    CleanupToolchainCacheRequest, CreateProjectRequest, CreateToolchainSetRequest,
    ExportEvidenceBundleRequest, ExportSupportBundleRequest, GetActiveToolchainSetRequest,
    GetCuttlefishStatusRequest, GetDefaultTargetRequest, GetJobRequest, Id, InstallApkRequest,
    InstallCuttlefishRequest, InstallToolchainRequest, Job, JobEvent, JobEventKind, JobFilter,
    JobHistoryFilter, JobState, KeyValue, LaunchRequest, ListJobHistoryRequest, ListJobsRequest,
    ListArtifactsRequest, ListAvailableRequest, ListInstalledRequest, ListProvidersRequest,
    ListRecentProjectsRequest, ListRunsRequest, ListTargetsRequest, ListTemplatesRequest,
    ListToolchainSetsRequest, OpenProjectRequest, Pagination, ResolveCuttlefishBuildRequest,
    SetActiveToolchainSetRequest, SetDefaultTargetRequest, SetProjectConfigRequest,
    StartCuttlefishRequest, StartJobRequest, StopCuttlefishRequest, StreamJobEventsRequest,
    StreamLogcatRequest, Timestamp, ToolchainKind, UninstallToolchainRequest, UpdateToolchainRequest,
    VerifyToolchainRequest, RunId,
};
use futures_util::StreamExt;
use gtk4 as gtk;
use gtk::gio::prelude::FileExt;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};
use tonic::transport::Channel;

const UI_CONFIG_FILE: &str = "ui-config.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct AppConfig {
    job_addr: String,
    toolchain_addr: String,
    project_addr: String,
    build_addr: String,
    targets_addr: String,
    observe_addr: String,
    workflow_addr: String,
    last_job_type: String,
    last_job_params: String,
    last_job_project_id: String,
    last_job_target_id: String,
    last_job_toolchain_set_id: String,
    last_job_id: String,
    last_correlation_id: String,
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
    fn load() -> Self {
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
                    if std::env::var("AADK_PROJECT_ADDR").is_err() && !file_cfg.project_addr.is_empty() {
                        cfg.project_addr = file_cfg.project_addr;
                    }
                    if std::env::var("AADK_BUILD_ADDR").is_err() && !file_cfg.build_addr.is_empty() {
                        cfg.build_addr = file_cfg.build_addr;
                    }
                    if std::env::var("AADK_TARGETS_ADDR").is_err() && !file_cfg.targets_addr.is_empty() {
                        cfg.targets_addr = file_cfg.targets_addr;
                    }
                    if std::env::var("AADK_OBSERVE_ADDR").is_err() && !file_cfg.observe_addr.is_empty() {
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

    fn save(&self) -> io::Result<()> {
        write_json_atomic(&ui_config_path(), self)
    }
}

fn data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/aadk")
    } else {
        PathBuf::from("/tmp/aadk")
    }
}

fn ui_config_path() -> PathBuf {
    data_dir().join("state").join(UI_CONFIG_FILE)
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

#[derive(Clone, Default)]
struct AppState {
    current_job_id: Option<String>,
}

#[derive(Serialize)]
struct UiLogExport {
    exported_at_unix_millis: i64,
    job_id: String,
    config: AppConfig,
    job: Option<JobSummary>,
    events: Vec<LogExportEvent>,
}

#[derive(Serialize)]
struct JobSummary {
    job_id: String,
    job_type: String,
    state: String,
    created_at_unix_millis: i64,
    started_at_unix_millis: Option<i64>,
    finished_at_unix_millis: Option<i64>,
    display_name: String,
    correlation_id: String,
    run_id: String,
    project_id: String,
    target_id: String,
    toolchain_set_id: String,
}

#[derive(Serialize)]
struct LogExportEvent {
    at_unix_millis: i64,
    kind: String,
    summary: String,
    data: Option<String>,
}

#[derive(Debug)]
enum UiCommand {
    HomeStartJob {
        cfg: AppConfig,
        job_type: String,
        params_raw: String,
        project_id: String,
        target_id: String,
        toolchain_set_id: String,
        correlation_id: String,
    },
    HomeWatchJob { cfg: AppConfig, job_id: String },
    HomeCancelCurrent { cfg: AppConfig },
    JobsList {
        cfg: AppConfig,
        job_types: String,
        states: String,
        created_after: String,
        created_before: String,
        finished_after: String,
        finished_before: String,
        correlation_id: String,
        page_size: u32,
        page_token: String,
    },
    JobsHistory {
        cfg: AppConfig,
        job_id: String,
        kinds: String,
        after: String,
        before: String,
        page_size: u32,
        page_token: String,
    },
    JobsExportLogs {
        cfg: AppConfig,
        job_id: String,
        output_path: String,
    },
    ToolchainListProviders { cfg: AppConfig },
    ToolchainListAvailable { cfg: AppConfig, provider_id: String },
    ToolchainInstall {
        cfg: AppConfig,
        provider_id: String,
        version: String,
        verify: bool,
        job_id: Option<String>,
        correlation_id: String,
    },
    ToolchainListInstalled { cfg: AppConfig, kind: ToolchainKind },
    ToolchainListSets { cfg: AppConfig },
    ToolchainVerifyInstalled {
        cfg: AppConfig,
        job_id: Option<String>,
        correlation_id: String,
    },
    ToolchainUpdate {
        cfg: AppConfig,
        toolchain_id: String,
        version: String,
        verify: bool,
        remove_cached: bool,
        job_id: Option<String>,
        correlation_id: String,
    },
    ToolchainUninstall {
        cfg: AppConfig,
        toolchain_id: String,
        remove_cached: bool,
        force: bool,
        job_id: Option<String>,
        correlation_id: String,
    },
    ToolchainCleanupCache {
        cfg: AppConfig,
        dry_run: bool,
        remove_all: bool,
        job_id: Option<String>,
        correlation_id: String,
    },
    ToolchainCreateSet {
        cfg: AppConfig,
        sdk_toolchain_id: Option<String>,
        ndk_toolchain_id: Option<String>,
        display_name: String,
    },
    ToolchainSetActive { cfg: AppConfig, toolchain_set_id: String },
    ToolchainGetActive { cfg: AppConfig },
    ProjectListTemplates { cfg: AppConfig },
    ProjectListRecent { cfg: AppConfig },
    ProjectLoadDefaults { cfg: AppConfig },
    ProjectCreate {
        cfg: AppConfig,
        name: String,
        path: String,
        template_id: String,
        job_id: Option<String>,
        correlation_id: String,
    },
    ProjectOpen { cfg: AppConfig, path: String },
    ProjectSetConfig {
        cfg: AppConfig,
        project_id: String,
        toolchain_set_id: Option<String>,
        default_target_id: Option<String>,
    },
    ProjectUseActiveDefaults { cfg: AppConfig, project_id: String },
    TargetsList { cfg: AppConfig },
    TargetsSetDefault { cfg: AppConfig, target_id: String },
    TargetsGetDefault { cfg: AppConfig },
    TargetsInstallCuttlefish {
        cfg: AppConfig,
        force: bool,
        branch: String,
        target: String,
        build_id: String,
        job_id: Option<String>,
        correlation_id: String,
    },
    TargetsResolveCuttlefishBuild {
        cfg: AppConfig,
        branch: String,
        target: String,
        build_id: String,
    },
    TargetsStartCuttlefish {
        cfg: AppConfig,
        show_full_ui: bool,
        job_id: Option<String>,
        correlation_id: String,
    },
    TargetsStopCuttlefish {
        cfg: AppConfig,
        job_id: Option<String>,
        correlation_id: String,
    },
    TargetsCuttlefishStatus { cfg: AppConfig },
    TargetsInstallApk {
        cfg: AppConfig,
        target_id: String,
        apk_path: String,
        job_id: Option<String>,
        correlation_id: String,
    },
    TargetsLaunchApp {
        cfg: AppConfig,
        target_id: String,
        application_id: String,
        activity: String,
        job_id: Option<String>,
        correlation_id: String,
    },
    TargetsStreamLogcat { cfg: AppConfig, target_id: String, filter: String },
    ObserveListRuns { cfg: AppConfig },
    ObserveExportSupport {
        cfg: AppConfig,
        include_logs: bool,
        include_config: bool,
        include_toolchain_provenance: bool,
        include_recent_runs: bool,
        recent_runs_limit: u32,
        job_id: Option<String>,
        correlation_id: String,
    },
    ObserveExportEvidence {
        cfg: AppConfig,
        run_id: String,
        job_id: Option<String>,
        correlation_id: String,
    },
    BuildRun {
        cfg: AppConfig,
        project_ref: String,
        variant: BuildVariant,
        variant_name: String,
        module: String,
        tasks: Vec<String>,
        clean_first: bool,
        gradle_args: Vec<KeyValue>,
        job_id: Option<String>,
        correlation_id: String,
    },
    BuildListArtifacts {
        cfg: AppConfig,
        project_ref: String,
        variant: BuildVariant,
        filter: ArtifactFilter,
    },
}

#[derive(Debug, Clone)]
enum AppEvent {
    Log { page: &'static str, line: String },
    SetCurrentJob { job_id: Option<String> },
    HomeResetStatus,
    HomeState { state: String },
    HomeProgress { progress: String },
    HomeResult { result: String },
    SetLastBuildApk { apk_path: String },
    SetCuttlefishBuildId { build_id: String },
    ToolchainAvailable { provider_id: String, versions: Vec<String> },
    ProjectTemplates { templates: Vec<ProjectTemplateOption> },
    ProjectToolchainSets { sets: Vec<ToolchainSetOption> },
    ProjectTargets { targets: Vec<TargetOption> },
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("dev.aadk.ui.full_scaffold")
        .build();

    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &gtk::Application) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("AADK UI — Full Scaffold")
        .default_width(1100)
        .default_height(700)
        .build();

    let cfg = Arc::new(std::sync::Mutex::new(AppConfig::load()));
    let state = Arc::new(std::sync::Mutex::new(AppState::default()));

    let (cmd_tx, cmd_rx) = mpsc::channel::<UiCommand>();
    let (ev_tx, ev_rx) = mpsc::channel::<AppEvent>();

    // Background thread with tokio runtime; holds a private copy of AppState for worker actions.
    // State mutations are pushed to GTK via AppEvent.
    thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");

        rt.block_on(async move {
            let mut worker_state = AppState::default();

            while let Ok(cmd) = cmd_rx.recv() {
                if let Err(e) = handle_command(cmd, &mut worker_state, ev_tx.clone()).await {
                    eprintln!("worker error: {e}");
                }
            }
        });
    });

    // Layout: sidebar + stack
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .hexpand(true)
        .vexpand(true)
        .build();

    let sidebar = gtk::StackSidebar::builder()
        .stack(&stack)
        .width_request(220)
        .build();

    root.append(&sidebar);
    root.append(&stack);

    // Pages
    let home = page_home(cfg.clone(), cmd_tx.clone());
    let jobs_history = page_jobs_history(cfg.clone(), cmd_tx.clone());
    let toolchains = page_toolchains(cfg.clone(), cmd_tx.clone());
    let projects = page_projects(cfg.clone(), cmd_tx.clone(), &window);
    let targets = page_targets(cfg.clone(), cmd_tx.clone(), &window);
    let evidence = page_evidence(cfg.clone(), cmd_tx.clone());
    let console = page_console(cfg.clone(), cmd_tx.clone(), &window);
    let settings = page_settings(cfg.clone());

    stack.add_titled(&home.page.container, Some("home"), "Home");
    stack.add_titled(&jobs_history.container, Some("jobs"), "Job History");
    stack.add_titled(&toolchains.page.container, Some("toolchains"), "Toolchains");
    stack.add_titled(&projects.page.container, Some("projects"), "Projects");
    stack.add_titled(&targets.page.container, Some("targets"), "Targets");
    stack.add_titled(&console.container, Some("console"), "Console");
    stack.add_titled(&evidence.container, Some("evidence"), "Evidence");
    stack.add_titled(&settings.container, Some("settings"), "Settings");

    // Clone page handles for event routing closure.
    let home_page_for_events = home.clone();
    let jobs_for_events = jobs_history.clone();
    let toolchains_for_events = toolchains.clone();
    let projects_for_events = projects.clone();
    let targets_for_events = targets.clone();
    let console_for_events = console.clone();
    let evidence_for_events = evidence.clone();
    let cfg_for_events = cfg.clone();

    // Event routing: drain worker events on the GTK thread.
    let state_for_events = state.clone();
    glib::source::timeout_add_local(Duration::from_millis(50), move || {
        loop {
            match ev_rx.try_recv() {
                Ok(ev) => match ev {
                    AppEvent::Log { page, line } => match page {
                        "home" => home_page_for_events.append(&line),
                        "jobs" => jobs_for_events.append(&line),
                        "toolchains" => toolchains_for_events.append(&line),
                        "projects" => projects_for_events.append(&line),
                        "targets" => targets_for_events.append(&line),
                        "console" => console_for_events.append(&line),
                        "evidence" => evidence_for_events.append(&line),
                        _ => {}
                    },
                    AppEvent::SetCurrentJob { job_id } => {
                        let mut state = state_for_events.lock().unwrap();
                        state.current_job_id = job_id;
                        let job_id = state.current_job_id.clone();
                        drop(state);
                        home_page_for_events.set_job_id(job_id.as_deref());
                        let mut cfg = cfg_for_events.lock().unwrap();
                        cfg.last_job_id = job_id.unwrap_or_default();
                        if let Err(err) = cfg.save() {
                            eprintln!("Failed to persist UI config: {err}");
                        }
                    }
                    AppEvent::HomeResetStatus => {
                        home_page_for_events.reset_status();
                    }
                    AppEvent::HomeState { state } => {
                        home_page_for_events.set_state(&state);
                    }
                    AppEvent::HomeProgress { progress } => {
                        home_page_for_events.set_progress(&progress);
                    }
                    AppEvent::HomeResult { result } => {
                        home_page_for_events.set_result(&result);
                    }
                    AppEvent::SetLastBuildApk { apk_path } => {
                        targets_for_events.set_apk_path(&apk_path);
                    }
                    AppEvent::SetCuttlefishBuildId { build_id } => {
                        targets_for_events.set_cuttlefish_build_id(&build_id);
                    }
                    AppEvent::ToolchainAvailable {
                        provider_id,
                        versions,
                    } => {
                        toolchains_for_events.set_available_versions(&provider_id, &versions);
                    }
                    AppEvent::ProjectTemplates { templates } => {
                        projects_for_events.set_templates(&templates);
                    }
                    AppEvent::ProjectToolchainSets { sets } => {
                        projects_for_events.set_toolchain_sets(&sets);
                    }
                    AppEvent::ProjectTargets { targets } => {
                        projects_for_events.set_targets(&targets);
                    }
                },
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return glib::ControlFlow::Break,
            }
        }
        glib::ControlFlow::Continue
    });

    // Hook cancel button to current_job_id from GTK-side state
    {
        let cfg = cfg.clone();
        let state = state.clone();
        let cmd_tx = cmd_tx.clone();
        home.cancel_btn.connect_clicked(move |_| {
            let cfg = cfg.lock().unwrap().clone();
            // If no current job, do nothing (silent).
            if state.lock().unwrap().current_job_id.is_some() {
                cmd_tx.send(UiCommand::HomeCancelCurrent { cfg }).ok();
            }
        });
    }

    window.set_child(Some(&root));
    window.present();
}

#[derive(Clone)]
struct Page {
    container: gtk::Box,
    buffer: gtk::TextBuffer,
    textview: gtk::TextView,
}

impl Page {
    fn append(&self, s: &str) {
        const MAX_CHARS: i32 = 200_000;

        let mut end = self.buffer.end_iter();
        self.buffer.insert(&mut end, s);

        if self.buffer.char_count() > MAX_CHARS {
            let mut start = self.buffer.start_iter();
            let mut cut = self.buffer.start_iter();
            cut.forward_chars(20_000);
            self.buffer.delete(&mut start, &mut cut);
        }

        let mut end = self.buffer.end_iter();
        self.textview.scroll_to_iter(&mut end, 0.0, false, 0.0, 0.0);
    }
}

#[derive(Clone)]
struct HomePage {
    page: Page,
    cancel_btn: gtk::Button,
    job_id_label: gtk::Label,
    state_label: gtk::Label,
    progress_label: gtk::Label,
    result_label: gtk::Label,
}

impl HomePage {
    fn append(&self, s: &str) {
        self.page.append(s);
    }

    fn set_job_id(&self, job_id: Option<&str>) {
        let label = job_id.unwrap_or("-");
        self.job_id_label.set_text(&format!("job_id: {label}"));
    }

    fn set_state(&self, state: &str) {
        self.state_label.set_text(&format!("state: {state}"));
    }

    fn set_progress(&self, progress: &str) {
        self.progress_label.set_text(&format!("progress: {progress}"));
    }

    fn set_result(&self, result: &str) {
        self.result_label.set_text(&format!("result: {result}"));
    }

    fn reset_status(&self) {
        self.set_job_id(None);
        self.set_state("-");
        self.set_progress("-");
        self.set_result("-");
    }
}

#[derive(Clone)]
struct TargetsPage {
    page: Page,
    apk_entry: gtk::Entry,
    cuttlefish_build_entry: gtk::Entry,
}

#[derive(Clone)]
struct ToolchainsPage {
    page: Page,
    sdk_version_combo: gtk::ComboBoxText,
    ndk_version_combo: gtk::ComboBoxText,
}

#[derive(Clone, Debug)]
struct ProjectTemplateOption {
    id: String,
    name: String,
}

#[derive(Clone, Debug)]
struct ToolchainSetOption {
    id: String,
    label: String,
}

#[derive(Clone, Debug)]
struct TargetOption {
    id: String,
    label: String,
}

#[derive(Clone)]
struct ProjectsPage {
    page: Page,
    template_combo: gtk::ComboBoxText,
    toolchain_set_combo: gtk::ComboBoxText,
    target_combo: gtk::ComboBoxText,
}

impl TargetsPage {
    fn append(&self, s: &str) {
        self.page.append(s);
    }

    fn set_apk_path(&self, path: &str) {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            self.apk_entry.set_text(trimmed);
        }
    }

    fn set_cuttlefish_build_id(&self, build_id: &str) {
        let trimmed = build_id.trim();
        if !trimmed.is_empty() {
            self.cuttlefish_build_entry.set_text(trimmed);
        }
    }
}

impl ToolchainsPage {
    fn append(&self, s: &str) {
        self.page.append(s);
    }

    fn set_available_versions(&self, provider_id: &str, versions: &[String]) {
        match provider_id {
            PROVIDER_SDK_ID => self.set_sdk_versions(versions),
            PROVIDER_NDK_ID => self.set_ndk_versions(versions),
            _ => {}
        }
    }

    fn set_sdk_versions(&self, versions: &[String]) {
        populate_combo_versions(&self.sdk_version_combo, versions, SDK_VERSION);
    }

    fn set_ndk_versions(&self, versions: &[String]) {
        populate_combo_versions(&self.ndk_version_combo, versions, NDK_VERSION);
    }
}

impl ProjectsPage {
    fn append(&self, s: &str) {
        self.page.append(s);
    }

    fn set_templates(&self, templates: &[ProjectTemplateOption]) {
        self.template_combo.remove_all();
        for tmpl in templates {
            let label = format!("{} ({})", tmpl.name, tmpl.id);
            self.template_combo.append(Some(tmpl.id.as_str()), &label);
        }
        if !templates.is_empty() {
            self.template_combo.set_active(Some(0));
        }
    }

    fn set_toolchain_sets(&self, sets: &[ToolchainSetOption]) {
        self.toolchain_set_combo.remove_all();
        self.toolchain_set_combo.append(Some("none"), "None");
        for set in sets {
            self.toolchain_set_combo
                .append(Some(set.id.as_str()), &set.label);
        }
        self.toolchain_set_combo.set_active(Some(0));
    }

    fn set_targets(&self, targets: &[TargetOption]) {
        self.target_combo.remove_all();
        self.target_combo.append(Some("none"), "None");
        for target in targets {
            self.target_combo.append(Some(target.id.as_str()), &target.label);
        }
        self.target_combo.set_active(Some(0));
    }
}

fn populate_combo_versions(combo: &gtk::ComboBoxText, versions: &[String], fallback: &str) {
    let current = combo.active_id().map(|id| id.to_string());
    combo.remove_all();
    if versions.is_empty() {
        if !fallback.trim().is_empty() {
            combo.append(Some(fallback), fallback);
            combo.set_active(Some(0));
        }
        return;
    }
    for version in versions {
        combo.append(Some(version.as_str()), version);
    }
    if let Some(current) = current {
        if let Some(index) = versions.iter().position(|v| v == &current) {
            combo.set_active(Some(index as u32));
            return;
        }
    }
    combo.set_active(Some(0));
}

fn combo_active_value(combo: &gtk::ComboBoxText) -> String {
    combo
        .active_id()
        .map(|id| id.to_string())
        .or_else(|| combo.active_text().map(|text| text.to_string()))
        .unwrap_or_default()
}

fn make_page(title: &str) -> Page {
    let container = gtk::Box::new(gtk::Orientation::Vertical, 8);
    container.set_margin_top(12);
    container.set_margin_bottom(12);
    container.set_margin_start(12);
    container.set_margin_end(12);

    let header = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .css_classes(vec!["title-2"])
        .build();

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .build();

    let textview = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .build();

    let buffer = textview.buffer();
    scroller.set_child(Some(&textview));

    container.append(&header);
    container.append(&scroller);

    Page { container, buffer, textview }
}

const KNOWN_JOB_TYPES: &[&str] = &[
    "demo.job",
    "workflow.pipeline",
    "project.create",
    "build.run",
    "toolchain.install",
    "toolchain.verify",
    "targets.install",
    "targets.launch",
    "targets.stop",
    "targets.cuttlefish.install",
    "targets.cuttlefish.start",
    "targets.cuttlefish.stop",
    "observe.support_bundle",
    "observe.evidence_bundle",
];

fn page_home(cfg: Arc<std::sync::Mutex<AppConfig>>, cmd_tx: mpsc::Sender<UiCommand>) -> HomePage {
    let page = make_page("Jobs — start + status");
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 8);

    let form = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let job_type_label = gtk::Label::builder().label("Job type").xalign(0.0).build();
    let job_type_entry = gtk::Entry::builder()
        .placeholder_text("job type")
        .hexpand(true)
        .build();
    let job_type_combo = gtk::ComboBoxText::new();
    for job_type in KNOWN_JOB_TYPES {
        job_type_combo.append(Some(job_type), job_type);
    }

    let params_label = gtk::Label::builder()
        .label("Params (key=value per line)")
        .xalign(0.0)
        .build();
    let params_scroller = gtk::ScrolledWindow::builder()
        .min_content_height(80)
        .hexpand(true)
        .build();
    let params_view = gtk::TextView::builder()
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .build();
    params_scroller.set_child(Some(&params_view));

    let project_id_label = gtk::Label::builder().label("Project id").xalign(0.0).build();
    let project_id_entry = gtk::Entry::builder()
        .placeholder_text("optional project id")
        .hexpand(true)
        .build();

    let target_id_label = gtk::Label::builder().label("Target id").xalign(0.0).build();
    let target_id_entry = gtk::Entry::builder()
        .placeholder_text("optional target id")
        .hexpand(true)
        .build();

    let toolchain_id_label = gtk::Label::builder().label("Toolchain set id").xalign(0.0).build();
    let toolchain_id_entry = gtk::Entry::builder()
        .placeholder_text("optional toolchain set id")
        .hexpand(true)
        .build();
    let correlation_id_label = gtk::Label::builder().label("Correlation id").xalign(0.0).build();
    let correlation_id_entry = gtk::Entry::builder()
        .placeholder_text("optional correlation id")
        .hexpand(true)
        .build();

    form.attach(&job_type_label, 0, 0, 1, 1);
    form.attach(&job_type_entry, 1, 0, 1, 1);
    form.attach(&job_type_combo, 2, 0, 1, 1);
    form.attach(&params_label, 0, 1, 1, 1);
    form.attach(&params_scroller, 1, 1, 2, 1);
    form.attach(&project_id_label, 0, 2, 1, 1);
    form.attach(&project_id_entry, 1, 2, 2, 1);
    form.attach(&target_id_label, 0, 3, 1, 1);
    form.attach(&target_id_entry, 1, 3, 2, 1);
    form.attach(&toolchain_id_label, 0, 4, 1, 1);
    form.attach(&toolchain_id_entry, 1, 4, 2, 1);
    form.attach(&correlation_id_label, 0, 5, 1, 1);
    form.attach(&correlation_id_entry, 1, 5, 2, 1);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let start_btn = gtk::Button::with_label("Start job");
    let cancel_btn = gtk::Button::with_label("Cancel current");
    let watch_label = gtk::Label::builder().label("Watch job").xalign(0.0).build();
    let watch_entry = gtk::Entry::builder()
        .placeholder_text("job id")
        .hexpand(true)
        .build();
    let watch_btn = gtk::Button::with_label("Watch");

    buttons.append(&start_btn);
    buttons.append(&cancel_btn);
    buttons.append(&watch_label);
    buttons.append(&watch_entry);
    buttons.append(&watch_btn);

    let status_frame = gtk::Frame::builder().label("Status").build();
    let status_grid = gtk::Grid::builder()
        .row_spacing(4)
        .column_spacing(8)
        .build();
    let job_id_label = gtk::Label::builder().label("job_id: -").xalign(0.0).build();
    let state_label = gtk::Label::builder().label("state: -").xalign(0.0).build();
    let progress_label = gtk::Label::builder().label("progress: -").xalign(0.0).build();
    let result_label = gtk::Label::builder().label("result: -").xalign(0.0).build();

    status_grid.attach(&job_id_label, 0, 0, 1, 1);
    status_grid.attach(&state_label, 0, 1, 1, 1);
    status_grid.attach(&progress_label, 0, 2, 1, 1);
    status_grid.attach(&result_label, 0, 3, 1, 1);
    status_frame.set_child(Some(&status_grid));

    controls.append(&form);
    controls.append(&buttons);
    controls.append(&status_frame);

    page.container
        .insert_child_after(&controls, Some(&page.container.first_child().unwrap()));

    {
        let cfg = cfg.lock().unwrap().clone();
        if !cfg.last_job_type.is_empty() {
            job_type_entry.set_text(&cfg.last_job_type);
            job_type_combo.set_active_id(Some(&cfg.last_job_type));
        } else {
            job_type_combo.set_active(Some(0));
            if let Some(text) = job_type_combo.active_text() {
                job_type_entry.set_text(&text);
            }
        }
        if !cfg.last_job_params.is_empty() {
            params_view
                .buffer()
                .set_text(&cfg.last_job_params);
        }
        if !cfg.last_job_project_id.is_empty() {
            project_id_entry.set_text(&cfg.last_job_project_id);
        }
        if !cfg.last_job_target_id.is_empty() {
            target_id_entry.set_text(&cfg.last_job_target_id);
        }
        if !cfg.last_job_toolchain_set_id.is_empty() {
            toolchain_id_entry.set_text(&cfg.last_job_toolchain_set_id);
        }
        if !cfg.last_correlation_id.is_empty() {
            correlation_id_entry.set_text(&cfg.last_correlation_id);
        }
        if !cfg.last_job_id.is_empty() {
            watch_entry.set_text(&cfg.last_job_id);
        }
    }

    let job_type_entry_select = job_type_entry.clone();
    job_type_combo.connect_changed(move |combo| {
        if let Some(text) = combo.active_text() {
            job_type_entry_select.set_text(&text);
        }
    });

    let cfg_start = cfg.clone();
    let cmd_tx_start = cmd_tx.clone();
    let params_view_start = params_view.clone();
    let job_type_entry_start = job_type_entry.clone();
    let project_id_entry_start = project_id_entry.clone();
    let target_id_entry_start = target_id_entry.clone();
    let toolchain_id_entry_start = toolchain_id_entry.clone();
    let correlation_id_entry_start = correlation_id_entry.clone();
    start_btn.connect_clicked(move |_| {
        let job_type = job_type_entry_start.text().to_string();
        let params_raw = text_view_text(&params_view_start);
        let project_id = project_id_entry_start.text().to_string();
        let target_id = target_id_entry_start.text().to_string();
        let toolchain_set_id = toolchain_id_entry_start.text().to_string();
        let correlation_id = correlation_id_entry_start.text().to_string();

        {
            let mut cfg = cfg_start.lock().unwrap();
            cfg.last_job_type = job_type.clone();
            cfg.last_job_params = params_raw.clone();
            cfg.last_job_project_id = project_id.clone();
            cfg.last_job_target_id = target_id.clone();
            cfg.last_job_toolchain_set_id = toolchain_set_id.clone();
            cfg.last_correlation_id = correlation_id.clone();
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }

        let cfg = cfg_start.lock().unwrap().clone();
        cmd_tx_start
            .send(UiCommand::HomeStartJob {
                cfg,
                job_type,
                params_raw,
                project_id,
                target_id,
                toolchain_set_id,
                correlation_id,
            })
            .ok();
    });

    let cfg_watch = cfg.clone();
    let cmd_tx_watch = cmd_tx.clone();
    let watch_entry_copy = watch_entry.clone();
    watch_btn.connect_clicked(move |_| {
        let job_id = watch_entry_copy.text().to_string();
        if job_id.trim().is_empty() {
            return;
        }
        {
            let mut cfg = cfg_watch.lock().unwrap();
            cfg.last_job_id = job_id.clone();
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_watch.lock().unwrap().clone();
        cmd_tx_watch
            .send(UiCommand::HomeWatchJob { cfg, job_id })
            .ok();
    });

    HomePage {
        page,
        cancel_btn,
        job_id_label,
        state_label,
        progress_label,
        result_label,
    }
}

fn page_jobs_history(cfg: Arc<std::sync::Mutex<AppConfig>>, cmd_tx: mpsc::Sender<UiCommand>) -> Page {
    let page = make_page("Job history — list + drill-down");
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 8);

    let list_frame = gtk::Frame::builder().label("List jobs").build();
    let list_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let job_types_entry = gtk::Entry::builder()
        .placeholder_text("job types (comma/space)")
        .hexpand(true)
        .build();
    let states_entry = gtk::Entry::builder()
        .placeholder_text("states (queued,running,success,failed,cancelled)")
        .hexpand(true)
        .build();
    let created_after_entry = gtk::Entry::builder()
        .placeholder_text("created after (unix ms)")
        .hexpand(true)
        .build();
    let created_before_entry = gtk::Entry::builder()
        .placeholder_text("created before (unix ms)")
        .hexpand(true)
        .build();
    let finished_after_entry = gtk::Entry::builder()
        .placeholder_text("finished after (unix ms)")
        .hexpand(true)
        .build();
    let finished_before_entry = gtk::Entry::builder()
        .placeholder_text("finished before (unix ms)")
        .hexpand(true)
        .build();
    let correlation_id_entry = gtk::Entry::builder()
        .placeholder_text("correlation id")
        .hexpand(true)
        .build();
    let page_size_entry = gtk::Entry::builder().text("50").hexpand(true).build();
    let page_token_entry = gtk::Entry::builder()
        .placeholder_text("page token")
        .hexpand(true)
        .build();

    let list_btn = gtk::Button::with_label("List jobs");

    list_grid.attach(&gtk::Label::new(Some("Job types")), 0, 0, 1, 1);
    list_grid.attach(&job_types_entry, 1, 0, 1, 1);
    list_grid.attach(&gtk::Label::new(Some("States")), 0, 1, 1, 1);
    list_grid.attach(&states_entry, 1, 1, 1, 1);
    list_grid.attach(&gtk::Label::new(Some("Created after")), 0, 2, 1, 1);
    list_grid.attach(&created_after_entry, 1, 2, 1, 1);
    list_grid.attach(&gtk::Label::new(Some("Created before")), 0, 3, 1, 1);
    list_grid.attach(&created_before_entry, 1, 3, 1, 1);
    list_grid.attach(&gtk::Label::new(Some("Finished after")), 0, 4, 1, 1);
    list_grid.attach(&finished_after_entry, 1, 4, 1, 1);
    list_grid.attach(&gtk::Label::new(Some("Finished before")), 0, 5, 1, 1);
    list_grid.attach(&finished_before_entry, 1, 5, 1, 1);
    list_grid.attach(&gtk::Label::new(Some("Correlation id")), 0, 6, 1, 1);
    list_grid.attach(&correlation_id_entry, 1, 6, 1, 1);
    list_grid.attach(&gtk::Label::new(Some("Page size")), 0, 7, 1, 1);
    list_grid.attach(&page_size_entry, 1, 7, 1, 1);
    list_grid.attach(&gtk::Label::new(Some("Page token")), 0, 8, 1, 1);
    list_grid.attach(&page_token_entry, 1, 8, 1, 1);
    list_grid.attach(&list_btn, 1, 9, 1, 1);

    list_frame.set_child(Some(&list_grid));

    let history_frame = gtk::Frame::builder().label("Job history").build();
    let history_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();
    let job_id_entry = gtk::Entry::builder()
        .placeholder_text("job id")
        .hexpand(true)
        .build();
    let kinds_entry = gtk::Entry::builder()
        .placeholder_text("kinds (state,progress,log,completed,failed)")
        .hexpand(true)
        .build();
    let after_entry = gtk::Entry::builder()
        .placeholder_text("after (unix ms)")
        .hexpand(true)
        .build();
    let before_entry = gtk::Entry::builder()
        .placeholder_text("before (unix ms)")
        .hexpand(true)
        .build();
    let history_page_size_entry = gtk::Entry::builder().text("200").hexpand(true).build();
    let history_page_token_entry = gtk::Entry::builder()
        .placeholder_text("page token")
        .hexpand(true)
        .build();
    let output_path_entry = gtk::Entry::builder()
        .placeholder_text("export output path (optional)")
        .hexpand(true)
        .build();

    let list_history_btn = gtk::Button::with_label("List history");
    let export_btn = gtk::Button::with_label("Export logs");

    history_grid.attach(&gtk::Label::new(Some("Job id")), 0, 0, 1, 1);
    history_grid.attach(&job_id_entry, 1, 0, 1, 1);
    history_grid.attach(&gtk::Label::new(Some("Kinds")), 0, 1, 1, 1);
    history_grid.attach(&kinds_entry, 1, 1, 1, 1);
    history_grid.attach(&gtk::Label::new(Some("After")), 0, 2, 1, 1);
    history_grid.attach(&after_entry, 1, 2, 1, 1);
    history_grid.attach(&gtk::Label::new(Some("Before")), 0, 3, 1, 1);
    history_grid.attach(&before_entry, 1, 3, 1, 1);
    history_grid.attach(&gtk::Label::new(Some("Page size")), 0, 4, 1, 1);
    history_grid.attach(&history_page_size_entry, 1, 4, 1, 1);
    history_grid.attach(&gtk::Label::new(Some("Page token")), 0, 5, 1, 1);
    history_grid.attach(&history_page_token_entry, 1, 5, 1, 1);
    history_grid.attach(&gtk::Label::new(Some("Output path")), 0, 6, 1, 1);
    history_grid.attach(&output_path_entry, 1, 6, 1, 1);
    history_grid.attach(&list_history_btn, 1, 7, 1, 1);
    history_grid.attach(&export_btn, 1, 8, 1, 1);

    history_frame.set_child(Some(&history_grid));

    controls.append(&list_frame);
    controls.append(&history_frame);

    page.container
        .insert_child_after(&controls, Some(&page.container.first_child().unwrap()));

    {
        let cfg = cfg.lock().unwrap().clone();
        if !cfg.last_job_type.is_empty() {
            job_types_entry.set_text(&cfg.last_job_type);
        }
        if !cfg.last_correlation_id.is_empty() {
            correlation_id_entry.set_text(&cfg.last_correlation_id);
        }
        if !cfg.last_job_id.is_empty() {
            job_id_entry.set_text(&cfg.last_job_id);
        }
    }

    let cfg_list = cfg.clone();
    let cmd_tx_list = cmd_tx.clone();
    let job_types_entry_list = job_types_entry.clone();
    let states_entry_list = states_entry.clone();
    let created_after_entry_list = created_after_entry.clone();
    let created_before_entry_list = created_before_entry.clone();
    let finished_after_entry_list = finished_after_entry.clone();
    let finished_before_entry_list = finished_before_entry.clone();
    let correlation_id_entry_list = correlation_id_entry.clone();
    let page_size_entry_list = page_size_entry.clone();
    let page_token_entry_list = page_token_entry.clone();
    list_btn.connect_clicked(move |_| {
        let page_size = page_size_entry_list.text().parse::<u32>().unwrap_or(50);
        let cfg = cfg_list.lock().unwrap().clone();
        cmd_tx_list
            .send(UiCommand::JobsList {
                cfg,
                job_types: job_types_entry_list.text().to_string(),
                states: states_entry_list.text().to_string(),
                created_after: created_after_entry_list.text().to_string(),
                created_before: created_before_entry_list.text().to_string(),
                finished_after: finished_after_entry_list.text().to_string(),
                finished_before: finished_before_entry_list.text().to_string(),
                correlation_id: correlation_id_entry_list.text().to_string(),
                page_size,
                page_token: page_token_entry_list.text().to_string(),
            })
            .ok();
    });

    let cfg_history = cfg.clone();
    let cmd_tx_history = cmd_tx.clone();
    let job_id_entry_history = job_id_entry.clone();
    let kinds_entry_history = kinds_entry.clone();
    let after_entry_history = after_entry.clone();
    let before_entry_history = before_entry.clone();
    let history_page_size_entry_history = history_page_size_entry.clone();
    let history_page_token_entry_history = history_page_token_entry.clone();
    list_history_btn.connect_clicked(move |_| {
        let page_size = history_page_size_entry_history.text().parse::<u32>().unwrap_or(200);
        let job_id = job_id_entry_history.text().to_string();
        {
            let mut cfg = cfg_history.lock().unwrap();
            cfg.last_job_id = job_id.clone();
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_history.lock().unwrap().clone();
        cmd_tx_history
            .send(UiCommand::JobsHistory {
                cfg,
                job_id,
                kinds: kinds_entry_history.text().to_string(),
                after: after_entry_history.text().to_string(),
                before: before_entry_history.text().to_string(),
                page_size,
                page_token: history_page_token_entry_history.text().to_string(),
            })
            .ok();
    });

    let cfg_export = cfg.clone();
    let cmd_tx_export = cmd_tx.clone();
    let job_id_entry_export = job_id_entry.clone();
    let output_path_entry_export = output_path_entry.clone();
    export_btn.connect_clicked(move |_| {
        let job_id = job_id_entry_export.text().to_string();
        {
            let mut cfg = cfg_export.lock().unwrap();
            cfg.last_job_id = job_id.clone();
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_export.lock().unwrap().clone();
        cmd_tx_export
            .send(UiCommand::JobsExportLogs {
                cfg,
                job_id,
                output_path: output_path_entry_export.text().to_string(),
            })
            .ok();
    });

    page
}

const PROVIDER_SDK_ID: &str = "provider-android-sdk-custom";
const PROVIDER_NDK_ID: &str = "provider-android-ndk-custom";
const SDK_VERSION: &str = "36.0.0";
const NDK_VERSION: &str = "r29";

fn page_toolchains(
    cfg: Arc<std::sync::Mutex<AppConfig>>,
    cmd_tx: mpsc::Sender<UiCommand>,
) -> ToolchainsPage {
    let page = make_page("Toolchains — ToolchainService");
    let actions = gtk::Box::new(gtk::Orientation::Vertical, 8);

    let job_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();
    let use_job_id_check = gtk::CheckButton::with_label("Use job id");
    let job_id_entry = gtk::Entry::builder()
        .placeholder_text("job id")
        .hexpand(true)
        .build();
    let correlation_id_entry = gtk::Entry::builder()
        .placeholder_text("correlation id")
        .hexpand(true)
        .build();
    job_grid.attach(&use_job_id_check, 0, 0, 1, 1);
    job_grid.attach(&job_id_entry, 1, 0, 1, 1);
    job_grid.attach(&gtk::Label::new(Some("Correlation id")), 0, 1, 1, 1);
    job_grid.attach(&correlation_id_entry, 1, 1, 1, 1);

    let row1 = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let list = gtk::Button::with_label("List providers");
    let list_sdk = gtk::Button::with_label("List available SDKs");
    let list_ndk = gtk::Button::with_label("List available NDKs");
    let list_sets = gtk::Button::with_label("List sets");
    row1.append(&list);
    row1.append(&list_sdk);
    row1.append(&list_ndk);
    row1.append(&list_sets);

    let version_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();
    let sdk_version_combo = gtk::ComboBoxText::new();
    sdk_version_combo.set_hexpand(true);
    sdk_version_combo.append(Some(SDK_VERSION), SDK_VERSION);
    sdk_version_combo.set_active(Some(0));
    let ndk_version_combo = gtk::ComboBoxText::new();
    ndk_version_combo.set_hexpand(true);
    ndk_version_combo.append(Some(NDK_VERSION), NDK_VERSION);
    ndk_version_combo.set_active(Some(0));
    let label_sdk_version = gtk::Label::builder().label("SDK version").xalign(0.0).build();
    let label_ndk_version = gtk::Label::builder().label("NDK version").xalign(0.0).build();
    version_grid.attach(&label_sdk_version, 0, 0, 1, 1);
    version_grid.attach(&sdk_version_combo, 1, 0, 1, 1);
    version_grid.attach(&label_ndk_version, 0, 1, 1, 1);
    version_grid.attach(&ndk_version_combo, 1, 1, 1, 1);

    let row2 = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let install_sdk = gtk::Button::with_label("Install SDK");
    let install_ndk = gtk::Button::with_label("Install NDK");
    let list_installed = gtk::Button::with_label("List installed");
    let verify_installed = gtk::Button::with_label("Verify installed");
    row2.append(&install_sdk);
    row2.append(&install_ndk);
    row2.append(&list_installed);
    row2.append(&verify_installed);

    let maintenance_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();
    let toolchain_id_entry = gtk::Entry::builder()
        .placeholder_text("Toolchain id")
        .hexpand(true)
        .build();
    let update_version_entry = gtk::Entry::builder()
        .placeholder_text("Update version")
        .hexpand(true)
        .build();
    let verify_update_check = gtk::CheckButton::with_label("Verify hash");
    verify_update_check.set_active(true);
    let remove_cached_check = gtk::CheckButton::with_label("Remove cached artifact");
    let force_uninstall_check = gtk::CheckButton::with_label("Force");
    let update_btn = gtk::Button::with_label("Update");
    let uninstall_btn = gtk::Button::with_label("Uninstall");
    let cleanup_btn = gtk::Button::with_label("Cleanup cache");
    let dry_run_check = gtk::CheckButton::with_label("Dry run");
    dry_run_check.set_active(true);
    let remove_all_check = gtk::CheckButton::with_label("Remove all cached");

    let label_toolchain_id = gtk::Label::builder().label("Toolchain id").xalign(0.0).build();
    let label_update_version = gtk::Label::builder().label("Target version").xalign(0.0).build();
    let label_cache = gtk::Label::builder().label("Cache cleanup").xalign(0.0).build();

    maintenance_grid.attach(&label_toolchain_id, 0, 0, 1, 1);
    maintenance_grid.attach(&toolchain_id_entry, 1, 0, 1, 1);
    maintenance_grid.attach(&label_update_version, 0, 1, 1, 1);
    maintenance_grid.attach(&update_version_entry, 1, 1, 1, 1);

    let maintenance_options = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    maintenance_options.append(&verify_update_check);
    maintenance_options.append(&remove_cached_check);
    maintenance_options.append(&force_uninstall_check);
    maintenance_grid.attach(&maintenance_options, 1, 2, 1, 1);

    let maintenance_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    maintenance_actions.append(&update_btn);
    maintenance_actions.append(&uninstall_btn);
    maintenance_grid.attach(&maintenance_actions, 1, 3, 1, 1);

    let cache_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    cache_actions.append(&cleanup_btn);
    cache_actions.append(&dry_run_check);
    cache_actions.append(&remove_all_check);
    maintenance_grid.attach(&label_cache, 0, 4, 1, 1);
    maintenance_grid.attach(&cache_actions, 1, 4, 1, 1);

    let set_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();
    let sdk_set_entry = gtk::Entry::builder()
        .placeholder_text("SDK toolchain id")
        .hexpand(true)
        .build();
    let ndk_set_entry = gtk::Entry::builder()
        .placeholder_text("NDK toolchain id")
        .hexpand(true)
        .build();
    let display_name_entry = gtk::Entry::builder()
        .placeholder_text("Toolchain set display name")
        .hexpand(true)
        .build();
    let create_set_btn = gtk::Button::with_label("Create set");
    let active_set_entry = gtk::Entry::builder()
        .placeholder_text("Active toolchain set id")
        .hexpand(true)
        .build();
    let set_active_btn = gtk::Button::with_label("Set active");
    let get_active_btn = gtk::Button::with_label("Get active");

    let label_sdk_set = gtk::Label::builder().label("SDK id").xalign(0.0).build();
    let label_ndk_set = gtk::Label::builder().label("NDK id").xalign(0.0).build();
    let label_display = gtk::Label::builder().label("Display name").xalign(0.0).build();
    let label_active = gtk::Label::builder().label("Active set").xalign(0.0).build();

    set_grid.attach(&label_sdk_set, 0, 0, 1, 1);
    set_grid.attach(&sdk_set_entry, 1, 0, 1, 1);
    set_grid.attach(&label_ndk_set, 0, 1, 1, 1);
    set_grid.attach(&ndk_set_entry, 1, 1, 1, 1);
    set_grid.attach(&label_display, 0, 2, 1, 1);
    set_grid.attach(&display_name_entry, 1, 2, 1, 1);
    set_grid.attach(&create_set_btn, 1, 3, 1, 1);

    set_grid.attach(&label_active, 0, 4, 1, 1);
    let active_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    active_row.append(&active_set_entry);
    active_row.append(&set_active_btn);
    active_row.append(&get_active_btn);
    set_grid.attach(&active_row, 1, 4, 1, 1);

    actions.append(&job_grid);
    actions.append(&row1);
    actions.append(&version_grid);
    actions.append(&row2);
    actions.append(&maintenance_grid);
    actions.append(&set_grid);
    page.container.insert_child_after(&actions, Some(&page.container.first_child().unwrap()));

    {
        let cfg = cfg.lock().unwrap().clone();
        if !cfg.last_job_id.is_empty() {
            job_id_entry.set_text(&cfg.last_job_id);
        }
        if !cfg.last_correlation_id.is_empty() {
            correlation_id_entry.set_text(&cfg.last_correlation_id);
        }
    }

    let cfg_list = cfg.clone();
    let cmd_tx_list = cmd_tx.clone();
    list.connect_clicked(move |_| {
        let cfg = cfg_list.lock().unwrap().clone();
        cmd_tx_list.send(UiCommand::ToolchainListProviders { cfg }).ok();
    });

    let cfg_sdk = cfg.clone();
    let cmd_tx_sdk = cmd_tx.clone();
    list_sdk.connect_clicked(move |_| {
        let cfg = cfg_sdk.lock().unwrap().clone();
        cmd_tx_sdk
            .send(UiCommand::ToolchainListAvailable {
                cfg,
                provider_id: PROVIDER_SDK_ID.into(),
            })
            .ok();
    });

    let cfg_ndk = cfg.clone();
    let cmd_tx_ndk = cmd_tx.clone();
    list_ndk.connect_clicked(move |_| {
        let cfg = cfg_ndk.lock().unwrap().clone();
        cmd_tx_ndk
            .send(UiCommand::ToolchainListAvailable {
                cfg,
                provider_id: PROVIDER_NDK_ID.into(),
            })
            .ok();
    });

    let cfg_list_sets = cfg.clone();
    let cmd_tx_list_sets = cmd_tx.clone();
    list_sets.connect_clicked(move |_| {
        let cfg = cfg_list_sets.lock().unwrap().clone();
        cmd_tx_list_sets
            .send(UiCommand::ToolchainListSets { cfg })
            .ok();
    });

    let cfg_install_sdk = cfg.clone();
    let cmd_tx_install_sdk = cmd_tx.clone();
    let sdk_version_combo_install = sdk_version_combo.clone();
    let use_job_id_install_sdk = use_job_id_check.clone();
    let job_id_entry_install_sdk = job_id_entry.clone();
    let correlation_entry_install_sdk = correlation_id_entry.clone();
    install_sdk.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_install_sdk.text().to_string();
        let correlation_id = correlation_entry_install_sdk.text().to_string();
        let job_id = if use_job_id_install_sdk.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_install_sdk.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_install_sdk.lock().unwrap().clone();
        let version = combo_active_value(&sdk_version_combo_install);
        cmd_tx_install_sdk
            .send(UiCommand::ToolchainInstall {
                cfg,
                provider_id: PROVIDER_SDK_ID.into(),
                version,
                verify: true,
                job_id,
                correlation_id,
            })
            .ok();
    });

    let cfg_install_ndk = cfg.clone();
    let cmd_tx_install_ndk = cmd_tx.clone();
    let ndk_version_combo_install = ndk_version_combo.clone();
    let use_job_id_install_ndk = use_job_id_check.clone();
    let job_id_entry_install_ndk = job_id_entry.clone();
    let correlation_entry_install_ndk = correlation_id_entry.clone();
    install_ndk.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_install_ndk.text().to_string();
        let correlation_id = correlation_entry_install_ndk.text().to_string();
        let job_id = if use_job_id_install_ndk.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_install_ndk.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_install_ndk.lock().unwrap().clone();
        let version = combo_active_value(&ndk_version_combo_install);
        cmd_tx_install_ndk
            .send(UiCommand::ToolchainInstall {
                cfg,
                provider_id: PROVIDER_NDK_ID.into(),
                version,
                verify: true,
                job_id,
                correlation_id,
            })
            .ok();
    });

    let cfg_list_installed = cfg.clone();
    let cmd_tx_list_installed = cmd_tx.clone();
    list_installed.connect_clicked(move |_| {
        let cfg = cfg_list_installed.lock().unwrap().clone();
        cmd_tx_list_installed
            .send(UiCommand::ToolchainListInstalled {
                cfg,
                kind: ToolchainKind::Unspecified,
            })
            .ok();
    });

    let cfg_verify_installed = cfg.clone();
    let cmd_tx_verify_installed = cmd_tx.clone();
    let use_job_id_verify = use_job_id_check.clone();
    let job_id_entry_verify = job_id_entry.clone();
    let correlation_entry_verify = correlation_id_entry.clone();
    verify_installed.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_verify.text().to_string();
        let correlation_id = correlation_entry_verify.text().to_string();
        let job_id = if use_job_id_verify.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_verify_installed.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_verify_installed.lock().unwrap().clone();
        cmd_tx_verify_installed
            .send(UiCommand::ToolchainVerifyInstalled {
                cfg,
                job_id,
                correlation_id,
            })
            .ok();
    });

    let cfg_update = cfg.clone();
    let cmd_tx_update = cmd_tx.clone();
    let toolchain_id_entry_update = toolchain_id_entry.clone();
    let update_version_entry_update = update_version_entry.clone();
    let verify_update_check_update = verify_update_check.clone();
    let remove_cached_check_update = remove_cached_check.clone();
    let use_job_id_update = use_job_id_check.clone();
    let job_id_entry_update = job_id_entry.clone();
    let correlation_entry_update = correlation_id_entry.clone();
    update_btn.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_update.text().to_string();
        let correlation_id = correlation_entry_update.text().to_string();
        let job_id = if use_job_id_update.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_update.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_update.lock().unwrap().clone();
        cmd_tx_update
            .send(UiCommand::ToolchainUpdate {
                cfg,
                toolchain_id: toolchain_id_entry_update.text().to_string(),
                version: update_version_entry_update.text().to_string(),
                verify: verify_update_check_update.is_active(),
                remove_cached: remove_cached_check_update.is_active(),
                job_id,
                correlation_id,
            })
            .ok();
    });

    let cfg_uninstall = cfg.clone();
    let cmd_tx_uninstall = cmd_tx.clone();
    let toolchain_id_entry_uninstall = toolchain_id_entry.clone();
    let remove_cached_check_uninstall = remove_cached_check.clone();
    let force_uninstall_check_uninstall = force_uninstall_check.clone();
    let use_job_id_uninstall = use_job_id_check.clone();
    let job_id_entry_uninstall = job_id_entry.clone();
    let correlation_entry_uninstall = correlation_id_entry.clone();
    uninstall_btn.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_uninstall.text().to_string();
        let correlation_id = correlation_entry_uninstall.text().to_string();
        let job_id = if use_job_id_uninstall.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_uninstall.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_uninstall.lock().unwrap().clone();
        cmd_tx_uninstall
            .send(UiCommand::ToolchainUninstall {
                cfg,
                toolchain_id: toolchain_id_entry_uninstall.text().to_string(),
                remove_cached: remove_cached_check_uninstall.is_active(),
                force: force_uninstall_check_uninstall.is_active(),
                job_id,
                correlation_id,
            })
            .ok();
    });

    let cfg_cleanup = cfg.clone();
    let cmd_tx_cleanup = cmd_tx.clone();
    let dry_run_check_cleanup = dry_run_check.clone();
    let remove_all_check_cleanup = remove_all_check.clone();
    let use_job_id_cleanup = use_job_id_check.clone();
    let job_id_entry_cleanup = job_id_entry.clone();
    let correlation_entry_cleanup = correlation_id_entry.clone();
    cleanup_btn.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_cleanup.text().to_string();
        let correlation_id = correlation_entry_cleanup.text().to_string();
        let job_id = if use_job_id_cleanup.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_cleanup.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_cleanup.lock().unwrap().clone();
        cmd_tx_cleanup
            .send(UiCommand::ToolchainCleanupCache {
                cfg,
                dry_run: dry_run_check_cleanup.is_active(),
                remove_all: remove_all_check_cleanup.is_active(),
                job_id,
                correlation_id,
            })
            .ok();
    });

    let cfg_create_set = cfg.clone();
    let cmd_tx_create_set = cmd_tx.clone();
    let sdk_entry_create = sdk_set_entry.clone();
    let ndk_entry_create = ndk_set_entry.clone();
    let display_entry_create = display_name_entry.clone();
    create_set_btn.connect_clicked(move |_| {
        let cfg = cfg_create_set.lock().unwrap().clone();
        let sdk_id = sdk_entry_create.text().to_string();
        let ndk_id = ndk_entry_create.text().to_string();
        let display_name = display_entry_create.text().to_string();
        cmd_tx_create_set
            .send(UiCommand::ToolchainCreateSet {
                cfg,
                sdk_toolchain_id: if sdk_id.trim().is_empty() { None } else { Some(sdk_id) },
                ndk_toolchain_id: if ndk_id.trim().is_empty() { None } else { Some(ndk_id) },
                display_name,
            })
            .ok();
    });

    let cfg_set_active = cfg.clone();
    let cmd_tx_set_active = cmd_tx.clone();
    let active_entry_set = active_set_entry.clone();
    set_active_btn.connect_clicked(move |_| {
        let cfg = cfg_set_active.lock().unwrap().clone();
        cmd_tx_set_active
            .send(UiCommand::ToolchainSetActive {
                cfg,
                toolchain_set_id: active_entry_set.text().to_string(),
            })
            .ok();
    });

    let cfg_get_active = cfg.clone();
    let cmd_tx_get_active = cmd_tx.clone();
    get_active_btn.connect_clicked(move |_| {
        let cfg = cfg_get_active.lock().unwrap().clone();
        cmd_tx_get_active
            .send(UiCommand::ToolchainGetActive { cfg })
            .ok();
    });
    {
        let cfg = cfg.lock().unwrap().clone();
        cmd_tx
            .send(UiCommand::ToolchainListAvailable {
                cfg: cfg.clone(),
                provider_id: PROVIDER_SDK_ID.into(),
            })
            .ok();
        cmd_tx
            .send(UiCommand::ToolchainListAvailable {
                cfg,
                provider_id: PROVIDER_NDK_ID.into(),
            })
            .ok();
    }
    ToolchainsPage {
        page,
        sdk_version_combo,
        ndk_version_combo,
    }
}

fn page_projects(
    cfg: Arc<std::sync::Mutex<AppConfig>>,
    cmd_tx: mpsc::Sender<UiCommand>,
    parent: &gtk::ApplicationWindow,
) -> ProjectsPage {
    let page = make_page("Projects — ProjectService");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let refresh_templates = gtk::Button::with_label("Refresh templates");
    let refresh_defaults = gtk::Button::with_label("Refresh defaults");
    let list_recent = gtk::Button::with_label("List recent");
    let open_btn = gtk::Button::with_label("Open project");
    let create_btn = gtk::Button::with_label("Create project");
    row.append(&refresh_templates);
    row.append(&refresh_defaults);
    row.append(&list_recent);
    row.append(&open_btn);
    row.append(&create_btn);
    page.container
        .insert_child_after(&row, Some(&page.container.first_child().unwrap()));

    let job_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();
    let use_job_id_check = gtk::CheckButton::with_label("Use job id");
    let job_id_entry = gtk::Entry::builder()
        .placeholder_text("job id")
        .hexpand(true)
        .build();
    let correlation_id_entry = gtk::Entry::builder()
        .placeholder_text("correlation id")
        .hexpand(true)
        .build();
    job_grid.attach(&use_job_id_check, 0, 0, 1, 1);
    job_grid.attach(&job_id_entry, 1, 0, 1, 1);
    job_grid.attach(&gtk::Label::new(Some("Correlation id")), 0, 1, 1, 1);
    job_grid.attach(&correlation_id_entry, 1, 1, 1, 1);
    page.container.insert_child_after(&job_grid, Some(&row));

    let form = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let template_combo = gtk::ComboBoxText::new();
    template_combo.set_hexpand(true);
    let name_entry = gtk::Entry::builder()
        .placeholder_text("Project name")
        .hexpand(true)
        .build();
    let path_entry = gtk::Entry::builder()
        .placeholder_text("Project path")
        .hexpand(true)
        .build();
    let browse_btn = gtk::Button::with_label("Browse...");

    let label_template = gtk::Label::builder().label("Template").xalign(0.0).build();
    let label_name = gtk::Label::builder().label("Name").xalign(0.0).build();
    let label_path = gtk::Label::builder().label("Path").xalign(0.0).build();

    form.attach(&label_template, 0, 0, 1, 1);
    form.attach(&template_combo, 1, 0, 1, 1);
    form.attach(&label_name, 0, 1, 1, 1);
    form.attach(&name_entry, 1, 1, 1, 1);
    form.attach(&label_path, 0, 2, 1, 1);
    let path_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    path_row.append(&path_entry);
    path_row.append(&browse_btn);
    form.attach(&path_row, 1, 2, 1, 1);

    page.container.insert_child_after(&form, Some(&job_grid));

    let config_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();
    let project_id_entry = gtk::Entry::builder()
        .placeholder_text("Project id")
        .hexpand(true)
        .build();
    let toolchain_set_combo = gtk::ComboBoxText::new();
    toolchain_set_combo.set_hexpand(true);
    toolchain_set_combo.append(Some("none"), "None");
    toolchain_set_combo.set_active(Some(0));
    let default_target_combo = gtk::ComboBoxText::new();
    default_target_combo.set_hexpand(true);
    default_target_combo.append(Some("none"), "None");
    default_target_combo.set_active(Some(0));
    let config_btn = gtk::Button::with_label("Set config");
    let use_defaults_btn = gtk::Button::with_label("Use active defaults");

    let label_project_id = gtk::Label::builder().label("Project id").xalign(0.0).build();
    let label_toolchain = gtk::Label::builder().label("Toolchain set").xalign(0.0).build();
    let label_target = gtk::Label::builder().label("Default target").xalign(0.0).build();

    config_grid.attach(&label_project_id, 0, 0, 1, 1);
    config_grid.attach(&project_id_entry, 1, 0, 1, 1);
    config_grid.attach(&label_toolchain, 0, 1, 1, 1);
    config_grid.attach(&toolchain_set_combo, 1, 1, 1, 1);
    config_grid.attach(&label_target, 0, 2, 1, 1);
    config_grid.attach(&default_target_combo, 1, 2, 1, 1);
    let config_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    config_actions.append(&config_btn);
    config_actions.append(&use_defaults_btn);
    config_grid.attach(&config_actions, 1, 3, 1, 1);

    page.container.insert_child_after(&config_grid, Some(&form));

    {
        let cfg = cfg.lock().unwrap().clone();
        if !cfg.last_job_id.is_empty() {
            job_id_entry.set_text(&cfg.last_job_id);
        }
        if !cfg.last_correlation_id.is_empty() {
            correlation_id_entry.set_text(&cfg.last_correlation_id);
        }
    }

    let cfg_refresh = cfg.clone();
    let cmd_tx_refresh = cmd_tx.clone();
    refresh_templates.connect_clicked(move |_| {
        let cfg = cfg_refresh.lock().unwrap().clone();
        cmd_tx_refresh.send(UiCommand::ProjectListTemplates { cfg }).ok();
    });

    let cfg_defaults = cfg.clone();
    let cmd_tx_defaults = cmd_tx.clone();
    refresh_defaults.connect_clicked(move |_| {
        let cfg = cfg_defaults.lock().unwrap().clone();
        cmd_tx_defaults
            .send(UiCommand::ProjectLoadDefaults { cfg })
            .ok();
    });

    let cfg_recent = cfg.clone();
    let cmd_tx_recent = cmd_tx.clone();
    list_recent.connect_clicked(move |_| {
        let cfg = cfg_recent.lock().unwrap().clone();
        cmd_tx_recent.send(UiCommand::ProjectListRecent { cfg }).ok();
    });

    let cfg_open = cfg.clone();
    let cmd_tx_open = cmd_tx.clone();
    let path_entry_open = path_entry.clone();
    open_btn.connect_clicked(move |_| {
        let cfg = cfg_open.lock().unwrap().clone();
        cmd_tx_open
            .send(UiCommand::ProjectOpen {
                cfg,
                path: path_entry_open.text().to_string(),
            })
            .ok();
    });

    let cfg_create = cfg.clone();
    let cmd_tx_create = cmd_tx.clone();
    let path_entry_create = path_entry.clone();
    let name_entry_create = name_entry.clone();
    let template_combo_create = template_combo.clone();
    let use_job_id_create = use_job_id_check.clone();
    let job_id_entry_create = job_id_entry.clone();
    let correlation_entry_create = correlation_id_entry.clone();
    create_btn.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_create.text().to_string();
        let correlation_id = correlation_entry_create.text().to_string();
        let job_id = if use_job_id_create.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_create.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_create.lock().unwrap().clone();
        let template_id = template_combo_create
            .active_id()
            .map(|id| id.to_string())
            .unwrap_or_default();
        cmd_tx_create
            .send(UiCommand::ProjectCreate {
                cfg,
                name: name_entry_create.text().to_string(),
                path: path_entry_create.text().to_string(),
                template_id,
                job_id,
                correlation_id,
            })
            .ok();
    });

    let cfg_config = cfg.clone();
    let cmd_tx_config = cmd_tx.clone();
    let project_id_entry_config = project_id_entry.clone();
    let toolchain_set_combo_config = toolchain_set_combo.clone();
    let default_target_combo_config = default_target_combo.clone();
    config_btn.connect_clicked(move |_| {
        let cfg = cfg_config.lock().unwrap().clone();
        let project_id = project_id_entry_config.text().to_string();
        let toolchain_set_id = toolchain_set_combo_config
            .active_id()
            .map(|id| id.to_string())
            .unwrap_or_default();
        let default_target_id = default_target_combo_config
            .active_id()
            .map(|id| id.to_string())
            .unwrap_or_default();
        cmd_tx_config
            .send(UiCommand::ProjectSetConfig {
                cfg,
                project_id,
                toolchain_set_id: if toolchain_set_id.trim().is_empty() {
                    None
                } else if toolchain_set_id == "none" {
                    None
                } else {
                    Some(toolchain_set_id)
                },
                default_target_id: if default_target_id.trim().is_empty() {
                    None
                } else if default_target_id == "none" {
                    None
                } else {
                    Some(default_target_id)
                },
            })
            .ok();
    });

    let cfg_defaults = cfg.clone();
    let cmd_tx_defaults = cmd_tx.clone();
    let project_id_entry_defaults = project_id_entry.clone();
    use_defaults_btn.connect_clicked(move |_| {
        let cfg = cfg_defaults.lock().unwrap().clone();
        cmd_tx_defaults
            .send(UiCommand::ProjectUseActiveDefaults {
                cfg,
                project_id: project_id_entry_defaults.text().to_string(),
            })
            .ok();
    });

    let parent_window = parent.clone();
    let path_entry_browse = path_entry.clone();
    browse_btn.connect_clicked(move |_| {
        let dialog = gtk::FileChooserNative::new(
            Some("Select Project Folder"),
            Some(&parent_window),
            gtk::FileChooserAction::SelectFolder,
            Some("Open"),
            Some("Cancel"),
        );

        let current = path_entry_browse.text().to_string();
        if !current.trim().is_empty() {
            let folder = gtk::gio::File::for_path(current.trim());
            let _ = dialog.set_current_folder(Some(&folder));
        }

        let path_entry_dialog = path_entry_browse.clone();
        dialog.connect_response(move |dialog, response| {
            if response == gtk::ResponseType::Accept {
                if let Some(file) = dialog.file() {
                    if let Some(path) = file.path() {
                        if let Some(path_str) = path.to_str() {
                            path_entry_dialog.set_text(path_str);
                        }
                    }
                }
            }
            dialog.destroy();
        });
        dialog.show();
    });

    ProjectsPage {
        page,
        template_combo,
        toolchain_set_combo,
        target_combo: default_target_combo,
    }
}

fn page_targets(
    cfg: Arc<std::sync::Mutex<AppConfig>>,
    cmd_tx: mpsc::Sender<UiCommand>,
    parent: &gtk::ApplicationWindow,
) -> TargetsPage {
    let page = make_page("Targets — TargetService (ADB + Cuttlefish + live logcat stream)");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let list = gtk::Button::with_label("List targets");
    let stream = gtk::Button::with_label("Stream logcat (sample)");
    row.append(&list);
    row.append(&stream);
    page.container.insert_child_after(&row, Some(&page.container.first_child().unwrap()));

    let job_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();
    let use_job_id_check = gtk::CheckButton::with_label("Use job id");
    let job_id_entry = gtk::Entry::builder()
        .placeholder_text("job id")
        .hexpand(true)
        .build();
    let correlation_id_entry = gtk::Entry::builder()
        .placeholder_text("correlation id")
        .hexpand(true)
        .build();
    job_grid.attach(&use_job_id_check, 0, 0, 1, 1);
    job_grid.attach(&job_id_entry, 1, 0, 1, 1);
    job_grid.attach(&gtk::Label::new(Some("Correlation id")), 0, 1, 1, 1);
    job_grid.attach(&correlation_id_entry, 1, 1, 1, 1);
    page.container.insert_child_after(&job_grid, Some(&row));

    let status = gtk::Button::with_label("Status");
    let web_ui = gtk::Button::with_label("Web UI");
    let env_ui = gtk::Button::with_label("Env control");
    let docs = gtk::Button::with_label("Docs");
    let install = gtk::Button::with_label("Install");
    let resolve_build = gtk::Button::with_label("Resolve build");
    let start = gtk::Button::with_label("Start");
    let stop = gtk::Button::with_label("Stop");

    let cuttlefish_buttons = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .column_homogeneous(true)
        .build();
    let cuttlefish_header = gtk::Label::builder().label("Cuttlefish").xalign(0.0).build();
    cuttlefish_buttons.attach(&cuttlefish_header, 0, 0, 4, 1);
    cuttlefish_buttons.attach(&status, 0, 1, 1, 1);
    cuttlefish_buttons.attach(&web_ui, 1, 1, 1, 1);
    cuttlefish_buttons.attach(&env_ui, 2, 1, 1, 1);
    cuttlefish_buttons.attach(&docs, 3, 1, 1, 1);
    cuttlefish_buttons.attach(&install, 0, 2, 1, 1);
    cuttlefish_buttons.attach(&resolve_build, 1, 2, 1, 1);
    cuttlefish_buttons.attach(&start, 2, 2, 1, 1);
    cuttlefish_buttons.attach(&stop, 3, 2, 1, 1);

    let default_target_serial = std::env::var("AADK_CUTTLEFISH_ADB_SERIAL")
        .unwrap_or_else(|_| "127.0.0.1:6520".into());

    let default_branch = std::env::var("AADK_CUTTLEFISH_BRANCH").unwrap_or_default();
    let default_cuttlefish_target = std::env::var("AADK_CUTTLEFISH_TARGET").unwrap_or_default();
    let default_build_id = std::env::var("AADK_CUTTLEFISH_BUILD_ID").unwrap_or_default();

    let cuttlefish_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();
    let cuttlefish_branch_entry = gtk::Entry::builder()
        .placeholder_text("Cuttlefish branch (optional)")
        .text(default_branch)
        .hexpand(true)
        .build();
    let cuttlefish_target_entry = gtk::Entry::builder()
        .placeholder_text("Cuttlefish target (optional)")
        .text(default_cuttlefish_target)
        .hexpand(true)
        .build();
    let cuttlefish_build_entry = gtk::Entry::builder()
        .placeholder_text("Cuttlefish build id (optional)")
        .text(default_build_id)
        .hexpand(true)
        .build();

    let label_branch = gtk::Label::builder().label("Branch").xalign(0.0).build();
    let label_target = gtk::Label::builder().label("Target").xalign(0.0).build();
    let label_build_id = gtk::Label::builder().label("Build id").xalign(0.0).build();

    cuttlefish_grid.attach(&label_branch, 0, 0, 1, 1);
    cuttlefish_grid.attach(&cuttlefish_branch_entry, 1, 0, 1, 1);
    cuttlefish_grid.attach(&label_target, 0, 1, 1, 1);
    cuttlefish_grid.attach(&cuttlefish_target_entry, 1, 1, 1, 1);
    cuttlefish_grid.attach(&label_build_id, 0, 2, 1, 1);
    cuttlefish_grid.attach(&cuttlefish_build_entry, 1, 2, 1, 1);

    let form = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let target_entry = gtk::Entry::builder()
        .text(default_target_serial)
        .hexpand(true)
        .build();
    let apk_entry = gtk::Entry::builder()
        .placeholder_text("Absolute APK path")
        .hexpand(true)
        .build();
    let apk_browse = gtk::Button::with_label("Browse...");
    let app_id_entry = gtk::Entry::builder()
        .text("com.example.sampleconsole")
        .hexpand(true)
        .build();
    let activity_entry = gtk::Entry::builder()
        .text(".MainActivity")
        .hexpand(true)
        .build();

    let label_target = gtk::Label::builder().label("Target id").xalign(0.0).build();
    let label_apk = gtk::Label::builder().label("APK path").xalign(0.0).build();
    let label_app_id = gtk::Label::builder().label("Application id").xalign(0.0).build();
    let label_activity = gtk::Label::builder().label("Activity").xalign(0.0).build();

    form.attach(&label_target, 0, 0, 1, 1);
    form.attach(&target_entry, 1, 0, 1, 1);
    form.attach(&label_apk, 0, 1, 1, 1);
    let apk_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    apk_row.append(&apk_entry);
    apk_row.append(&apk_browse);
    form.attach(&apk_row, 1, 1, 1, 1);
    form.attach(&label_app_id, 0, 2, 1, 1);
    form.attach(&app_id_entry, 1, 2, 1, 1);
    form.attach(&label_activity, 0, 3, 1, 1);
    form.attach(&activity_entry, 1, 3, 1, 1);

    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let install_apk = gtk::Button::with_label("Install APK");
    let launch_app = gtk::Button::with_label("Launch app");
    action_row.append(&install_apk);
    action_row.append(&launch_app);
    form.attach(&action_row, 1, 4, 1, 1);

    let default_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let set_default_btn = gtk::Button::with_label("Set default");
    let get_default_btn = gtk::Button::with_label("Get default");
    default_row.append(&set_default_btn);
    default_row.append(&get_default_btn);
    form.attach(&default_row, 1, 5, 1, 1);

    page.container.insert_child_after(&cuttlefish_buttons, Some(&job_grid));
    page.container.insert_child_after(&cuttlefish_grid, Some(&cuttlefish_buttons));
    page.container.insert_child_after(&form, Some(&cuttlefish_grid));

    {
        let cfg = cfg.lock().unwrap().clone();
        if !cfg.last_job_id.is_empty() {
            job_id_entry.set_text(&cfg.last_job_id);
        }
        if !cfg.last_correlation_id.is_empty() {
            correlation_id_entry.set_text(&cfg.last_correlation_id);
        }
    }

    let cfg_list = cfg.clone();
    let cmd_tx_list = cmd_tx.clone();
    list.connect_clicked(move |_| {
        let cfg = cfg_list.lock().unwrap().clone();
        cmd_tx_list.send(UiCommand::TargetsList { cfg }).ok();
    });

    let cfg_stream = cfg.clone();
    let cmd_tx_stream = cmd_tx.clone();
    stream.connect_clicked(move |_| {
        let cfg = cfg_stream.lock().unwrap().clone();
        cmd_tx_stream.send(UiCommand::TargetsStreamLogcat {
            cfg,
            target_id: "target-sample-pixel".into(),
            filter: "".into(),
        }).ok();
    });

    let cfg_status = cfg.clone();
    let cmd_tx_status = cmd_tx.clone();
    status.connect_clicked(move |_| {
        let cfg = cfg_status.lock().unwrap().clone();
        cmd_tx_status
            .send(UiCommand::TargetsCuttlefishStatus { cfg })
            .ok();
    });

    let cfg_resolve = cfg.clone();
    let cmd_tx_resolve = cmd_tx.clone();
    let branch_entry_resolve = cuttlefish_branch_entry.clone();
    let target_entry_resolve = cuttlefish_target_entry.clone();
    let build_id_entry_resolve = cuttlefish_build_entry.clone();
    resolve_build.connect_clicked(move |_| {
        let cfg = cfg_resolve.lock().unwrap().clone();
        cmd_tx_resolve
            .send(UiCommand::TargetsResolveCuttlefishBuild {
                cfg,
                branch: branch_entry_resolve.text().to_string(),
                target: target_entry_resolve.text().to_string(),
                build_id: build_id_entry_resolve.text().to_string(),
            })
            .ok();
    });

    let page_web = page.clone();
    web_ui.connect_clicked(move |_| {
        let url = std::env::var("AADK_CUTTLEFISH_WEBRTC_URL")
            .unwrap_or_else(|_| "https://localhost:8443".into());
        match gtk::gio::AppInfo::launch_default_for_uri(
            &url,
            None::<&gtk::gio::AppLaunchContext>,
        ) {
            Ok(_) => page_web.append(&format!("Opened Cuttlefish UI: {url}\n")),
            Err(err) => page_web.append(&format!("Failed to open Cuttlefish UI: {err}\n")),
        }
    });

    let page_env = page.clone();
    env_ui.connect_clicked(move |_| {
        let url = std::env::var("AADK_CUTTLEFISH_ENV_URL")
            .unwrap_or_else(|_| "https://localhost:1443".into());
        match gtk::gio::AppInfo::launch_default_for_uri(
            &url,
            None::<&gtk::gio::AppLaunchContext>,
        ) {
            Ok(_) => page_env.append(&format!("Opened Cuttlefish env control: {url}\n")),
            Err(err) => page_env.append(&format!("Failed to open Cuttlefish env control: {err}\n")),
        }
    });

    let page_docs = page.clone();
    docs.connect_clicked(move |_| {
        let url = "https://source.android.com/docs/devices/cuttlefish/get-started";
        match gtk::gio::AppInfo::launch_default_for_uri(
            url,
            None::<&gtk::gio::AppLaunchContext>,
        ) {
            Ok(_) => page_docs.append(&format!("Opened Cuttlefish docs: {url}\n")),
            Err(err) => page_docs.append(&format!("Failed to open Cuttlefish docs: {err}\n")),
        }
    });

    let cfg_start = cfg.clone();
    let cmd_tx_start = cmd_tx.clone();
    let use_job_id_start = use_job_id_check.clone();
    let job_id_entry_start = job_id_entry.clone();
    let correlation_entry_start = correlation_id_entry.clone();
    start.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_start.text().to_string();
        let correlation_id = correlation_entry_start.text().to_string();
        let job_id = if use_job_id_start.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_start.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_start.lock().unwrap().clone();
        cmd_tx_start
            .send(UiCommand::TargetsStartCuttlefish {
                cfg,
                show_full_ui: true,
                job_id,
                correlation_id,
            })
            .ok();
    });

    let cfg_stop = cfg.clone();
    let cmd_tx_stop = cmd_tx.clone();
    let use_job_id_stop = use_job_id_check.clone();
    let job_id_entry_stop = job_id_entry.clone();
    let correlation_entry_stop = correlation_id_entry.clone();
    stop.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_stop.text().to_string();
        let correlation_id = correlation_entry_stop.text().to_string();
        let job_id = if use_job_id_stop.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_stop.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_stop.lock().unwrap().clone();
        cmd_tx_stop
            .send(UiCommand::TargetsStopCuttlefish {
                cfg,
                job_id,
                correlation_id,
            })
            .ok();
    });

    let cfg_set_default = cfg.clone();
    let cmd_tx_set_default = cmd_tx.clone();
    let target_entry_default = target_entry.clone();
    set_default_btn.connect_clicked(move |_| {
        let cfg = cfg_set_default.lock().unwrap().clone();
        cmd_tx_set_default
            .send(UiCommand::TargetsSetDefault {
                cfg,
                target_id: target_entry_default.text().to_string(),
            })
            .ok();
    });

    let cfg_get_default = cfg.clone();
    let cmd_tx_get_default = cmd_tx.clone();
    get_default_btn.connect_clicked(move |_| {
        let cfg = cfg_get_default.lock().unwrap().clone();
        cmd_tx_get_default
            .send(UiCommand::TargetsGetDefault { cfg })
            .ok();
    });

    let cfg_install = cfg.clone();
    let cmd_tx_install = cmd_tx.clone();
    let branch_entry_install = cuttlefish_branch_entry.clone();
    let target_entry_install = cuttlefish_target_entry.clone();
    let build_entry_install = cuttlefish_build_entry.clone();
    let use_job_id_install = use_job_id_check.clone();
    let job_id_entry_install = job_id_entry.clone();
    let correlation_entry_install = correlation_id_entry.clone();
    install.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_install.text().to_string();
        let correlation_id = correlation_entry_install.text().to_string();
        let job_id = if use_job_id_install.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_install.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_install.lock().unwrap().clone();
        cmd_tx_install
            .send(UiCommand::TargetsInstallCuttlefish {
                cfg,
                force: false,
                branch: branch_entry_install.text().to_string(),
                target: target_entry_install.text().to_string(),
                build_id: build_entry_install.text().to_string(),
                job_id,
                correlation_id,
            })
            .ok();
    });

    let parent_window = parent.clone();
    let apk_entry_browse = apk_entry.clone();
    apk_browse.connect_clicked(move |_| {
        let dialog = gtk::FileChooserNative::new(
            Some("Select APK"),
            Some(&parent_window),
            gtk::FileChooserAction::Open,
            Some("Open"),
            Some("Cancel"),
        );

        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Android APK"));
        filter.add_pattern("*.apk");
        dialog.add_filter(&filter);
        dialog.set_filter(&filter);

        let current = apk_entry_browse.text().to_string();
        if !current.trim().is_empty() {
            if let Some(parent_dir) = Path::new(current.trim()).parent() {
                let folder = gtk::gio::File::for_path(parent_dir);
                let _ = dialog.set_current_folder(Some(&folder));
            }
        }

        let apk_entry_dialog = apk_entry_browse.clone();
        dialog.connect_response(move |dialog, response| {
            if response == gtk::ResponseType::Accept {
                if let Some(file) = dialog.file() {
                    if let Some(path) = file.path() {
                        if let Some(path_str) = path.to_str() {
                            apk_entry_dialog.set_text(path_str);
                        }
                    }
                }
            }
            dialog.destroy();
        });
        dialog.show();
    });

    let cfg_install_apk = cfg.clone();
    let cmd_tx_install_apk = cmd_tx.clone();
    let target_entry_install = target_entry.clone();
    let apk_entry_install = apk_entry.clone();
    let use_job_id_install_apk = use_job_id_check.clone();
    let job_id_entry_install_apk = job_id_entry.clone();
    let correlation_entry_install_apk = correlation_id_entry.clone();
    install_apk.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_install_apk.text().to_string();
        let correlation_id = correlation_entry_install_apk.text().to_string();
        let job_id = if use_job_id_install_apk.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_install_apk.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_install_apk.lock().unwrap().clone();
        cmd_tx_install_apk
            .send(UiCommand::TargetsInstallApk {
                cfg,
                target_id: target_entry_install.text().to_string(),
                apk_path: apk_entry_install.text().to_string(),
                job_id,
                correlation_id,
            })
            .ok();
    });

    let cfg_launch = cfg.clone();
    let cmd_tx_launch = cmd_tx.clone();
    let target_entry_launch = target_entry.clone();
    let app_id_entry_launch = app_id_entry.clone();
    let activity_entry_launch = activity_entry.clone();
    let use_job_id_launch = use_job_id_check.clone();
    let job_id_entry_launch = job_id_entry.clone();
    let correlation_entry_launch = correlation_id_entry.clone();
    launch_app.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_launch.text().to_string();
        let correlation_id = correlation_entry_launch.text().to_string();
        let job_id = if use_job_id_launch.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_launch.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_launch.lock().unwrap().clone();
        cmd_tx_launch
            .send(UiCommand::TargetsLaunchApp {
                cfg,
                target_id: target_entry_launch.text().to_string(),
                application_id: app_id_entry_launch.text().to_string(),
                activity: activity_entry_launch.text().to_string(),
                job_id,
                correlation_id,
            })
            .ok();
    });

    TargetsPage {
        page,
        apk_entry,
        cuttlefish_build_entry: cuttlefish_build_entry.clone(),
    }
}

fn page_console(
    cfg: Arc<std::sync::Mutex<AppConfig>>,
    cmd_tx: mpsc::Sender<UiCommand>,
    parent: &gtk::ApplicationWindow,
) -> Page {
    let page = make_page("Console — BuildService (Gradle)");

    let form = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let project_entry = gtk::Entry::builder()
        .placeholder_text("Project path or id (recent project id)")
        .hexpand(true)
        .build();
    let project_browse = gtk::Button::with_label("Browse...");
    let module_entry = gtk::Entry::builder()
        .placeholder_text("Module (optional, e.g. app or :app)")
        .hexpand(true)
        .build();
    let variant_name_entry = gtk::Entry::builder()
        .placeholder_text("Variant name override (e.g. demoDebug)")
        .hexpand(true)
        .build();
    let tasks_entry = gtk::Entry::builder()
        .placeholder_text("Tasks (comma/space separated, e.g. assembleDebug lint)")
        .hexpand(true)
        .build();
    let args_entry = gtk::Entry::builder()
        .placeholder_text("Gradle args, e.g. --stacktrace -Pfoo=bar")
        .hexpand(true)
        .build();

    let variant_combo = gtk::DropDown::from_strings(&["debug", "release"]);
    variant_combo.set_selected(0);

    let clean_check = gtk::CheckButton::with_label("Clean first");

    let run = gtk::Button::with_label("Build");

    let label_project = gtk::Label::builder().label("Project").xalign(0.0).build();
    let label_module = gtk::Label::builder().label("Module").xalign(0.0).build();
    let label_variant = gtk::Label::builder().label("Variant").xalign(0.0).build();
    let label_variant_name = gtk::Label::builder().label("Variant name").xalign(0.0).build();
    let label_tasks = gtk::Label::builder().label("Tasks").xalign(0.0).build();
    let label_args = gtk::Label::builder().label("Gradle args").xalign(0.0).build();

    form.attach(&label_project, 0, 0, 1, 1);
    let project_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    project_row.append(&project_entry);
    project_row.append(&project_browse);
    form.attach(&project_row, 1, 0, 1, 1);

    form.attach(&label_module, 0, 1, 1, 1);
    form.attach(&module_entry, 1, 1, 1, 1);

    form.attach(&label_variant, 0, 2, 1, 1);
    form.attach(&variant_combo, 1, 2, 1, 1);

    form.attach(&label_variant_name, 0, 3, 1, 1);
    form.attach(&variant_name_entry, 1, 3, 1, 1);

    form.attach(&label_tasks, 0, 4, 1, 1);
    form.attach(&tasks_entry, 1, 4, 1, 1);

    form.attach(&label_args, 0, 5, 1, 1);
    form.attach(&args_entry, 1, 5, 1, 1);

    let options_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    options_row.append(&clean_check);
    form.attach(&options_row, 1, 6, 1, 1);

    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    action_row.append(&run);
    form.attach(&action_row, 1, 7, 1, 1);

    page.container.insert_child_after(&form, Some(&page.container.first_child().unwrap()));

    let job_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();
    let use_job_id_check = gtk::CheckButton::with_label("Use job id");
    let job_id_entry = gtk::Entry::builder()
        .placeholder_text("job id")
        .hexpand(true)
        .build();
    let correlation_id_entry = gtk::Entry::builder()
        .placeholder_text("correlation id")
        .hexpand(true)
        .build();
    job_grid.attach(&use_job_id_check, 0, 0, 1, 1);
    job_grid.attach(&job_id_entry, 1, 0, 1, 1);
    job_grid.attach(&gtk::Label::new(Some("Correlation id")), 0, 1, 1, 1);
    job_grid.attach(&correlation_id_entry, 1, 1, 1, 1);
    page.container.insert_child_after(&job_grid, Some(&form));

    {
        let cfg = cfg.lock().unwrap().clone();
        if !cfg.last_job_id.is_empty() {
            job_id_entry.set_text(&cfg.last_job_id);
        }
        if !cfg.last_correlation_id.is_empty() {
            correlation_id_entry.set_text(&cfg.last_correlation_id);
        }
    }

    let artifacts_label = gtk::Label::builder()
        .label("Artifacts (filters)")
        .xalign(0.0)
        .build();

    let artifact_form = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let artifact_modules_entry = gtk::Entry::builder()
        .placeholder_text("Modules filter (comma/space separated)")
        .hexpand(true)
        .build();
    let artifact_variant_entry = gtk::Entry::builder()
        .placeholder_text("Variant filter (leave empty to use dropdown)")
        .hexpand(true)
        .build();
    let artifact_types_entry = gtk::Entry::builder()
        .placeholder_text("Types: apk,aab,aar,mapping,test")
        .hexpand(true)
        .build();
    let artifact_name_entry = gtk::Entry::builder()
        .placeholder_text("Name contains")
        .hexpand(true)
        .build();
    let artifact_path_entry = gtk::Entry::builder()
        .placeholder_text("Path contains")
        .hexpand(true)
        .build();

    let list_artifacts = gtk::Button::with_label("List artifacts");

    let label_artifact_modules = gtk::Label::builder().label("Modules").xalign(0.0).build();
    let label_artifact_variant = gtk::Label::builder().label("Variant").xalign(0.0).build();
    let label_artifact_types = gtk::Label::builder().label("Types").xalign(0.0).build();
    let label_artifact_name = gtk::Label::builder().label("Name contains").xalign(0.0).build();
    let label_artifact_path = gtk::Label::builder().label("Path contains").xalign(0.0).build();

    artifact_form.attach(&label_artifact_modules, 0, 0, 1, 1);
    artifact_form.attach(&artifact_modules_entry, 1, 0, 1, 1);
    artifact_form.attach(&label_artifact_variant, 0, 1, 1, 1);
    artifact_form.attach(&artifact_variant_entry, 1, 1, 1, 1);
    artifact_form.attach(&label_artifact_types, 0, 2, 1, 1);
    artifact_form.attach(&artifact_types_entry, 1, 2, 1, 1);
    artifact_form.attach(&label_artifact_name, 0, 3, 1, 1);
    artifact_form.attach(&artifact_name_entry, 1, 3, 1, 1);
    artifact_form.attach(&label_artifact_path, 0, 4, 1, 1);
    artifact_form.attach(&artifact_path_entry, 1, 4, 1, 1);

    let list_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    list_row.append(&list_artifacts);
    artifact_form.attach(&list_row, 1, 5, 1, 1);

    page.container.insert_child_after(&artifacts_label, Some(&job_grid));
    page.container.insert_child_after(&artifact_form, Some(&artifacts_label));

    let parent_window = parent.clone();
    let project_entry_browse = project_entry.clone();
    project_browse.connect_clicked(move |_| {
        let dialog = gtk::FileChooserNative::new(
            Some("Select Project Folder"),
            Some(&parent_window),
            gtk::FileChooserAction::SelectFolder,
            Some("Open"),
            Some("Cancel"),
        );

        let current = project_entry_browse.text().to_string();
        if !current.trim().is_empty() {
            let folder = gtk::gio::File::for_path(current.trim());
            let _ = dialog.set_current_folder(Some(&folder));
        }

        let project_entry_dialog = project_entry_browse.clone();
        dialog.connect_response(move |dialog, response| {
            if response == gtk::ResponseType::Accept {
                if let Some(file) = dialog.file() {
                    if let Some(path) = file.path() {
                        if let Some(path_str) = path.to_str() {
                            project_entry_dialog.set_text(path_str);
                        }
                    }
                }
            }
            dialog.destroy();
        });
        dialog.show();
    });

    let cfg_run = cfg.clone();
    let cmd_tx_run = cmd_tx.clone();
    let project_entry_run = project_entry.clone();
    let module_entry_run = module_entry.clone();
    let variant_name_entry_run = variant_name_entry.clone();
    let tasks_entry_run = tasks_entry.clone();
    let args_entry_run = args_entry.clone();
    let variant_combo_run = variant_combo.clone();
    let clean_check_run = clean_check.clone();
    let use_job_id_run = use_job_id_check.clone();
    let job_id_entry_run = job_id_entry.clone();
    let correlation_entry_run = correlation_id_entry.clone();
    run.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_run.text().to_string();
        let correlation_id = correlation_entry_run.text().to_string();
        let job_id = if use_job_id_run.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_run.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_run.lock().unwrap().clone();
        let project_ref = project_entry_run.text().to_string();
        let module = module_entry_run.text().to_string();
        let variant_name = variant_name_entry_run.text().to_string();
        let tasks = parse_list_tokens(&tasks_entry_run.text());
        let gradle_args = parse_gradle_args(&args_entry_run.text());
        let variant = match variant_combo_run.selected() {
            1 => BuildVariant::Release,
            _ => BuildVariant::Debug,
        };
        let clean_first = clean_check_run.is_active();
        cmd_tx_run
            .send(UiCommand::BuildRun {
                cfg,
                project_ref,
                variant,
                variant_name,
                module,
                tasks,
                clean_first,
                gradle_args,
                job_id,
                correlation_id,
            })
            .ok();
    });

    let cfg_list = cfg.clone();
    let cmd_tx_list = cmd_tx.clone();
    let project_entry_list = project_entry.clone();
    let variant_combo_list = variant_combo.clone();
    let artifact_modules_entry_list = artifact_modules_entry.clone();
    let artifact_variant_entry_list = artifact_variant_entry.clone();
    let artifact_types_entry_list = artifact_types_entry.clone();
    let artifact_name_entry_list = artifact_name_entry.clone();
    let artifact_path_entry_list = artifact_path_entry.clone();
    list_artifacts.connect_clicked(move |_| {
        let cfg = cfg_list.lock().unwrap().clone();
        let project_ref = project_entry_list.text().to_string();
        let variant = match variant_combo_list.selected() {
            1 => BuildVariant::Release,
            _ => BuildVariant::Debug,
        };
        let modules = parse_list_tokens(&artifact_modules_entry_list.text());
        let variant_filter = artifact_variant_entry_list.text().trim().to_string();
        let types = parse_artifact_types(&artifact_types_entry_list.text());
        let name_contains = artifact_name_entry_list.text().trim().to_string();
        let path_contains = artifact_path_entry_list.text().trim().to_string();

        let filter = ArtifactFilter {
            modules,
            variant: variant_filter,
            types: types.into_iter().map(|t| t as i32).collect(),
            name_contains,
            path_contains,
        };

        cmd_tx_list
            .send(UiCommand::BuildListArtifacts {
                cfg,
                project_ref,
                variant,
                filter,
            })
            .ok();
    });

    page
}

fn page_evidence(cfg: Arc<std::sync::Mutex<AppConfig>>, cmd_tx: mpsc::Sender<UiCommand>) -> Page {
    let page = make_page("Evidence — ObserveService (runs + bundle export)");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let list_runs = gtk::Button::with_label("List runs");
    let export_support = gtk::Button::with_label("Export support bundle");
    let export_evidence = gtk::Button::with_label("Export evidence bundle");
    row.append(&list_runs);
    row.append(&export_support);
    row.append(&export_evidence);
    page.container
        .insert_child_after(&row, Some(&page.container.first_child().unwrap()));

    let job_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();
    let use_job_id_check = gtk::CheckButton::with_label("Use job id");
    let job_id_entry = gtk::Entry::builder()
        .placeholder_text("job id")
        .hexpand(true)
        .build();
    let correlation_id_entry = gtk::Entry::builder()
        .placeholder_text("correlation id")
        .hexpand(true)
        .build();
    job_grid.attach(&use_job_id_check, 0, 0, 1, 1);
    job_grid.attach(&job_id_entry, 1, 0, 1, 1);
    job_grid.attach(&gtk::Label::new(Some("Correlation id")), 0, 1, 1, 1);
    job_grid.attach(&correlation_id_entry, 1, 1, 1, 1);
    page.container.insert_child_after(&job_grid, Some(&row));

    let form = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let run_id_entry = gtk::Entry::builder()
        .placeholder_text("Run id for evidence bundle")
        .hexpand(true)
        .build();
    let recent_limit_entry = gtk::Entry::builder()
        .text("10")
        .hexpand(true)
        .build();

    let include_logs = gtk::CheckButton::with_label("Include logs");
    include_logs.set_active(true);
    let include_config = gtk::CheckButton::with_label("Include config");
    include_config.set_active(true);
    let include_toolchain = gtk::CheckButton::with_label("Include toolchain provenance");
    include_toolchain.set_active(true);
    let include_recent = gtk::CheckButton::with_label("Include recent runs");
    include_recent.set_active(true);

    let label_run_id = gtk::Label::builder()
        .label("Evidence run id")
        .xalign(0.0)
        .build();
    let label_limit = gtk::Label::builder()
        .label("Recent runs limit")
        .xalign(0.0)
        .build();

    form.attach(&label_run_id, 0, 0, 1, 1);
    form.attach(&run_id_entry, 1, 0, 1, 1);
    form.attach(&label_limit, 0, 1, 1, 1);
    form.attach(&recent_limit_entry, 1, 1, 1, 1);

    let checkbox_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    checkbox_row.append(&include_logs);
    checkbox_row.append(&include_config);
    checkbox_row.append(&include_toolchain);
    checkbox_row.append(&include_recent);
    form.attach(&checkbox_row, 1, 2, 1, 1);

    page.container.insert_child_after(&form, Some(&job_grid));

    {
        let cfg = cfg.lock().unwrap().clone();
        if !cfg.last_job_id.is_empty() {
            job_id_entry.set_text(&cfg.last_job_id);
        }
        if !cfg.last_correlation_id.is_empty() {
            correlation_id_entry.set_text(&cfg.last_correlation_id);
        }
    }

    let cfg_list = cfg.clone();
    let cmd_tx_list = cmd_tx.clone();
    list_runs.connect_clicked(move |_| {
        let cfg = cfg_list.lock().unwrap().clone();
        cmd_tx_list.send(UiCommand::ObserveListRuns { cfg }).ok();
    });

    let cfg_support = cfg.clone();
    let cmd_tx_support = cmd_tx.clone();
    let include_logs_support = include_logs.clone();
    let include_config_support = include_config.clone();
    let include_toolchain_support = include_toolchain.clone();
    let include_recent_support = include_recent.clone();
    let recent_limit_support = recent_limit_entry.clone();
    let use_job_id_support = use_job_id_check.clone();
    let job_id_entry_support = job_id_entry.clone();
    let correlation_entry_support = correlation_id_entry.clone();
    export_support.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_support.text().to_string();
        let correlation_id = correlation_entry_support.text().to_string();
        let job_id = if use_job_id_support.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_support.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_support.lock().unwrap().clone();
        let limit = recent_limit_support
            .text()
            .parse::<u32>()
            .unwrap_or(10);
        cmd_tx_support
            .send(UiCommand::ObserveExportSupport {
                cfg,
                include_logs: include_logs_support.is_active(),
                include_config: include_config_support.is_active(),
                include_toolchain_provenance: include_toolchain_support.is_active(),
                include_recent_runs: include_recent_support.is_active(),
                recent_runs_limit: limit,
                job_id,
                correlation_id,
            })
            .ok();
    });

    let cfg_evidence = cfg.clone();
    let cmd_tx_evidence = cmd_tx.clone();
    let run_id_evidence = run_id_entry.clone();
    let use_job_id_evidence = use_job_id_check.clone();
    let job_id_entry_evidence = job_id_entry.clone();
    let correlation_entry_evidence = correlation_id_entry.clone();
    export_evidence.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_evidence.text().to_string();
        let correlation_id = correlation_entry_evidence.text().to_string();
        let job_id = if use_job_id_evidence.is_active() && !job_id_raw.trim().is_empty() {
            Some(job_id_raw.clone())
        } else {
            None
        };
        {
            let mut cfg = cfg_evidence.lock().unwrap();
            if !job_id_raw.trim().is_empty() {
                cfg.last_job_id = job_id_raw.clone();
            }
            if !correlation_id.trim().is_empty() {
                cfg.last_correlation_id = correlation_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_evidence.lock().unwrap().clone();
        cmd_tx_evidence
            .send(UiCommand::ObserveExportEvidence {
                cfg,
                run_id: run_id_evidence.text().to_string(),
                job_id,
                correlation_id,
            })
            .ok();
    });

    page
}

fn page_settings(cfg: Arc<std::sync::Mutex<AppConfig>>) -> Page {
    let page = make_page("Settings — service addresses");

    let form = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let add_row = |row: i32, label: &str, initial: String, setter: Box<dyn Fn(String) + 'static>| {
        let l = gtk::Label::builder().label(label).xalign(0.0).build();
        let e = gtk::Entry::builder().text(initial).hexpand(true).build();
        e.connect_changed(move |ent| setter(ent.text().to_string()));
        form.attach(&l, 0, row, 1, 1);
        form.attach(&e, 1, row, 1, 1);
    };

    let cfg1 = cfg.clone();
    add_row(
        0,
        "JobService",
        cfg.lock().unwrap().job_addr.clone(),
        Box::new(move |v| {
            let mut cfg = cfg1.lock().unwrap();
            cfg.job_addr = v;
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }),
    );

    let cfg2 = cfg.clone();
    add_row(
        1,
        "ToolchainService",
        cfg.lock().unwrap().toolchain_addr.clone(),
        Box::new(move |v| {
            let mut cfg = cfg2.lock().unwrap();
            cfg.toolchain_addr = v;
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }),
    );

    let cfg3 = cfg.clone();
    add_row(
        2,
        "ProjectService",
        cfg.lock().unwrap().project_addr.clone(),
        Box::new(move |v| {
            let mut cfg = cfg3.lock().unwrap();
            cfg.project_addr = v;
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }),
    );

    let cfg4 = cfg.clone();
    add_row(
        3,
        "BuildService",
        cfg.lock().unwrap().build_addr.clone(),
        Box::new(move |v| {
            let mut cfg = cfg4.lock().unwrap();
            cfg.build_addr = v;
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }),
    );

    let cfg5 = cfg.clone();
    add_row(
        4,
        "TargetService",
        cfg.lock().unwrap().targets_addr.clone(),
        Box::new(move |v| {
            let mut cfg = cfg5.lock().unwrap();
            cfg.targets_addr = v;
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }),
    );

    let cfg6 = cfg.clone();
    add_row(
        5,
        "ObserveService",
        cfg.lock().unwrap().observe_addr.clone(),
        Box::new(move |v| {
            let mut cfg = cfg6.lock().unwrap();
            cfg.observe_addr = v;
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }),
    );

    page.container.insert_child_after(&form, Some(&page.container.first_child().unwrap()));
    page
}

fn parse_gradle_args(raw: &str) -> Vec<KeyValue> {
    raw.split_whitespace()
        .filter(|token| !token.trim().is_empty())
        .map(|token| KeyValue {
            key: token.to_string(),
            value: String::new(),
        })
        .collect()
}

fn text_view_text(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    let start = buffer.start_iter();
    let end = buffer.end_iter();
    buffer.text(&start, &end, false).to_string()
}

fn parse_list_tokens(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .map(|token| token.trim())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_string())
        .collect()
}

fn parse_kv_lines(raw: &str) -> Vec<KeyValue> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let (key, value) = match trimmed.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => (trimmed, ""),
            };
            if key.is_empty() {
                None
            } else {
                Some(KeyValue {
                    key: key.to_string(),
                    value: value.to_string(),
                })
            }
        })
        .collect()
}

fn to_optional_id(raw: &str) -> Option<Id> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(Id {
            value: trimmed.to_string(),
        })
    }
}

fn run_id_from_optional(raw: &str) -> Option<RunId> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(RunId {
            value: trimmed.to_string(),
        })
    }
}

fn kv_pairs(items: &[KeyValue]) -> String {
    items
        .iter()
        .map(|kv| format!("{}={}", kv.key, kv.value))
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_job_states(raw: &str) -> (Vec<i32>, Vec<String>) {
    let mut states = Vec::new();
    let mut unknown = Vec::new();
    for token in parse_list_tokens(raw) {
        let state = match token.to_ascii_lowercase().as_str() {
            "queued" => Some(JobState::Queued),
            "running" => Some(JobState::Running),
            "success" | "succeeded" => Some(JobState::Success),
            "failed" | "failure" => Some(JobState::Failed),
            "cancelled" | "canceled" => Some(JobState::Cancelled),
            _ => None,
        };
        if let Some(state) = state {
            states.push(state as i32);
        } else {
            unknown.push(token);
        }
    }
    (states, unknown)
}

fn parse_job_event_kinds(raw: &str) -> (Vec<i32>, Vec<String>) {
    let mut kinds = Vec::new();
    let mut unknown = Vec::new();
    for token in parse_list_tokens(raw) {
        let kind = match token.to_ascii_lowercase().as_str() {
            "state" | "state_changed" => Some(JobEventKind::StateChanged),
            "progress" => Some(JobEventKind::Progress),
            "log" | "logs" => Some(JobEventKind::Log),
            "completed" | "complete" => Some(JobEventKind::Completed),
            "failed" | "failure" => Some(JobEventKind::Failed),
            _ => None,
        };
        if let Some(kind) = kind {
            kinds.push(kind as i32);
        } else {
            unknown.push(token);
        }
    }
    (kinds, unknown)
}

fn parse_optional_millis(raw: &str) -> Result<Option<i64>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<i64>()
        .map(Some)
        .map_err(|_| format!("invalid unix millis: {trimmed}"))
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

impl JobSummary {
    fn from_proto(job: &Job) -> Self {
        let state = JobState::try_from(job.state).unwrap_or(JobState::Unspecified);
        Self {
            job_id: job.job_id.as_ref().map(|id| id.value.clone()).unwrap_or_default(),
            job_type: job.job_type.clone(),
            state: format!("{state:?}"),
            created_at_unix_millis: job.created_at.as_ref().map(|ts| ts.unix_millis).unwrap_or_default(),
            started_at_unix_millis: job.started_at.as_ref().map(|ts| ts.unix_millis),
            finished_at_unix_millis: job.finished_at.as_ref().map(|ts| ts.unix_millis),
            display_name: job.display_name.clone(),
            correlation_id: job.correlation_id.clone(),
            run_id: job.run_id.as_ref().map(|id| id.value.clone()).unwrap_or_default(),
            project_id: job.project_id.as_ref().map(|id| id.value.clone()).unwrap_or_default(),
            target_id: job.target_id.as_ref().map(|id| id.value.clone()).unwrap_or_default(),
            toolchain_set_id: job.toolchain_set_id.as_ref().map(|id| id.value.clone()).unwrap_or_default(),
        }
    }
}

impl LogExportEvent {
    fn from_proto(event: &JobEvent) -> Self {
        let at_unix_millis = event.at.as_ref().map(|ts| ts.unix_millis).unwrap_or_default();
        match event.payload.as_ref() {
            Some(JobPayload::StateChanged(state)) => {
                let state = JobState::try_from(state.new_state).unwrap_or(JobState::Unspecified);
                Self {
                    at_unix_millis,
                    kind: "state_changed".into(),
                    summary: format!("{state:?}"),
                    data: None,
                }
            }
            Some(JobPayload::Progress(progress)) => {
                if let Some(p) = progress.progress.as_ref() {
                    let metrics = kv_pairs(&p.metrics);
                    let summary = if metrics.is_empty() {
                        format!("{}% {}", p.percent, p.phase)
                    } else {
                        format!("{}% {} ({metrics})", p.percent, p.phase)
                    };
                    Self {
                        at_unix_millis,
                        kind: "progress".into(),
                        summary,
                        data: None,
                    }
                } else {
                    Self {
                        at_unix_millis,
                        kind: "progress".into(),
                        summary: "missing progress payload".into(),
                        data: None,
                    }
                }
            }
            Some(JobPayload::Log(log)) => {
                if let Some(chunk) = log.chunk.as_ref() {
                    let summary = format!("stream={} truncated={}", chunk.stream, chunk.truncated);
                    let data = Some(String::from_utf8_lossy(&chunk.data).to_string());
                    Self {
                        at_unix_millis,
                        kind: "log".into(),
                        summary,
                        data,
                    }
                } else {
                    Self {
                        at_unix_millis,
                        kind: "log".into(),
                        summary: "missing log chunk".into(),
                        data: None,
                    }
                }
            }
            Some(JobPayload::Completed(completed)) => {
                let outputs = kv_pairs(&completed.outputs);
                let summary = if outputs.is_empty() {
                    completed.summary.clone()
                } else {
                    format!("{} ({outputs})", completed.summary)
                };
                Self {
                    at_unix_millis,
                    kind: "completed".into(),
                    summary,
                    data: None,
                }
            }
            Some(JobPayload::Failed(failed)) => {
                let summary = failed
                    .error
                    .as_ref()
                    .map(|err| format!("{} ({})", err.message, err.code))
                    .unwrap_or_else(|| "failed".into());
                Self {
                    at_unix_millis,
                    kind: "failed".into(),
                    summary,
                    data: None,
                }
            }
            None => Self {
                at_unix_millis,
                kind: "unknown".into(),
                summary: "missing payload".into(),
                data: None,
            },
        }
    }
}

fn job_event_lines(event: &JobEvent) -> Vec<String> {
    let export = LogExportEvent::from_proto(event);
    let mut lines = Vec::new();
    lines.push(format!(
        "{} {}: {}\n",
        export.at_unix_millis, export.kind, export.summary
    ));
    if let Some(data) = export.data {
        if data.ends_with('\n') {
            lines.push(data);
        } else {
            lines.push(format!("{data}\n"));
        }
    }
    lines
}

fn format_job_row(job: &Job) -> String {
    let job_id = job.job_id.as_ref().map(|id| id.value.as_str()).unwrap_or("-");
    let state = JobState::try_from(job.state).unwrap_or(JobState::Unspecified);
    let created = job.created_at.as_ref().map(|ts| ts.unix_millis).unwrap_or_default();
    let finished = job.finished_at.as_ref().map(|ts| ts.unix_millis).unwrap_or_default();
    format!(
        "- {} type={} state={:?} created={} finished={} display={}\n",
        job_id,
        job.job_type,
        state,
        created,
        finished,
        job.display_name
    )
}

fn parse_artifact_types(raw: &str) -> Vec<ArtifactType> {
    parse_list_tokens(raw)
        .into_iter()
        .filter_map(|token| match token.to_ascii_lowercase().as_str() {
            "apk" => Some(ArtifactType::Apk),
            "aab" | "bundle" => Some(ArtifactType::Aab),
            "aar" => Some(ArtifactType::Aar),
            "mapping" | "mapping.txt" => Some(ArtifactType::Mapping),
            "test" | "tests" | "test_result" | "test-results" => Some(ArtifactType::TestResult),
            _ => None,
        })
        .collect()
}

fn artifact_type_label(artifact_type: ArtifactType) -> &'static str {
    match artifact_type {
        ArtifactType::Apk => "apk",
        ArtifactType::Aab => "aab",
        ArtifactType::Aar => "aar",
        ArtifactType::Mapping => "mapping",
        ArtifactType::TestResult => "test_result",
        ArtifactType::Unspecified => "unspecified",
    }
}

fn metadata_value<'a>(items: &'a [KeyValue], key: &str) -> Option<&'a str> {
    items
        .iter()
        .find(|item| item.key == key)
        .map(|item| item.value.as_str())
}

fn format_artifact_line(artifact: &Artifact) -> String {
    let kind = ArtifactType::try_from(artifact.r#type).unwrap_or(ArtifactType::Unspecified);
    let module = metadata_value(&artifact.metadata, "module").unwrap_or("-");
    let variant = metadata_value(&artifact.metadata, "variant").unwrap_or("-");
    let task = metadata_value(&artifact.metadata, "task").unwrap_or("-");
    format!(
        "- [{}] {} module={} variant={} task={} size={} path={}\n",
        artifact_type_label(kind),
        artifact.name,
        module,
        variant,
        task,
        artifact.size_bytes,
        artifact.path
    )
}

async fn handle_command(cmd: UiCommand, worker_state: &mut AppState, ui: mpsc::Sender<AppEvent>) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        UiCommand::HomeStartJob {
            cfg,
            job_type,
            params_raw,
            project_id,
            target_id,
            toolchain_set_id,
            correlation_id,
        } => {
            ui.send(AppEvent::HomeResetStatus).ok();
            ui.send(AppEvent::Log { page: "home", line: format!("Connecting to JobService at {}\n", cfg.job_addr) }).ok();

            let job_type = job_type.trim().to_string();
            if job_type.is_empty() {
                ui.send(AppEvent::Log { page: "home", line: "job_type is required\n".into() }).ok();
                return Ok(());
            }

            let mut client = JobServiceClient::new(connect(&cfg.job_addr).await?);
            let resp = client.start_job(StartJobRequest {
                job_type: job_type.clone(),
                params: parse_kv_lines(&params_raw),
                project_id: to_optional_id(&project_id),
                target_id: to_optional_id(&target_id),
                toolchain_set_id: to_optional_id(&toolchain_set_id),
                correlation_id: correlation_id.trim().to_string(),
                run_id: run_id_from_optional(&correlation_id),
            }).await?.into_inner();

            let job_id = resp.job.and_then(|r| r.job_id).map(|i| i.value).unwrap_or_default();
            if job_id.is_empty() {
                ui.send(AppEvent::Log { page: "home", line: "StartJob returned empty job_id\n".into() }).ok();
                return Ok(());
            }
            worker_state.current_job_id = Some(job_id.clone());
            ui.send(AppEvent::SetCurrentJob { job_id: Some(job_id.clone()) }).ok();
            ui.send(AppEvent::Log { page: "home", line: format!("Started job: {job_id} ({job_type})\n") }).ok();
            ui.send(AppEvent::HomeState { state: "Queued".into() }).ok();

            stream_job_events_home(&cfg.job_addr, &job_id, ui.clone()).await?;
        }

        UiCommand::HomeWatchJob { cfg, job_id } => {
            let job_id = job_id.trim().to_string();
            if job_id.is_empty() {
                ui.send(AppEvent::Log { page: "home", line: "job_id is required to watch\n".into() }).ok();
                return Ok(());
            }
            ui.send(AppEvent::HomeResetStatus).ok();
            worker_state.current_job_id = Some(job_id.clone());
            ui.send(AppEvent::SetCurrentJob { job_id: Some(job_id.clone()) }).ok();
            ui.send(AppEvent::Log { page: "home", line: format!("Watching job: {job_id}\n") }).ok();

            stream_job_events_home(&cfg.job_addr, &job_id, ui.clone()).await?;
        }

        UiCommand::HomeCancelCurrent { cfg } => {
            if let Some(job_id) = worker_state.current_job_id.clone() {
                ui.send(AppEvent::Log { page: "home", line: format!("Cancelling job: {job_id}\n") }).ok();
                let mut client = JobServiceClient::new(connect(&cfg.job_addr).await?);
                let resp = client.cancel_job(CancelJobRequest { job_id: Some(Id { value: job_id.clone() }) }).await?.into_inner();
                ui.send(AppEvent::Log { page: "home", line: format!("Cancel accepted: {}\n", resp.accepted) }).ok();
            }
        }

        UiCommand::JobsList {
            cfg,
            job_types,
            states,
            created_after,
            created_before,
            finished_after,
            finished_before,
            correlation_id,
            page_size,
            page_token,
        } => {
            ui.send(AppEvent::Log { page: "jobs", line: format!("Listing jobs via {}\n", cfg.job_addr) }).ok();
            let (state_filters, unknown_states) = parse_job_states(&states);
            if !unknown_states.is_empty() {
                ui.send(AppEvent::Log { page: "jobs", line: format!("Unknown states: {}\n", unknown_states.join(", ")) }).ok();
            }
            let created_after = match parse_optional_millis(&created_after) {
                Ok(value) => value,
                Err(err) => {
                    ui.send(AppEvent::Log { page: "jobs", line: format!("{err}\n") }).ok();
                    return Ok(());
                }
            };
            let created_before = match parse_optional_millis(&created_before) {
                Ok(value) => value,
                Err(err) => {
                    ui.send(AppEvent::Log { page: "jobs", line: format!("{err}\n") }).ok();
                    return Ok(());
                }
            };
            let finished_after = match parse_optional_millis(&finished_after) {
                Ok(value) => value,
                Err(err) => {
                    ui.send(AppEvent::Log { page: "jobs", line: format!("{err}\n") }).ok();
                    return Ok(());
                }
            };
            let finished_before = match parse_optional_millis(&finished_before) {
                Ok(value) => value,
                Err(err) => {
                    ui.send(AppEvent::Log { page: "jobs", line: format!("{err}\n") }).ok();
                    return Ok(());
                }
            };

            let filter = JobFilter {
                job_types: parse_list_tokens(&job_types),
                states: state_filters,
                created_after: created_after.map(|ms| Timestamp { unix_millis: ms }),
                created_before: created_before.map(|ms| Timestamp { unix_millis: ms }),
                finished_after: finished_after.map(|ms| Timestamp { unix_millis: ms }),
                finished_before: finished_before.map(|ms| Timestamp { unix_millis: ms }),
                correlation_id: correlation_id.trim().to_string(),
                run_id: None,
            };

            let mut client = JobServiceClient::new(connect(&cfg.job_addr).await?);
            let resp = client
                .list_jobs(ListJobsRequest {
                    page: Some(Pagination { page_size: page_size.max(1), page_token }),
                    filter: Some(filter),
                })
                .await?
                .into_inner();

            if resp.jobs.is_empty() {
                ui.send(AppEvent::Log { page: "jobs", line: "No jobs found.\n".into() }).ok();
            } else {
                ui.send(AppEvent::Log { page: "jobs", line: format!("Jobs ({})\n", resp.jobs.len()) }).ok();
                for job in &resp.jobs {
                    ui.send(AppEvent::Log { page: "jobs", line: format_job_row(job) }).ok();
                }
            }
            if let Some(page_info) = resp.page_info {
                if !page_info.next_page_token.is_empty() {
                    ui.send(AppEvent::Log { page: "jobs", line: format!("next_page_token={}\n", page_info.next_page_token) }).ok();
                }
            }
        }

        UiCommand::JobsHistory {
            cfg,
            job_id,
            kinds,
            after,
            before,
            page_size,
            page_token,
        } => {
            let job_id = job_id.trim().to_string();
            if job_id.is_empty() {
                ui.send(AppEvent::Log { page: "jobs", line: "job_id is required for history\n".into() }).ok();
                return Ok(());
            }
            ui.send(AppEvent::Log { page: "jobs", line: format!("Listing job history for {job_id} via {}\n", cfg.job_addr) }).ok();
            let (kind_filters, unknown_kinds) = parse_job_event_kinds(&kinds);
            if !unknown_kinds.is_empty() {
                ui.send(AppEvent::Log { page: "jobs", line: format!("Unknown kinds: {}\n", unknown_kinds.join(", ")) }).ok();
            }
            let after = match parse_optional_millis(&after) {
                Ok(value) => value,
                Err(err) => {
                    ui.send(AppEvent::Log { page: "jobs", line: format!("{err}\n") }).ok();
                    return Ok(());
                }
            };
            let before = match parse_optional_millis(&before) {
                Ok(value) => value,
                Err(err) => {
                    ui.send(AppEvent::Log { page: "jobs", line: format!("{err}\n") }).ok();
                    return Ok(());
                }
            };

            let filter = JobHistoryFilter {
                kinds: kind_filters,
                after: after.map(|ms| Timestamp { unix_millis: ms }),
                before: before.map(|ms| Timestamp { unix_millis: ms }),
            };

            let mut client = JobServiceClient::new(connect(&cfg.job_addr).await?);
            let resp = client
                .list_job_history(ListJobHistoryRequest {
                    job_id: Some(Id { value: job_id.clone() }),
                    page: Some(Pagination { page_size: page_size.max(1), page_token }),
                    filter: Some(filter),
                })
                .await?
                .into_inner();

            if resp.events.is_empty() {
                ui.send(AppEvent::Log { page: "jobs", line: "No events found.\n".into() }).ok();
            } else {
                ui.send(AppEvent::Log { page: "jobs", line: format!("Events ({})\n", resp.events.len()) }).ok();
                for event in &resp.events {
                    for line in job_event_lines(event) {
                        ui.send(AppEvent::Log { page: "jobs", line }).ok();
                    }
                }
            }
            if let Some(page_info) = resp.page_info {
                if !page_info.next_page_token.is_empty() {
                    ui.send(AppEvent::Log { page: "jobs", line: format!("next_page_token={}\n", page_info.next_page_token) }).ok();
                }
            }
        }

        UiCommand::JobsExportLogs { cfg, job_id, output_path } => {
            let job_id = job_id.trim().to_string();
            if job_id.is_empty() {
                ui.send(AppEvent::Log { page: "jobs", line: "job_id is required for export\n".into() }).ok();
                return Ok(());
            }
            ui.send(AppEvent::Log { page: "jobs", line: format!("Exporting logs for {job_id}\n") }).ok();

            let path = if output_path.trim().is_empty() {
                default_export_path("ui-job-export", &job_id)
            } else {
                PathBuf::from(output_path)
            };

            let mut client = JobServiceClient::new(connect(&cfg.job_addr).await?);
            let job = match client.get_job(GetJobRequest { job_id: Some(Id { value: job_id.clone() }) }).await {
                Ok(resp) => resp.into_inner().job,
                Err(err) => {
                    ui.send(AppEvent::Log { page: "jobs", line: format!("GetJob failed: {err}\n") }).ok();
                    None
                }
            };
            let events = collect_job_history(&mut client, &job_id).await?;
            let export = UiLogExport {
                exported_at_unix_millis: now_millis(),
                job_id: job_id.clone(),
                config: cfg.clone(),
                job: job.as_ref().map(JobSummary::from_proto),
                events: events.iter().map(LogExportEvent::from_proto).collect(),
            };
            write_json_atomic(&path, &export)?;
            ui.send(AppEvent::Log { page: "jobs", line: format!("Exported logs to {}\n", path.display()) }).ok();
        }

        UiCommand::ToolchainListProviders { cfg } => {
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Connecting to ToolchainService at {}\n", cfg.toolchain_addr) }).ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client.list_providers(ListProvidersRequest {}).await?.into_inner();

            ui.send(AppEvent::Log { page: "toolchains", line: "Providers:\n".into() }).ok();
            for p in resp.providers {
                ui.send(AppEvent::Log { page: "toolchains", line: format!("- {} (kind={})\n", p.name, p.kind) }).ok();
            }
        }

        UiCommand::ToolchainListAvailable { cfg, provider_id } => {
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Listing available for {provider_id} via {}\n", cfg.toolchain_addr) }).ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let provider_id_value = provider_id.clone();
            let resp = client.list_available(ListAvailableRequest {
                provider_id: Some(Id { value: provider_id.clone() }),
                page: None,
            }).await?.into_inner();
            let versions = resp
                .items
                .iter()
                .filter_map(|item| item.version.as_ref().map(|v| v.version.clone()))
                .collect::<Vec<_>>();
            ui.send(AppEvent::ToolchainAvailable { provider_id: provider_id_value, versions }).ok();

            if resp.items.is_empty() {
                ui.send(AppEvent::Log { page: "toolchains", line: "No available toolchains.\n".into() }).ok();
            } else {
                ui.send(AppEvent::Log { page: "toolchains", line: "Available:\n".into() }).ok();
                for item in resp.items {
                    let name = item.provider.as_ref().map(|p| p.name.as_str()).unwrap_or("unknown");
                    let version = item.version.as_ref().map(|v| v.version.as_str()).unwrap_or("unknown");
                    let url = item.artifact.as_ref().map(|a| a.url.as_str()).unwrap_or("");
                    let sha = item.artifact.as_ref().map(|a| a.sha256.as_str()).unwrap_or("");
                    let size = item.artifact.as_ref().map(|a| a.size_bytes).unwrap_or(0);
                    ui.send(AppEvent::Log { page: "toolchains", line: format!("- {name} {version}\n  url={url}\n  sha256={sha}\n  size_bytes={size}\n") }).ok();
                }
            }
        }

        UiCommand::ToolchainInstall {
            cfg,
            provider_id,
            version,
            verify,
            job_id,
            correlation_id,
        } => {
            let version = version.trim().to_string();
            if version.is_empty() {
                ui.send(AppEvent::Log { page: "toolchains", line: "Toolchain install requires a version.\n".into() }).ok();
                return Ok(());
            }
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Installing {provider_id} {version} (verify={verify})\n") }).ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client.install_toolchain(InstallToolchainRequest {
                provider_id: Some(Id { value: provider_id }),
                version,
                install_root: "".into(),
                verify_hash: verify,
                job_id: job_id
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| Id { value: value.clone() }),
                correlation_id: correlation_id.trim().to_string(),
                run_id: run_id_from_optional(&correlation_id),
            }).await?.into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Install job queued: {job_id}\n") }).ok();
            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                tokio::spawn(async move {
                    if let Err(err) = stream_job_events(job_addr, job_id.clone(), "toolchains", ui_stream).await {
                        let _ = ui_err.send(AppEvent::Log { page: "toolchains", line: format!("job stream error ({job_id}): {err}\n") });
                    }
                });
            }
        }

        UiCommand::ToolchainUpdate {
            cfg,
            toolchain_id,
            version,
            verify,
            remove_cached,
            job_id,
            correlation_id,
        } => {
            let toolchain_id = toolchain_id.trim().to_string();
            let version = version.trim().to_string();
            if toolchain_id.is_empty() || version.is_empty() {
                ui.send(AppEvent::Log { page: "toolchains", line: "Toolchain update requires toolchain id and version.\n".into() }).ok();
                return Ok(());
            }
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Updating {toolchain_id} to {version} (verify={verify})\n") }).ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client.update_toolchain(UpdateToolchainRequest {
                toolchain_id: Some(Id { value: toolchain_id }),
                version,
                verify_hash: verify,
                remove_cached_artifact: remove_cached,
                job_id: job_id
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| Id { value: value.clone() }),
                correlation_id: correlation_id.trim().to_string(),
                run_id: run_id_from_optional(&correlation_id),
            }).await?.into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Update job queued: {job_id}\n") }).ok();
            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                tokio::spawn(async move {
                    if let Err(err) = stream_job_events(job_addr, job_id.clone(), "toolchains", ui_stream).await {
                        let _ = ui_err.send(AppEvent::Log { page: "toolchains", line: format!("job stream error ({job_id}): {err}\n") });
                    }
                });
            }
        }

        UiCommand::ToolchainUninstall {
            cfg,
            toolchain_id,
            remove_cached,
            force,
            job_id,
            correlation_id,
        } => {
            let toolchain_id = toolchain_id.trim().to_string();
            if toolchain_id.is_empty() {
                ui.send(AppEvent::Log { page: "toolchains", line: "Toolchain uninstall requires toolchain id.\n".into() }).ok();
                return Ok(());
            }
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Uninstalling {toolchain_id} (force={force})\n") }).ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client.uninstall_toolchain(UninstallToolchainRequest {
                toolchain_id: Some(Id { value: toolchain_id }),
                remove_cached_artifact: remove_cached,
                force,
                job_id: job_id
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| Id { value: value.clone() }),
                correlation_id: correlation_id.trim().to_string(),
                run_id: run_id_from_optional(&correlation_id),
            }).await?.into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Uninstall job queued: {job_id}\n") }).ok();
            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                tokio::spawn(async move {
                    if let Err(err) = stream_job_events(job_addr, job_id.clone(), "toolchains", ui_stream).await {
                        let _ = ui_err.send(AppEvent::Log { page: "toolchains", line: format!("job stream error ({job_id}): {err}\n") });
                    }
                });
            }
        }

        UiCommand::ToolchainCleanupCache {
            cfg,
            dry_run,
            remove_all,
            job_id,
            correlation_id,
        } => {
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Cleaning cache via {} (dry_run={dry_run}, remove_all={remove_all})\n", cfg.toolchain_addr) }).ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client.cleanup_toolchain_cache(CleanupToolchainCacheRequest {
                dry_run,
                remove_all,
                job_id: job_id
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| Id { value: value.clone() }),
                correlation_id: correlation_id.trim().to_string(),
                run_id: run_id_from_optional(&correlation_id),
            }).await?.into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Cleanup job queued: {job_id}\n") }).ok();
            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                tokio::spawn(async move {
                    if let Err(err) = stream_job_events(job_addr, job_id.clone(), "toolchains", ui_stream).await {
                        let _ = ui_err.send(AppEvent::Log { page: "toolchains", line: format!("job stream error ({job_id}): {err}\n") });
                    }
                });
            }
        }

        UiCommand::ToolchainListInstalled { cfg, kind } => {
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Listing installed toolchains via {}\n", cfg.toolchain_addr) }).ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client.list_installed(ListInstalledRequest { kind: kind as i32 }).await?.into_inner();

            if resp.items.is_empty() {
                ui.send(AppEvent::Log { page: "toolchains", line: "No installed toolchains.\n".into() }).ok();
            } else {
                ui.send(AppEvent::Log { page: "toolchains", line: "Installed:\n".into() }).ok();
                for item in resp.items {
                    let id = item.toolchain_id.as_ref().map(|i| i.value.as_str()).unwrap_or("");
                    let name = item.provider.as_ref().map(|p| p.name.as_str()).unwrap_or("unknown");
                    let version = item.version.as_ref().map(|v| v.version.as_str()).unwrap_or("unknown");
                    ui.send(AppEvent::Log { page: "toolchains", line: format!("- {id} {name} {version} verified={}\n  path={}\n", item.verified, item.install_path) }).ok();
                }
            }
        }

        UiCommand::ToolchainListSets { cfg } => {
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Listing toolchain sets via {}\n", cfg.toolchain_addr) }).ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client.list_toolchain_sets(ListToolchainSetsRequest { page: None }).await?.into_inner();

            if resp.sets.is_empty() {
                ui.send(AppEvent::Log { page: "toolchains", line: "No toolchain sets found.\n".into() }).ok();
            } else {
                ui.send(AppEvent::Log { page: "toolchains", line: "Toolchain sets:\n".into() }).ok();
                for set in resp.sets {
                    let set_id = set.toolchain_set_id.as_ref().map(|id| id.value.as_str()).unwrap_or("");
                    let sdk = set.sdk_toolchain_id.as_ref().map(|id| id.value.as_str()).unwrap_or("");
                    let ndk = set.ndk_toolchain_id.as_ref().map(|id| id.value.as_str()).unwrap_or("");
                    ui.send(AppEvent::Log { page: "toolchains", line: format!("- {} ({})\n  sdk={}\n  ndk={}\n", set.display_name, set_id, sdk, ndk) }).ok();
                }
            }

            if let Some(page_info) = resp.page_info {
                if !page_info.next_page_token.trim().is_empty() {
                    ui.send(AppEvent::Log { page: "toolchains", line: format!("next_page_token={}\n", page_info.next_page_token) }).ok();
                }
            }
        }

        UiCommand::ToolchainVerifyInstalled {
            cfg,
            job_id,
            correlation_id,
        } => {
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Verifying installed toolchains via {}\n", cfg.toolchain_addr) }).ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client.list_installed(ListInstalledRequest { kind: ToolchainKind::Unspecified as i32 }).await?.into_inner();

            if resp.items.is_empty() {
                ui.send(AppEvent::Log { page: "toolchains", line: "No installed toolchains to verify.\n".into() }).ok();
            } else {
                for item in resp.items {
                    let Some(id) = item.toolchain_id.as_ref().map(|i| i.value.clone()) else {
                        ui.send(AppEvent::Log { page: "toolchains", line: "Skipping toolchain with missing id.\n".into() }).ok();
                        continue;
                    };
                    let verify = client.verify_toolchain(VerifyToolchainRequest {
                        toolchain_id: Some(Id { value: id.clone() }),
                        job_id: job_id
                            .as_ref()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| Id { value: value.clone() }),
                        correlation_id: correlation_id.trim().to_string(),
                        run_id: run_id_from_optional(&correlation_id),
                    }).await?.into_inner();

                    if verify.verified {
                        ui.send(AppEvent::Log { page: "toolchains", line: format!("- {id} verified OK\n") }).ok();
                    } else if let Some(err) = verify.error {
                        ui.send(AppEvent::Log { page: "toolchains", line: format!("- {id} verify failed: {} ({})\n", err.message, err.code) }).ok();
                    } else {
                        ui.send(AppEvent::Log { page: "toolchains", line: format!("- {id} verify failed (no details)\n") }).ok();
                    }

                    if let Some(job_id) = verify.job_id.map(|i| i.value) {
                        if !job_id.is_empty() {
                            let job_addr = cfg.job_addr.clone();
                            let ui_stream = ui.clone();
                            let ui_err = ui.clone();
                            tokio::spawn(async move {
                                if let Err(err) = stream_job_events(job_addr, job_id.clone(), "toolchains", ui_stream).await {
                                    let _ = ui_err.send(AppEvent::Log { page: "toolchains", line: format!("job stream error ({job_id}): {err}\n") });
                                }
                            });
                        }
                    }
                }
            }
        }

        UiCommand::ToolchainCreateSet {
            cfg,
            sdk_toolchain_id,
            ndk_toolchain_id,
            display_name,
        } => {
            let sdk_id = sdk_toolchain_id
                .as_ref()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string());
            let ndk_id = ndk_toolchain_id
                .as_ref()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string());
            if sdk_id.is_none() && ndk_id.is_none() {
                ui.send(AppEvent::Log { page: "toolchains", line: "Provide an SDK and/or NDK toolchain id.\n".into() }).ok();
                return Ok(());
            }

            ui.send(AppEvent::Log { page: "toolchains", line: format!("Creating toolchain set via {}\n", cfg.toolchain_addr) }).ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client.create_toolchain_set(CreateToolchainSetRequest {
                sdk_toolchain_id: sdk_id.map(|value| Id { value }),
                ndk_toolchain_id: ndk_id.map(|value| Id { value }),
                display_name: display_name.trim().to_string(),
            }).await?;

            if let Some(set) = resp.into_inner().set {
                let set_id = set.toolchain_set_id.as_ref().map(|id| id.value.as_str()).unwrap_or("");
                let sdk = set.sdk_toolchain_id.as_ref().map(|id| id.value.as_str()).unwrap_or("");
                let ndk = set.ndk_toolchain_id.as_ref().map(|id| id.value.as_str()).unwrap_or("");
                ui.send(AppEvent::Log { page: "toolchains", line: format!("Created set {set_id}\n  display_name={}\n  sdk={}\n  ndk={}\n", set.display_name, sdk, ndk) }).ok();
            } else {
                ui.send(AppEvent::Log { page: "toolchains", line: "Create toolchain set returned no set.\n".into() }).ok();
            }
        }

        UiCommand::ToolchainSetActive { cfg, toolchain_set_id } => {
            let set_id = toolchain_set_id.trim().to_string();
            if set_id.is_empty() {
                ui.send(AppEvent::Log { page: "toolchains", line: "Set active requires a toolchain set id.\n".into() }).ok();
                return Ok(());
            }
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Setting active toolchain set via {}\n", cfg.toolchain_addr) }).ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client.set_active_toolchain_set(SetActiveToolchainSetRequest {
                toolchain_set_id: Some(Id { value: set_id.clone() }),
            }).await?.into_inner();

            if resp.ok {
                ui.send(AppEvent::Log { page: "toolchains", line: format!("Active toolchain set set to {set_id}\n") }).ok();
            } else {
                ui.send(AppEvent::Log { page: "toolchains", line: "Failed to set active toolchain set.\n".into() }).ok();
            }
        }

        UiCommand::ToolchainGetActive { cfg } => {
            ui.send(AppEvent::Log { page: "toolchains", line: format!("Fetching active toolchain set via {}\n", cfg.toolchain_addr) }).ok();
            let mut client = ToolchainServiceClient::new(connect(&cfg.toolchain_addr).await?);
            let resp = client.get_active_toolchain_set(GetActiveToolchainSetRequest {}).await?.into_inner();
            if let Some(set) = resp.set {
                let set_id = set.toolchain_set_id.as_ref().map(|id| id.value.as_str()).unwrap_or("");
                let sdk = set.sdk_toolchain_id.as_ref().map(|id| id.value.as_str()).unwrap_or("");
                let ndk = set.ndk_toolchain_id.as_ref().map(|id| id.value.as_str()).unwrap_or("");
                ui.send(AppEvent::Log { page: "toolchains", line: format!("Active set {set_id}\n  display_name={}\n  sdk={}\n  ndk={}\n", set.display_name, sdk, ndk) }).ok();
            } else {
                ui.send(AppEvent::Log { page: "toolchains", line: "No active toolchain set configured.\n".into() }).ok();
            }
        }

        UiCommand::ProjectListTemplates { cfg } => {
            ui.send(AppEvent::Log { page: "projects", line: format!("Connecting to ProjectService at {}\n", cfg.project_addr) }).ok();
            let mut client = ProjectServiceClient::new(connect(&cfg.project_addr).await?);
            let resp = client.list_templates(ListTemplatesRequest {}).await?.into_inner();

            ui.send(AppEvent::Log { page: "projects", line: "Templates:\n".into() }).ok();
            let mut options = Vec::new();
            for t in resp.templates {
                let template_id = t.template_id.as_ref().map(|i| i.value.clone()).unwrap_or_default();
                if template_id.is_empty() {
                    ui.send(AppEvent::Log { page: "projects", line: format!("- {} (missing template_id)\n", t.name) }).ok();
                    continue;
                }

                let defaults = if t.defaults.is_empty() {
                    "defaults: none".to_string()
                } else {
                    let pairs = t
                        .defaults
                        .iter()
                        .map(|kv| format!("{}={}", kv.key, kv.value))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("defaults: {pairs}")
                };

                ui.send(AppEvent::Log { page: "projects", line: format!("- {} ({})\n  {}\n  {}\n", t.name, template_id, t.description, defaults) }).ok();
                options.push(ProjectTemplateOption {
                    id: template_id,
                    name: t.name,
                });
            }
            ui.send(AppEvent::ProjectTemplates { templates: options }).ok();
        }

        UiCommand::ProjectListRecent { cfg } => {
            ui.send(AppEvent::Log { page: "projects", line: format!("Listing recent projects via {}\n", cfg.project_addr) }).ok();
            let mut client = ProjectServiceClient::new(connect(&cfg.project_addr).await?);
            let resp = client.list_recent_projects(ListRecentProjectsRequest { page: None }).await?.into_inner();

            if resp.projects.is_empty() {
                ui.send(AppEvent::Log { page: "projects", line: "No recent projects.\n".into() }).ok();
            } else {
                ui.send(AppEvent::Log { page: "projects", line: "Recent projects:\n".into() }).ok();
                for project in resp.projects {
                    let id = project.project_id.as_ref().map(|i| i.value.as_str()).unwrap_or("");
                    ui.send(AppEvent::Log { page: "projects", line: format!("- {} ({})\n  path={}\n", project.name, id, project.path) }).ok();
                }
            }

            if let Some(page_info) = resp.page_info {
                if !page_info.next_page_token.trim().is_empty() {
                    ui.send(AppEvent::Log { page: "projects", line: format!("next_page_token={}\n", page_info.next_page_token) }).ok();
                }
            }
        }

        UiCommand::ProjectLoadDefaults { cfg } => {
            ui.send(AppEvent::Log { page: "projects", line: format!("Loading toolchain sets via {}\n", cfg.toolchain_addr) }).ok();
            match connect(&cfg.toolchain_addr).await {
                Ok(channel) => {
                    let mut client = ToolchainServiceClient::new(channel);
                    match client
                        .list_toolchain_sets(ListToolchainSetsRequest { page: None })
                        .await
                    {
                        Ok(resp) => {
                            let mut sets = Vec::new();
                            for set in resp.into_inner().sets {
                                let set_id = set
                                    .toolchain_set_id
                                    .as_ref()
                                    .map(|id| id.value.as_str())
                                    .unwrap_or("");
                                if set_id.is_empty() {
                                    continue;
                                }
                                let name = if set.display_name.trim().is_empty() {
                                    "Toolchain Set"
                                } else {
                                    set.display_name.as_str()
                                };
                                let sdk = set
                                    .sdk_toolchain_id
                                    .as_ref()
                                    .map(|id| id.value.as_str())
                                    .unwrap_or("");
                                let ndk = set
                                    .ndk_toolchain_id
                                    .as_ref()
                                    .map(|id| id.value.as_str())
                                    .unwrap_or("");
                                let label = format!("{name} ({set_id}) sdk={sdk} ndk={ndk}");
                                sets.push(ToolchainSetOption {
                                    id: set_id.to_string(),
                                    label,
                                });
                            }
                            ui.send(AppEvent::ProjectToolchainSets { sets }).ok();
                        }
                        Err(err) => {
                            ui.send(AppEvent::Log { page: "projects", line: format!("List toolchain sets failed: {err}\n") }).ok();
                        }
                    }
                }
                Err(err) => {
                    ui.send(AppEvent::Log { page: "projects", line: format!("ToolchainService connection failed: {err}\n") }).ok();
                }
            }

            ui.send(AppEvent::Log { page: "projects", line: format!("Loading targets via {}\n", cfg.targets_addr) }).ok();
            match connect(&cfg.targets_addr).await {
                Ok(channel) => {
                    let mut client = TargetServiceClient::new(channel);
                    match client
                        .list_targets(ListTargetsRequest { include_offline: true })
                        .await
                    {
                        Ok(resp) => {
                            let mut targets = Vec::new();
                            for target in resp.into_inner().targets {
                                let target_id = target
                                    .target_id
                                    .as_ref()
                                    .map(|id| id.value.as_str())
                                    .unwrap_or("");
                                if target_id.is_empty() {
                                    continue;
                                }
                                let label = format!(
                                    "{} [{}] {} ({})",
                                    target.display_name, target.provider, target.state, target_id
                                );
                                targets.push(TargetOption {
                                    id: target_id.to_string(),
                                    label,
                                });
                            }
                            ui.send(AppEvent::ProjectTargets { targets }).ok();
                        }
                        Err(err) => {
                            ui.send(AppEvent::Log { page: "projects", line: format!("List targets failed: {err}\n") }).ok();
                        }
                    }
                }
                Err(err) => {
                    ui.send(AppEvent::Log { page: "projects", line: format!("TargetService connection failed: {err}\n") }).ok();
                }
            }
        }

        UiCommand::ProjectCreate {
            cfg,
            name,
            path,
            template_id,
            job_id,
            correlation_id,
        } => {
            let name = name.trim().to_string();
            let path = path.trim().to_string();
            let template_id = template_id.trim().to_string();

            if name.is_empty() {
                ui.send(AppEvent::Log { page: "projects", line: "Create project requires a name.\n".into() }).ok();
                return Ok(());
            }
            if path.is_empty() {
                ui.send(AppEvent::Log { page: "projects", line: "Create project requires a path.\n".into() }).ok();
                return Ok(());
            }
            if template_id.is_empty() {
                ui.send(AppEvent::Log { page: "projects", line: "Select a template before creating the project.\n".into() }).ok();
                return Ok(());
            }

            ui.send(AppEvent::Log { page: "projects", line: format!("Creating project via {}\n", cfg.project_addr) }).ok();
            let mut client = ProjectServiceClient::new(connect(&cfg.project_addr).await?);
            let resp = match client.create_project(CreateProjectRequest {
                name: name.clone(),
                path: path.clone(),
                template_id: Some(Id { value: template_id.clone() }),
                params: vec![],
                toolchain_set_id: None,
                job_id: job_id
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| Id { value: value.clone() }),
                correlation_id: correlation_id.trim().to_string(),
                run_id: run_id_from_optional(&correlation_id),
            }).await {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log { page: "projects", line: format!("Create project failed: {err}\n") }).ok();
                    return Ok(());
                }
            };

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            let project_id = resp.project_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log { page: "projects", line: format!("Create queued: project_id={project_id} job_id={job_id}\n") }).ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                tokio::spawn(async move {
                    if let Err(err) = stream_job_events(job_addr, job_id.clone(), "projects", ui_stream).await {
                        let _ = ui_err.send(AppEvent::Log { page: "projects", line: format!("job stream error ({job_id}): {err}\n") });
                    }
                });
            }
        }

        UiCommand::ProjectOpen { cfg, path } => {
            let path = path.trim().to_string();
            if path.is_empty() {
                ui.send(AppEvent::Log { page: "projects", line: "Open project requires a path.\n".into() }).ok();
                return Ok(());
            }

            ui.send(AppEvent::Log { page: "projects", line: format!("Opening project via {}\n", cfg.project_addr) }).ok();
            let mut client = ProjectServiceClient::new(connect(&cfg.project_addr).await?);
            let resp = match client.open_project(OpenProjectRequest { path: path.clone() }).await {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log { page: "projects", line: format!("Open project failed: {err}\n") }).ok();
                    return Ok(());
                }
            };

            if let Some(project) = resp.project {
                let id = project.project_id.as_ref().map(|i| i.value.as_str()).unwrap_or("");
                ui.send(AppEvent::Log { page: "projects", line: format!("Opened: {} ({})\n  path={}\n", project.name, id, project.path) }).ok();
            } else {
                ui.send(AppEvent::Log { page: "projects", line: "Open project returned no project.\n".into() }).ok();
            }
        }

        UiCommand::ProjectSetConfig {
            cfg,
            project_id,
            toolchain_set_id,
            default_target_id,
        } => {
            let project_id = project_id.trim().to_string();
            if project_id.is_empty() {
                ui.send(AppEvent::Log { page: "projects", line: "Set project config requires a project id.\n".into() }).ok();
                return Ok(());
            }
            let toolchain_set_id = toolchain_set_id
                .as_ref()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string());
            let default_target_id = default_target_id
                .as_ref()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string());

            if toolchain_set_id.is_none() && default_target_id.is_none() {
                ui.send(AppEvent::Log { page: "projects", line: "Provide a toolchain set id and/or default target id.\n".into() }).ok();
                return Ok(());
            }

            ui.send(AppEvent::Log { page: "projects", line: format!("Updating project config via {}\n", cfg.project_addr) }).ok();
            let mut client = ProjectServiceClient::new(connect(&cfg.project_addr).await?);
            let resp = client.set_project_config(SetProjectConfigRequest {
                project_id: Some(Id { value: project_id.clone() }),
                toolchain_set_id: toolchain_set_id.map(|value| Id { value }),
                default_target_id: default_target_id.map(|value| Id { value }),
            }).await?;

            if resp.into_inner().ok {
                ui.send(AppEvent::Log { page: "projects", line: format!("Updated config for {project_id}\n") }).ok();
            } else {
                ui.send(AppEvent::Log { page: "projects", line: "Project config update returned ok=false.\n".into() }).ok();
            }
        }

        UiCommand::ProjectUseActiveDefaults { cfg, project_id } => {
            let project_id = project_id.trim().to_string();
            if project_id.is_empty() {
                ui.send(AppEvent::Log { page: "projects", line: "Use active defaults requires a project id.\n".into() }).ok();
                return Ok(());
            }

            let mut toolchain_set_id = None;
            match connect(&cfg.toolchain_addr).await {
                Ok(channel) => {
                    let mut client = ToolchainServiceClient::new(channel);
                    match client.get_active_toolchain_set(GetActiveToolchainSetRequest {}).await {
                        Ok(resp) => {
                            toolchain_set_id = resp
                                .into_inner()
                                .set
                                .and_then(|set| set.toolchain_set_id)
                                .map(|id| id.value)
                                .filter(|value| !value.trim().is_empty());
                        }
                        Err(err) => {
                            ui.send(AppEvent::Log { page: "projects", line: format!("Active toolchain set lookup failed: {err}\n") }).ok();
                        }
                    }
                }
                Err(err) => {
                    ui.send(AppEvent::Log { page: "projects", line: format!("ToolchainService connection failed: {err}\n") }).ok();
                }
            }

            let mut default_target_id = None;
            match connect(&cfg.targets_addr).await {
                Ok(channel) => {
                    let mut client = TargetServiceClient::new(channel);
                    match client.get_default_target(GetDefaultTargetRequest {}).await {
                        Ok(resp) => {
                            default_target_id = resp
                                .into_inner()
                                .target
                                .and_then(|target| target.target_id)
                                .map(|id| id.value)
                                .filter(|value| !value.trim().is_empty());
                        }
                        Err(err) => {
                            ui.send(AppEvent::Log { page: "projects", line: format!("Default target lookup failed: {err}\n") }).ok();
                        }
                    }
                }
                Err(err) => {
                    ui.send(AppEvent::Log { page: "projects", line: format!("TargetService connection failed: {err}\n") }).ok();
                }
            }

            if toolchain_set_id.is_none() && default_target_id.is_none() {
                ui.send(AppEvent::Log { page: "projects", line: "No active toolchain set or default target found.\n".into() }).ok();
                return Ok(());
            }

            ui.send(AppEvent::Log { page: "projects", line: format!("Applying active defaults via {}\n", cfg.project_addr) }).ok();
            let mut client = ProjectServiceClient::new(connect(&cfg.project_addr).await?);
            let resp = client.set_project_config(SetProjectConfigRequest {
                project_id: Some(Id { value: project_id.clone() }),
                toolchain_set_id: toolchain_set_id.clone().map(|value| Id { value }),
                default_target_id: default_target_id.clone().map(|value| Id { value }),
            }).await?;

            if resp.into_inner().ok {
                ui.send(AppEvent::Log { page: "projects", line: format!("Applied defaults to {project_id}\n  toolchain_set_id={}\n  default_target_id={}\n", toolchain_set_id.unwrap_or_default(), default_target_id.unwrap_or_default()) }).ok();
            } else {
                ui.send(AppEvent::Log { page: "projects", line: "Project config update returned ok=false.\n".into() }).ok();
            }
        }

        UiCommand::TargetsList { cfg } => {
            ui.send(AppEvent::Log { page: "targets", line: format!("Connecting to TargetService at {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client.list_targets(ListTargetsRequest { include_offline: true }).await?.into_inner();

            ui.send(AppEvent::Log { page: "targets", line: "Targets:\n".into() }).ok();
            for t in resp.targets {
                let id = t.target_id.as_ref().map(|i| i.value.clone()).unwrap_or_default();
                ui.send(AppEvent::Log { page: "targets", line: format!("- {} [{}] {} ({})\n", t.display_name, id, t.state, t.provider) }).ok();
            }
        }

        UiCommand::TargetsSetDefault { cfg, target_id } => {
            let target_id = target_id.trim().to_string();
            if target_id.is_empty() {
                ui.send(AppEvent::Log { page: "targets", line: "Set default target requires a target id.\n".into() }).ok();
                return Ok(());
            }
            ui.send(AppEvent::Log { page: "targets", line: format!("Setting default target via {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client
                .set_default_target(SetDefaultTargetRequest {
                    target_id: Some(Id { value: target_id.clone() }),
                })
                .await?
                .into_inner();

            if resp.ok {
                ui.send(AppEvent::Log { page: "targets", line: format!("Default target set to {target_id}\n") }).ok();
            } else {
                ui.send(AppEvent::Log { page: "targets", line: format!("Failed to set default target to {target_id}\n") }).ok();
            }
        }

        UiCommand::TargetsGetDefault { cfg } => {
            ui.send(AppEvent::Log { page: "targets", line: format!("Fetching default target via {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client.get_default_target(GetDefaultTargetRequest {}).await?.into_inner();
            if let Some(target) = resp.target {
                let id = target.target_id.as_ref().map(|i| i.value.as_str()).unwrap_or("");
                ui.send(AppEvent::Log { page: "targets", line: format!("Default target: {} ({}) [{}]\n", target.display_name, id, target.state) }).ok();
            } else {
                ui.send(AppEvent::Log { page: "targets", line: "No default target configured.\n".into() }).ok();
            }
        }

        UiCommand::TargetsInstallCuttlefish {
            cfg,
            force,
            branch,
            target,
            build_id,
            job_id,
            correlation_id,
        } => {
            ui.send(AppEvent::Log { page: "targets", line: format!("Connecting to TargetService at {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client
                .install_cuttlefish(InstallCuttlefishRequest {
                    force,
                    branch: branch.trim().to_string(),
                    target: target.trim().to_string(),
                    build_id: build_id.trim().to_string(),
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id { value: value.clone() }),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await?
                .into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log { page: "targets", line: format!("Cuttlefish install job: {job_id}\n") }).ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                tokio::spawn(async move {
                    if let Err(err) = stream_job_events(job_addr, job_id.clone(), "targets", ui_stream).await {
                        let _ = ui_err.send(AppEvent::Log { page: "targets", line: format!("job stream error ({job_id}): {err}\n") });
                    }
                });
            }
        }

        UiCommand::TargetsResolveCuttlefishBuild { cfg, branch, target, build_id } => {
            ui.send(AppEvent::Log { page: "targets", line: format!("Resolving Cuttlefish build via {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client
                .resolve_cuttlefish_build(ResolveCuttlefishBuildRequest {
                    branch: branch.trim().to_string(),
                    target: target.trim().to_string(),
                    build_id: build_id.trim().to_string(),
                })
                .await?
                .into_inner();

            ui.send(AppEvent::Log {
                page: "targets",
                line: format!(
                    "Resolved build_id={} product={} (branch={}, target={})\n",
                    resp.build_id, resp.product, resp.branch, resp.target
                ),
            })
            .ok();
            ui.send(AppEvent::SetCuttlefishBuildId { build_id: resp.build_id }).ok();
        }

        UiCommand::TargetsStartCuttlefish {
            cfg,
            show_full_ui,
            job_id,
            correlation_id,
        } => {
            ui.send(AppEvent::Log { page: "targets", line: format!("Connecting to TargetService at {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client
                .start_cuttlefish(StartCuttlefishRequest {
                    show_full_ui,
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id { value: value.clone() }),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await?
                .into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log { page: "targets", line: format!("Cuttlefish start job: {job_id}\n") }).ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                tokio::spawn(async move {
                    if let Err(err) = stream_job_events(job_addr, job_id.clone(), "targets", ui_stream).await {
                        let _ = ui_err.send(AppEvent::Log { page: "targets", line: format!("job stream error ({job_id}): {err}\n") });
                    }
                });
            }
        }

        UiCommand::TargetsStopCuttlefish {
            cfg,
            job_id,
            correlation_id,
        } => {
            ui.send(AppEvent::Log { page: "targets", line: format!("Connecting to TargetService at {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client
                .stop_cuttlefish(StopCuttlefishRequest {
                    job_id: job_id
                        .as_ref()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Id { value: value.clone() }),
                    correlation_id: correlation_id.trim().to_string(),
                    run_id: run_id_from_optional(&correlation_id),
                })
                .await?
                .into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log { page: "targets", line: format!("Cuttlefish stop job: {job_id}\n") }).ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                tokio::spawn(async move {
                    if let Err(err) = stream_job_events(job_addr, job_id.clone(), "targets", ui_stream).await {
                        let _ = ui_err.send(AppEvent::Log { page: "targets", line: format!("job stream error ({job_id}): {err}\n") });
                    }
                });
            }
        }

        UiCommand::TargetsCuttlefishStatus { cfg } => {
            ui.send(AppEvent::Log { page: "targets", line: format!("Connecting to TargetService at {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client.get_cuttlefish_status(GetCuttlefishStatusRequest {}).await?.into_inner();
            ui.send(AppEvent::Log { page: "targets", line: format!("Cuttlefish state: {} (adb={})\n", resp.state, resp.adb_serial) }).ok();
            for kv in resp.details {
                ui.send(AppEvent::Log { page: "targets", line: format!("- {}: {}\n", kv.key, kv.value) }).ok();
            }
        }

        UiCommand::TargetsInstallApk {
            cfg,
            target_id,
            apk_path,
            job_id,
            correlation_id,
        } => {
            let target_id = target_id.trim().to_string();
            let apk_path = apk_path.trim().to_string();
            if target_id.is_empty() {
                ui.send(AppEvent::Log { page: "targets", line: "Install APK requires a target id.\n".into() }).ok();
                return Ok(());
            }
            if apk_path.is_empty() {
                ui.send(AppEvent::Log { page: "targets", line: "Install APK requires an apk path.\n".into() }).ok();
                return Ok(());
            }

            ui.send(AppEvent::Log { page: "targets", line: format!("Connecting to TargetService at {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = match client.install_apk(InstallApkRequest {
                target_id: Some(Id { value: target_id.clone() }),
                project_id: None,
                apk_path: apk_path.clone(),
                job_id: job_id
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| Id { value: value.clone() }),
                correlation_id: correlation_id.trim().to_string(),
                run_id: run_id_from_optional(&correlation_id),
            }).await {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log { page: "targets", line: format!("Install APK request failed: {err}\n") }).ok();
                    return Ok(());
                }
            };

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log { page: "targets", line: format!("Install APK job: {job_id}\n") }).ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                tokio::spawn(async move {
                    if let Err(err) = stream_job_events(job_addr, job_id.clone(), "targets", ui_stream).await {
                        let _ = ui_err.send(AppEvent::Log { page: "targets", line: format!("job stream error ({job_id}): {err}\n") });
                    }
                });
            }
        }

        UiCommand::TargetsLaunchApp {
            cfg,
            target_id,
            application_id,
            activity,
            job_id,
            correlation_id,
        } => {
            let target_id = target_id.trim().to_string();
            let application_id = application_id.trim().to_string();
            let activity = activity.trim().to_string();
            if target_id.is_empty() {
                ui.send(AppEvent::Log { page: "targets", line: "Launch requires a target id.\n".into() }).ok();
                return Ok(());
            }
            if application_id.is_empty() {
                ui.send(AppEvent::Log { page: "targets", line: "Launch requires an application id.\n".into() }).ok();
                return Ok(());
            }

            ui.send(AppEvent::Log { page: "targets", line: format!("Connecting to TargetService at {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = match client.launch(LaunchRequest {
                target_id: Some(Id { value: target_id.clone() }),
                application_id: application_id.clone(),
                activity,
                job_id: job_id
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| Id { value: value.clone() }),
                correlation_id: correlation_id.trim().to_string(),
                run_id: run_id_from_optional(&correlation_id),
            }).await {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log { page: "targets", line: format!("Launch request failed: {err}\n") }).ok();
                    return Ok(());
                }
            };

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log { page: "targets", line: format!("Launch job: {job_id}\n") }).ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                tokio::spawn(async move {
                    if let Err(err) = stream_job_events(job_addr, job_id.clone(), "targets", ui_stream).await {
                        let _ = ui_err.send(AppEvent::Log { page: "targets", line: format!("job stream error ({job_id}): {err}\n") });
                    }
                });
            }
        }

        UiCommand::TargetsStreamLogcat { cfg, target_id, filter } => {
            ui.send(AppEvent::Log { page: "targets", line: format!("Streaming logcat from {target_id} via {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let mut stream = client.stream_logcat(StreamLogcatRequest {
                target_id: Some(Id { value: target_id.clone() }),
                filter,
                include_history: true,
            }).await?.into_inner();

            while let Some(item) = stream.next().await {
                match item {
                    Ok(evt) => {
                        let line = String::from_utf8_lossy(&evt.line).to_string();
                        ui.send(AppEvent::Log { page: "targets", line }).ok();
                    }
                    Err(s) => { ui.send(AppEvent::Log { page: "targets", line: format!("logcat stream error: {s}\n") }).ok(); break; }
                }
            }
        }

        UiCommand::ObserveListRuns { cfg } => {
            ui.send(AppEvent::Log { page: "evidence", line: format!("Listing runs via {}\n", cfg.observe_addr) }).ok();
            let mut client = ObserveServiceClient::new(connect(&cfg.observe_addr).await?);
            let resp = client.list_runs(ListRunsRequest {
                page: Some(Pagination {
                    page_size: 25,
                    page_token: String::new(),
                }),
                filter: None,
            }).await?.into_inner();

            if resp.runs.is_empty() {
                ui.send(AppEvent::Log { page: "evidence", line: "No runs recorded.\n".into() }).ok();
            } else {
                ui.send(AppEvent::Log { page: "evidence", line: "Runs:\n".into() }).ok();
                for run in resp.runs {
                    let run_id = run.run_id.as_ref().map(|i| i.value.as_str()).unwrap_or("");
                    let started = run.started_at.as_ref().map(|t| t.unix_millis).unwrap_or(0);
                    let finished = run.finished_at.as_ref().map(|t| t.unix_millis).unwrap_or(0);
                    ui.send(AppEvent::Log {
                        page: "evidence",
                        line: format!(
                            "- {} result={} started={} finished={} jobs={}\n",
                            run_id,
                            run.result,
                            started,
                            finished,
                            run.job_ids.len()
                        ),
                    }).ok();
                }
            }

            if let Some(page_info) = resp.page_info {
                if !page_info.next_page_token.trim().is_empty() {
                    ui.send(AppEvent::Log { page: "evidence", line: format!("next_page_token={}\n", page_info.next_page_token) }).ok();
                }
            }
        }

        UiCommand::ObserveExportSupport {
            cfg,
            include_logs,
            include_config,
            include_toolchain_provenance,
            include_recent_runs,
            recent_runs_limit,
            job_id,
            correlation_id,
        } => {
            ui.send(AppEvent::Log { page: "evidence", line: format!("Connecting to ObserveService at {}\n", cfg.observe_addr) }).ok();
            let mut client = ObserveServiceClient::new(connect(&cfg.observe_addr).await?);
            let resp = client.export_support_bundle(ExportSupportBundleRequest {
                include_logs,
                include_config,
                include_toolchain_provenance,
                include_recent_runs,
                recent_runs_limit,
                job_id: job_id
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| Id { value: value.clone() }),
                project_id: None,
                target_id: None,
                toolchain_set_id: None,
                correlation_id: correlation_id.trim().to_string(),
                run_id: run_id_from_optional(&correlation_id),
            }).await?.into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page: "evidence",
                line: format!("Support bundle job: {job_id}\nOutput path: {}\n", resp.output_path),
            }).ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                tokio::spawn(async move {
                    if let Err(err) = stream_job_events(job_addr, job_id.clone(), "evidence", ui_stream).await {
                        let _ = ui_err.send(AppEvent::Log { page: "evidence", line: format!("job stream error ({job_id}): {err}\n") });
                    }
                });
            }
        }

        UiCommand::ObserveExportEvidence {
            cfg,
            run_id,
            job_id,
            correlation_id,
        } => {
            let run_id = run_id.trim().to_string();
            if run_id.is_empty() {
                ui.send(AppEvent::Log { page: "evidence", line: "Run id is required for evidence export.\n".into() }).ok();
                return Ok(());
            }
            ui.send(AppEvent::Log { page: "evidence", line: format!("Connecting to ObserveService at {}\n", cfg.observe_addr) }).ok();
            let mut client = ObserveServiceClient::new(connect(&cfg.observe_addr).await?);
            let resp = client.export_evidence_bundle(ExportEvidenceBundleRequest {
                run_id: Some(RunId { value: run_id.clone() }),
                job_id: job_id
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| Id { value: value.clone() }),
                correlation_id: correlation_id.trim().to_string(),
            }).await?.into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log {
                page: "evidence",
                line: format!("Evidence bundle job: {job_id}\nOutput path: {}\n", resp.output_path),
            }).ok();

            if !job_id.is_empty() {
                let job_addr = cfg.job_addr.clone();
                let ui_stream = ui.clone();
                let ui_err = ui.clone();
                tokio::spawn(async move {
                    if let Err(err) = stream_job_events(job_addr, job_id.clone(), "evidence", ui_stream).await {
                        let _ = ui_err.send(AppEvent::Log { page: "evidence", line: format!("job stream error ({job_id}): {err}\n") });
                    }
                });
            }
        }

        UiCommand::BuildRun {
            cfg,
            project_ref,
            variant,
            variant_name,
            module,
            tasks,
            clean_first,
            gradle_args,
            job_id,
            correlation_id,
        } => {
            if project_ref.trim().is_empty() {
                ui.send(AppEvent::Log { page: "console", line: "Project path or id is required.\n".into() }).ok();
                return Ok(());
            }

            ui.send(AppEvent::Log { page: "console", line: format!("Connecting to BuildService at {}\n", cfg.build_addr) }).ok();
            let mut client = BuildServiceClient::new(connect(&cfg.build_addr).await?);

            let resp = match client.build(BuildRequest {
                project_id: Some(Id { value: project_ref.trim().to_string() }),
                variant: variant as i32,
                clean_first,
                gradle_args,
                job_id: job_id
                    .as_ref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| Id { value: value.clone() }),
                module,
                variant_name,
                tasks,
                correlation_id: correlation_id.trim().to_string(),
                run_id: run_id_from_optional(&correlation_id),
            }).await {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log { page: "console", line: format!("Build request failed: {err}\n") }).ok();
                    return Ok(());
                }
            };

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            if job_id.is_empty() {
                ui.send(AppEvent::Log { page: "console", line: "Build returned empty job_id.\n".into() }).ok();
                return Ok(());
            }
            ui.send(AppEvent::Log { page: "console", line: format!("Build started: job_id={job_id}\n") }).ok();

            let job_addr = cfg.job_addr.clone();
            let ui_stream = ui.clone();
            let ui_err = ui.clone();
            tokio::spawn(async move {
                if let Err(err) = stream_job_events(job_addr, job_id.clone(), "console", ui_stream).await {
                    let _ = ui_err.send(AppEvent::Log { page: "console", line: format!("job stream error ({job_id}): {err}\n") });
                }
            });
        }

        UiCommand::BuildListArtifacts {
            cfg,
            project_ref,
            variant,
            filter,
        } => {
            if project_ref.trim().is_empty() {
                ui.send(AppEvent::Log { page: "console", line: "Project path or id is required.\n".into() }).ok();
                return Ok(());
            }

            ui.send(AppEvent::Log { page: "console", line: format!("Listing artifacts via {}\n", cfg.build_addr) }).ok();
            let mut client = BuildServiceClient::new(connect(&cfg.build_addr).await?);
            let resp = match client.list_artifacts(ListArtifactsRequest {
                project_id: Some(Id { value: project_ref.trim().to_string() }),
                variant: variant as i32,
                filter: Some(filter),
            }).await {
                Ok(resp) => resp.into_inner(),
                Err(err) => {
                    ui.send(AppEvent::Log { page: "console", line: format!("List artifacts failed: {err}\n") }).ok();
                    return Ok(());
                }
            };

            if resp.artifacts.is_empty() {
                ui.send(AppEvent::Log { page: "console", line: "No artifacts found.\n".into() }).ok();
                return Ok(());
            }

            let total = resp.artifacts.len();
            let mut by_module: BTreeMap<String, Vec<Artifact>> = BTreeMap::new();
            for artifact in resp.artifacts {
                let module = metadata_value(&artifact.metadata, "module").unwrap_or("");
                let label = if module.trim().is_empty() {
                    "<root>".to_string()
                } else {
                    module.to_string()
                };
                by_module.entry(label).or_default().push(artifact);
            }

            ui.send(AppEvent::Log { page: "console", line: format!("Artifacts ({total})\n") }).ok();
            for (module, artifacts) in by_module {
                ui.send(AppEvent::Log { page: "console", line: format!("Module: {module}\n") }).ok();
                for artifact in artifacts {
                    ui.send(AppEvent::Log { page: "console", line: format_artifact_line(&artifact) }).ok();
                }
            }
        }
    }

    Ok(())
}

fn default_export_path(prefix: &str, job_id: &str) -> PathBuf {
    let ts = now_millis();
    data_dir()
        .join("state")
        .join(format!("{prefix}-{job_id}-{ts}.json"))
}

async fn collect_job_history(
    client: &mut JobServiceClient<Channel>,
    job_id: &str,
) -> Result<Vec<JobEvent>, Box<dyn std::error::Error>> {
    let mut events = Vec::new();
    let mut page_token = String::new();
    loop {
        let resp = client
            .list_job_history(ListJobHistoryRequest {
                job_id: Some(Id {
                    value: job_id.to_string(),
                }),
                page: Some(Pagination {
                    page_size: 200,
                    page_token: page_token.clone(),
                }),
                filter: None,
            })
            .await?
            .into_inner();
        events.extend(resp.events);
        let next_token = resp
            .page_info
            .map(|page_info| page_info.next_page_token)
            .unwrap_or_default();
        if next_token.is_empty() {
            break;
        }
        page_token = next_token;
    }
    Ok(events)
}

async fn stream_job_events_home(
    addr: &str,
    job_id: &str,
    ui: mpsc::Sender<AppEvent>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = JobServiceClient::new(connect(addr).await?);
    let mut stream = client
        .stream_job_events(StreamJobEventsRequest {
            job_id: Some(Id {
                value: job_id.to_string(),
            }),
            include_history: true,
        })
        .await?
        .into_inner();

    while let Some(item) = stream.next().await {
        match item {
            Ok(evt) => {
                if let Some(payload) = evt.payload.as_ref() {
                    match payload {
                        JobPayload::StateChanged(state) => {
                            let state = JobState::try_from(state.new_state)
                                .unwrap_or(JobState::Unspecified);
                            ui.send(AppEvent::HomeState { state: format!("{state:?}") }).ok();
                            if matches!(
                                state,
                                JobState::Success | JobState::Failed | JobState::Cancelled
                            ) {
                                ui.send(AppEvent::HomeResult { result: format!("{state:?}") }).ok();
                            }
                        }
                        JobPayload::Progress(progress) => {
                            if let Some(p) = progress.progress.as_ref() {
                                ui.send(AppEvent::HomeProgress {
                                    progress: format!("{}% {}", p.percent, p.phase),
                                }).ok();
                            }
                        }
                        JobPayload::Completed(completed) => {
                            ui.send(AppEvent::HomeState { state: "Success".into() }).ok();
                            ui.send(AppEvent::HomeResult { result: completed.summary.clone() }).ok();
                        }
                        JobPayload::Failed(failed) => {
                            let message = failed
                                .error
                                .as_ref()
                                .map(|err| err.message.clone())
                                .unwrap_or_else(|| "failed".into());
                            ui.send(AppEvent::HomeState { state: "Failed".into() }).ok();
                            ui.send(AppEvent::HomeResult { result: message }).ok();
                        }
                        JobPayload::Log(_) => {}
                    }
                }
                for line in job_event_lines(&evt) {
                    ui.send(AppEvent::Log { page: "home", line }).ok();
                }
                if let Some(JobPayload::Completed(_)) = evt.payload.as_ref() {
                    break;
                }
                if let Some(JobPayload::Failed(_)) = evt.payload.as_ref() {
                    break;
                }
                if let Some(JobPayload::StateChanged(state)) = evt.payload.as_ref() {
                    let state = JobState::try_from(state.new_state)
                        .unwrap_or(JobState::Unspecified);
                    if matches!(state, JobState::Cancelled) {
                        break;
                    }
                }
            }
            Err(err) => {
                ui.send(AppEvent::Log { page: "home", line: format!("job stream error: {err}\n") }).ok();
                break;
            }
        }
    }

    Ok(())
}

async fn stream_job_events(
    addr: String,
    job_id: String,
    page: &'static str,
    ui: mpsc::Sender<AppEvent>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = JobServiceClient::new(connect(&addr).await?);
    let mut stream = client.stream_job_events(StreamJobEventsRequest {
        job_id: Some(Id { value: job_id.clone() }),
        include_history: true,
    }).await?.into_inner();

    while let Some(item) = stream.next().await {
        match item {
            Ok(evt) => {
                if page == "console" {
                    if let Some(JobPayload::Completed(completed)) = evt.payload.as_ref() {
                        if let Some(apk) = completed.outputs.iter().find(|kv| kv.key == "apk_path") {
                            let _ = ui.send(AppEvent::SetLastBuildApk { apk_path: apk.value.clone() });
                        }
                    }
                }
                let line = format!("job {job_id}: {:?}\n", evt.payload);
                ui.send(AppEvent::Log { page, line }).ok();
            }
            Err(err) => {
                ui.send(AppEvent::Log { page, line: format!("job {job_id} stream error: {err}\n") }).ok();
                break;
            }
        }
    }

    Ok(())
}

async fn connect(addr: &str) -> Result<Channel, Box<dyn std::error::Error>> {
    let endpoint = format!("http://{addr}");
    Ok(Channel::from_shared(endpoint)?.connect().await?)
}
