use std::{path::Path, sync::{Arc, mpsc}, thread, time::Duration};

use aadk_proto::aadk::v1::{
    build_service_client::BuildServiceClient,
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    observe_service_client::ObserveServiceClient,
    project_service_client::ProjectServiceClient,
    target_service_client::TargetServiceClient,
    toolchain_service_client::ToolchainServiceClient,
    BuildRequest, BuildVariant, CancelJobRequest, CreateProjectRequest, CreateToolchainSetRequest,
    ExportSupportBundleRequest, ExportEvidenceBundleRequest, GetActiveToolchainSetRequest,
    GetCuttlefishStatusRequest, GetDefaultTargetRequest, Id, InstallApkRequest, InstallCuttlefishRequest,
    InstallToolchainRequest, KeyValue, LaunchRequest, ListAvailableRequest, ListInstalledRequest,
    ListProvidersRequest, ListRecentProjectsRequest, ListTargetsRequest, ListTemplatesRequest,
    ListRunsRequest, ListToolchainSetsRequest, OpenProjectRequest, Pagination, ResolveCuttlefishBuildRequest,
    SetActiveToolchainSetRequest, SetDefaultTargetRequest, SetProjectConfigRequest, StartCuttlefishRequest,
    StartJobRequest, StopCuttlefishRequest, StreamJobEventsRequest, StreamLogcatRequest, ToolchainKind,
    VerifyToolchainRequest,
};
use futures_util::StreamExt;
use gtk4 as gtk;
use gtk::gio::prelude::FileExt;
use gtk::prelude::*;
use tonic::transport::Channel;

#[derive(Clone, Debug)]
struct AppConfig {
    job_addr: String,
    toolchain_addr: String,
    project_addr: String,
    build_addr: String,
    targets_addr: String,
    observe_addr: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            job_addr: std::env::var("AADK_JOB_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".into()),
            toolchain_addr: std::env::var("AADK_TOOLCHAIN_ADDR").unwrap_or_else(|_| "127.0.0.1:50052".into()),
            project_addr: std::env::var("AADK_PROJECT_ADDR").unwrap_or_else(|_| "127.0.0.1:50053".into()),
            build_addr: std::env::var("AADK_BUILD_ADDR").unwrap_or_else(|_| "127.0.0.1:50054".into()),
            targets_addr: std::env::var("AADK_TARGETS_ADDR").unwrap_or_else(|_| "127.0.0.1:50055".into()),
            observe_addr: std::env::var("AADK_OBSERVE_ADDR").unwrap_or_else(|_| "127.0.0.1:50056".into()),
        }
    }
}

#[derive(Clone, Default)]
struct AppState {
    current_job_id: Option<String>,
}

#[derive(Debug)]
enum UiCommand {
    HomeStartDemo { cfg: AppConfig },
    HomeCancelCurrent { cfg: AppConfig },
    ToolchainListProviders { cfg: AppConfig },
    ToolchainListAvailable { cfg: AppConfig, provider_id: String },
    ToolchainInstall { cfg: AppConfig, provider_id: String, version: String, verify: bool },
    ToolchainListInstalled { cfg: AppConfig, kind: ToolchainKind },
    ToolchainListSets { cfg: AppConfig },
    ToolchainVerifyInstalled { cfg: AppConfig },
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
    ProjectCreate { cfg: AppConfig, name: String, path: String, template_id: String },
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
    },
    TargetsResolveCuttlefishBuild {
        cfg: AppConfig,
        branch: String,
        target: String,
        build_id: String,
    },
    TargetsStartCuttlefish { cfg: AppConfig, show_full_ui: bool },
    TargetsStopCuttlefish { cfg: AppConfig },
    TargetsCuttlefishStatus { cfg: AppConfig },
    TargetsInstallApk { cfg: AppConfig, target_id: String, apk_path: String },
    TargetsLaunchApp { cfg: AppConfig, target_id: String, application_id: String, activity: String },
    TargetsStreamLogcat { cfg: AppConfig, target_id: String, filter: String },
    ObserveListRuns { cfg: AppConfig },
    ObserveExportSupport {
        cfg: AppConfig,
        include_logs: bool,
        include_config: bool,
        include_toolchain_provenance: bool,
        include_recent_runs: bool,
        recent_runs_limit: u32,
    },
    ObserveExportEvidence { cfg: AppConfig, run_id: String },
    BuildRun {
        cfg: AppConfig,
        project_ref: String,
        variant: BuildVariant,
        clean_first: bool,
        gradle_args: Vec<KeyValue>,
    },
}

#[derive(Debug, Clone)]
enum AppEvent {
    Log { page: &'static str, line: String },
    SetCurrentJob { job_id: Option<String> },
    SetLastBuildApk { apk_path: String },
    SetCuttlefishBuildId { build_id: String },
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

    let cfg = Arc::new(std::sync::Mutex::new(AppConfig::default()));
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
    let toolchains = page_toolchains(cfg.clone(), cmd_tx.clone());
    let projects = page_projects(cfg.clone(), cmd_tx.clone(), &window);
    let targets = page_targets(cfg.clone(), cmd_tx.clone(), &window);
    let evidence = page_evidence(cfg.clone(), cmd_tx.clone());
    let console = page_console(cfg.clone(), cmd_tx.clone(), &window);
    let settings = page_settings(cfg.clone());

    stack.add_titled(&home.page.container, Some("home"), "Home");
    stack.add_titled(&toolchains.container, Some("toolchains"), "Toolchains");
    stack.add_titled(&projects.page.container, Some("projects"), "Projects");
    stack.add_titled(&targets.page.container, Some("targets"), "Targets");
    stack.add_titled(&console.container, Some("console"), "Console");
    stack.add_titled(&evidence.container, Some("evidence"), "Evidence");
    stack.add_titled(&settings.container, Some("settings"), "Settings");

    // Clone page handles for event routing closure.
    let home_page_for_events = home.page.clone();
    let toolchains_for_events = toolchains.clone();
    let projects_for_events = projects.clone();
    let targets_for_events = targets.clone();
    let console_for_events = console.clone();
    let evidence_for_events = evidence.clone();

    // Event routing: drain worker events on the GTK thread.
    let state_for_events = state.clone();
    glib::source::timeout_add_local(Duration::from_millis(50), move || {
        loop {
            match ev_rx.try_recv() {
                Ok(ev) => match ev {
                    AppEvent::Log { page, line } => match page {
                        "home" => home_page_for_events.append(&line),
                        "toolchains" => toolchains_for_events.append(&line),
                        "projects" => projects_for_events.append(&line),
                        "targets" => targets_for_events.append(&line),
                        "console" => console_for_events.append(&line),
                        "evidence" => evidence_for_events.append(&line),
                        _ => {}
                    },
                    AppEvent::SetCurrentJob { job_id } => {
                        state_for_events.lock().unwrap().current_job_id = job_id;
                    }
                    AppEvent::SetLastBuildApk { apk_path } => {
                        targets_for_events.set_apk_path(&apk_path);
                    }
                    AppEvent::SetCuttlefishBuildId { build_id } => {
                        targets_for_events.set_cuttlefish_build_id(&build_id);
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
}

#[derive(Clone)]
struct TargetsPage {
    page: Page,
    apk_entry: gtk::Entry,
    cuttlefish_build_entry: gtk::Entry,
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

fn page_home(cfg: Arc<std::sync::Mutex<AppConfig>>, cmd_tx: mpsc::Sender<UiCommand>) -> HomePage {
    let page = make_page("Home — JobService demo (broadcast + replay)");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let start_btn = gtk::Button::with_label("Start Demo Job");
    let cancel_btn = gtk::Button::with_label("Cancel Current Job");

    row.append(&start_btn);
    row.append(&cancel_btn);

    page.container.insert_child_after(&row, Some(&page.container.first_child().unwrap()));

    start_btn.connect_clicked(move |_| {
        let cfg = cfg.lock().unwrap().clone();
        cmd_tx.send(UiCommand::HomeStartDemo { cfg }).ok();
    });

    HomePage { page, cancel_btn }
}

const PROVIDER_SDK_ID: &str = "provider-android-sdk-custom";
const PROVIDER_NDK_ID: &str = "provider-android-ndk-custom";
const SDK_VERSION: &str = "36.0.0";
const NDK_VERSION: &str = "r29";

fn page_toolchains(cfg: Arc<std::sync::Mutex<AppConfig>>, cmd_tx: mpsc::Sender<UiCommand>) -> Page {
    let page = make_page("Toolchains — ToolchainService");
    let actions = gtk::Box::new(gtk::Orientation::Vertical, 8);

    let row1 = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let list = gtk::Button::with_label("List Providers");
    let list_sdk = gtk::Button::with_label("List Available SDK");
    let list_ndk = gtk::Button::with_label("List Available NDK");
    let list_sets = gtk::Button::with_label("List Toolchain Sets");
    row1.append(&list);
    row1.append(&list_sdk);
    row1.append(&list_ndk);
    row1.append(&list_sets);

    let version_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();
    let sdk_version_entry = gtk::Entry::builder()
        .text(SDK_VERSION)
        .hexpand(true)
        .build();
    let ndk_version_entry = gtk::Entry::builder()
        .text(NDK_VERSION)
        .hexpand(true)
        .build();
    let label_sdk_version = gtk::Label::builder().label("SDK version").xalign(0.0).build();
    let label_ndk_version = gtk::Label::builder().label("NDK version").xalign(0.0).build();
    version_grid.attach(&label_sdk_version, 0, 0, 1, 1);
    version_grid.attach(&sdk_version_entry, 1, 0, 1, 1);
    version_grid.attach(&label_ndk_version, 0, 1, 1, 1);
    version_grid.attach(&ndk_version_entry, 1, 1, 1, 1);

    let row2 = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let install_sdk = gtk::Button::with_label("Install SDK (verify)");
    let install_ndk = gtk::Button::with_label("Install NDK (verify)");
    let list_installed = gtk::Button::with_label("List Installed");
    let verify_installed = gtk::Button::with_label("Verify Installed");
    row2.append(&install_sdk);
    row2.append(&install_ndk);
    row2.append(&list_installed);
    row2.append(&verify_installed);

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
    let create_set_btn = gtk::Button::with_label("Create Toolchain Set");
    let active_set_entry = gtk::Entry::builder()
        .placeholder_text("Active toolchain set id")
        .hexpand(true)
        .build();
    let set_active_btn = gtk::Button::with_label("Set Active");
    let get_active_btn = gtk::Button::with_label("Get Active");

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

    actions.append(&row1);
    actions.append(&version_grid);
    actions.append(&row2);
    actions.append(&set_grid);
    page.container.insert_child_after(&actions, Some(&page.container.first_child().unwrap()));

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
    let sdk_version_entry_install = sdk_version_entry.clone();
    install_sdk.connect_clicked(move |_| {
        let cfg = cfg_install_sdk.lock().unwrap().clone();
        let version = sdk_version_entry_install.text().to_string();
        cmd_tx_install_sdk
            .send(UiCommand::ToolchainInstall {
                cfg,
                provider_id: PROVIDER_SDK_ID.into(),
                version,
                verify: true,
            })
            .ok();
    });

    let cfg_install_ndk = cfg.clone();
    let cmd_tx_install_ndk = cmd_tx.clone();
    let ndk_version_entry_install = ndk_version_entry.clone();
    install_ndk.connect_clicked(move |_| {
        let cfg = cfg_install_ndk.lock().unwrap().clone();
        let version = ndk_version_entry_install.text().to_string();
        cmd_tx_install_ndk
            .send(UiCommand::ToolchainInstall {
                cfg,
                provider_id: PROVIDER_NDK_ID.into(),
                version,
                verify: true,
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
    verify_installed.connect_clicked(move |_| {
        let cfg = cfg_verify_installed.lock().unwrap().clone();
        cmd_tx_verify_installed
            .send(UiCommand::ToolchainVerifyInstalled { cfg })
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
    page
}

fn page_projects(
    cfg: Arc<std::sync::Mutex<AppConfig>>,
    cmd_tx: mpsc::Sender<UiCommand>,
    parent: &gtk::ApplicationWindow,
) -> ProjectsPage {
    let page = make_page("Projects — ProjectService");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let refresh_templates = gtk::Button::with_label("Refresh Templates");
    let refresh_defaults = gtk::Button::with_label("Refresh Defaults");
    let list_recent = gtk::Button::with_label("List Recent");
    let open_btn = gtk::Button::with_label("Open Project");
    let create_btn = gtk::Button::with_label("Create Project");
    row.append(&refresh_templates);
    row.append(&refresh_defaults);
    row.append(&list_recent);
    row.append(&open_btn);
    row.append(&create_btn);
    page.container
        .insert_child_after(&row, Some(&page.container.first_child().unwrap()));

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

    page.container.insert_child_after(&form, Some(&row));

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
    let config_btn = gtk::Button::with_label("Set Project Config");
    let use_defaults_btn = gtk::Button::with_label("Use Active Defaults");

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
    create_btn.connect_clicked(move |_| {
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
    let list = gtk::Button::with_label("List Targets");
    let status = gtk::Button::with_label("Cuttlefish Status");
    let web_ui = gtk::Button::with_label("Open Cuttlefish UI");
    let docs = gtk::Button::with_label("Open Cuttlefish Docs");
    let install = gtk::Button::with_label("Install Cuttlefish");
    let resolve_build = gtk::Button::with_label("Resolve Build ID");
    let start = gtk::Button::with_label("Start Cuttlefish");
    let stop = gtk::Button::with_label("Stop Cuttlefish");
    let stream = gtk::Button::with_label("Stream Logcat (sample target)");
    row.append(&list);
    row.append(&status);
    row.append(&web_ui);
    row.append(&docs);
    row.append(&install);
    row.append(&resolve_build);
    row.append(&start);
    row.append(&stop);
    row.append(&stream);
    page.container.insert_child_after(&row, Some(&page.container.first_child().unwrap()));

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
        .text("com.example.sampleprog")
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
    let launch_app = gtk::Button::with_label("Launch App");
    action_row.append(&install_apk);
    action_row.append(&launch_app);
    form.attach(&action_row, 1, 4, 1, 1);

    let default_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let set_default_btn = gtk::Button::with_label("Set Default Target");
    let get_default_btn = gtk::Button::with_label("Get Default Target");
    default_row.append(&set_default_btn);
    default_row.append(&get_default_btn);
    form.attach(&default_row, 1, 5, 1, 1);

    page.container.insert_child_after(&cuttlefish_grid, Some(&row));
    page.container.insert_child_after(&form, Some(&cuttlefish_grid));

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
    start.connect_clicked(move |_| {
        let cfg = cfg_start.lock().unwrap().clone();
        cmd_tx_start
            .send(UiCommand::TargetsStartCuttlefish { cfg, show_full_ui: true })
            .ok();
    });

    let cfg_stop = cfg.clone();
    let cmd_tx_stop = cmd_tx.clone();
    stop.connect_clicked(move |_| {
        let cfg = cfg_stop.lock().unwrap().clone();
        cmd_tx_stop
            .send(UiCommand::TargetsStopCuttlefish { cfg })
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
    install.connect_clicked(move |_| {
        let cfg = cfg_install.lock().unwrap().clone();
        cmd_tx_install
            .send(UiCommand::TargetsInstallCuttlefish {
                cfg,
                force: false,
                branch: branch_entry_install.text().to_string(),
                target: target_entry_install.text().to_string(),
                build_id: build_entry_install.text().to_string(),
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
    install_apk.connect_clicked(move |_| {
        let cfg = cfg_install_apk.lock().unwrap().clone();
        cmd_tx_install_apk
            .send(UiCommand::TargetsInstallApk {
                cfg,
                target_id: target_entry_install.text().to_string(),
                apk_path: apk_entry_install.text().to_string(),
            })
            .ok();
    });

    let cfg_launch = cfg.clone();
    let cmd_tx_launch = cmd_tx.clone();
    let target_entry_launch = target_entry.clone();
    let app_id_entry_launch = app_id_entry.clone();
    let activity_entry_launch = activity_entry.clone();
    launch_app.connect_clicked(move |_| {
        let cfg = cfg_launch.lock().unwrap().clone();
        cmd_tx_launch
            .send(UiCommand::TargetsLaunchApp {
                cfg,
                target_id: target_entry_launch.text().to_string(),
                application_id: app_id_entry_launch.text().to_string(),
                activity: activity_entry_launch.text().to_string(),
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
        .placeholder_text("Project path or id (recent id or AADK_PROJECT_ROOT)")
        .hexpand(true)
        .build();
    let project_browse = gtk::Button::with_label("Browse...");
    let args_entry = gtk::Entry::builder()
        .placeholder_text("Gradle args, e.g. --stacktrace -Pfoo=bar")
        .hexpand(true)
        .build();

    let variant_combo = gtk::DropDown::from_strings(&["debug", "release"]);
    variant_combo.set_selected(0);

    let clean_check = gtk::CheckButton::with_label("Clean first");

    let run = gtk::Button::with_label("Run Build");

    let label_project = gtk::Label::builder().label("Project").xalign(0.0).build();
    let label_variant = gtk::Label::builder().label("Variant").xalign(0.0).build();
    let label_args = gtk::Label::builder().label("Gradle args").xalign(0.0).build();

    form.attach(&label_project, 0, 0, 1, 1);
    let project_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    project_row.append(&project_entry);
    project_row.append(&project_browse);
    form.attach(&project_row, 1, 0, 1, 1);

    form.attach(&label_variant, 0, 1, 1, 1);
    form.attach(&variant_combo, 1, 1, 1, 1);

    form.attach(&label_args, 0, 2, 1, 1);
    form.attach(&args_entry, 1, 2, 1, 1);

    let options_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    options_row.append(&clean_check);
    form.attach(&options_row, 1, 3, 1, 1);

    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    action_row.append(&run);
    form.attach(&action_row, 1, 4, 1, 1);

    page.container.insert_child_after(&form, Some(&page.container.first_child().unwrap()));

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
    let args_entry_run = args_entry.clone();
    let variant_combo_run = variant_combo.clone();
    let clean_check_run = clean_check.clone();
    run.connect_clicked(move |_| {
        let cfg = cfg_run.lock().unwrap().clone();
        let project_ref = project_entry_run.text().to_string();
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
                clean_first,
                gradle_args,
            })
            .ok();
    });

    page
}

fn page_evidence(cfg: Arc<std::sync::Mutex<AppConfig>>, cmd_tx: mpsc::Sender<UiCommand>) -> Page {
    let page = make_page("Evidence — ObserveService (runs + bundle export)");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let list_runs = gtk::Button::with_label("List Runs");
    let export_support = gtk::Button::with_label("Export Support Bundle");
    let export_evidence = gtk::Button::with_label("Export Evidence Bundle");
    row.append(&list_runs);
    row.append(&export_support);
    row.append(&export_evidence);
    page.container
        .insert_child_after(&row, Some(&page.container.first_child().unwrap()));

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

    page.container.insert_child_after(&form, Some(&row));

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
    export_support.connect_clicked(move |_| {
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
            })
            .ok();
    });

    let cfg_evidence = cfg.clone();
    let cmd_tx_evidence = cmd_tx.clone();
    let run_id_evidence = run_id_entry.clone();
    export_evidence.connect_clicked(move |_| {
        let cfg = cfg_evidence.lock().unwrap().clone();
        cmd_tx_evidence
            .send(UiCommand::ObserveExportEvidence {
                cfg,
                run_id: run_id_evidence.text().to_string(),
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
    add_row(0, "JobService", cfg.lock().unwrap().job_addr.clone(), Box::new(move |v| cfg1.lock().unwrap().job_addr = v));

    let cfg2 = cfg.clone();
    add_row(1, "ToolchainService", cfg.lock().unwrap().toolchain_addr.clone(), Box::new(move |v| cfg2.lock().unwrap().toolchain_addr = v));

    let cfg3 = cfg.clone();
    add_row(2, "ProjectService", cfg.lock().unwrap().project_addr.clone(), Box::new(move |v| cfg3.lock().unwrap().project_addr = v));

    let cfg4 = cfg.clone();
    add_row(3, "BuildService", cfg.lock().unwrap().build_addr.clone(), Box::new(move |v| cfg4.lock().unwrap().build_addr = v));

    let cfg5 = cfg.clone();
    add_row(4, "TargetService", cfg.lock().unwrap().targets_addr.clone(), Box::new(move |v| cfg5.lock().unwrap().targets_addr = v));

    let cfg6 = cfg.clone();
    add_row(5, "ObserveService", cfg.lock().unwrap().observe_addr.clone(), Box::new(move |v| cfg6.lock().unwrap().observe_addr = v));

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

async fn handle_command(cmd: UiCommand, worker_state: &mut AppState, ui: mpsc::Sender<AppEvent>) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        UiCommand::HomeStartDemo { cfg } => {
            ui.send(AppEvent::Log { page: "home", line: format!("Connecting to JobService at {}\n", cfg.job_addr) }).ok();

            let mut client = JobServiceClient::new(connect(&cfg.job_addr).await?);
            let resp = client.start_job(StartJobRequest {
                job_type: "demo.job".into(),
                params: vec![],
                project_id: None,
                target_id: None,
                toolchain_set_id: None,
            }).await?.into_inner();

            let job_id = resp.job.and_then(|r| r.job_id).map(|i| i.value).unwrap_or_default();
            worker_state.current_job_id = Some(job_id.clone());
            ui.send(AppEvent::SetCurrentJob { job_id: Some(job_id.clone()) }).ok();
            ui.send(AppEvent::Log { page: "home", line: format!("Started demo job: {job_id}\n") }).ok();

            let mut stream = client.stream_job_events(StreamJobEventsRequest {
                job_id: Some(Id { value: job_id.clone() }),
                include_history: true,
            }).await?.into_inner();

            while let Some(item) = stream.next().await {
                match item {
                    Ok(evt) => ui.send(AppEvent::Log { page: "home", line: format!("event: {:?}\n", evt.payload) }).ok(),
                    Err(s) => { ui.send(AppEvent::Log { page: "home", line: format!("stream error: {s}\n") }).ok(); break; }
                };
            }
        }

        UiCommand::HomeCancelCurrent { cfg } => {
            if let Some(job_id) = worker_state.current_job_id.clone() {
                ui.send(AppEvent::Log { page: "home", line: format!("Cancelling job: {job_id}\n") }).ok();
                let mut client = JobServiceClient::new(connect(&cfg.job_addr).await?);
                let resp = client.cancel_job(CancelJobRequest { job_id: Some(Id { value: job_id.clone() }) }).await?.into_inner();
                ui.send(AppEvent::Log { page: "home", line: format!("Cancel accepted: {}\n", resp.accepted) }).ok();
            }
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
            let resp = client.list_available(ListAvailableRequest {
                provider_id: Some(Id { value: provider_id }),
                page: None,
            }).await?.into_inner();

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

        UiCommand::ToolchainInstall { cfg, provider_id, version, verify } => {
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

        UiCommand::ToolchainVerifyInstalled { cfg } => {
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

        UiCommand::ProjectCreate { cfg, name, path, template_id } => {
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
        } => {
            ui.send(AppEvent::Log { page: "targets", line: format!("Connecting to TargetService at {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client
                .install_cuttlefish(InstallCuttlefishRequest {
                    force,
                    branch: branch.trim().to_string(),
                    target: target.trim().to_string(),
                    build_id: build_id.trim().to_string(),
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

        UiCommand::TargetsStartCuttlefish { cfg, show_full_ui } => {
            ui.send(AppEvent::Log { page: "targets", line: format!("Connecting to TargetService at {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client.start_cuttlefish(StartCuttlefishRequest { show_full_ui }).await?.into_inner();

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

        UiCommand::TargetsStopCuttlefish { cfg } => {
            ui.send(AppEvent::Log { page: "targets", line: format!("Connecting to TargetService at {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client.stop_cuttlefish(StopCuttlefishRequest {}).await?.into_inner();

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

        UiCommand::TargetsInstallApk { cfg, target_id, apk_path } => {
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

        UiCommand::TargetsLaunchApp { cfg, target_id, application_id, activity } => {
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
        } => {
            ui.send(AppEvent::Log { page: "evidence", line: format!("Connecting to ObserveService at {}\n", cfg.observe_addr) }).ok();
            let mut client = ObserveServiceClient::new(connect(&cfg.observe_addr).await?);
            let resp = client.export_support_bundle(ExportSupportBundleRequest {
                include_logs,
                include_config,
                include_toolchain_provenance,
                include_recent_runs,
                recent_runs_limit,
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

        UiCommand::ObserveExportEvidence { cfg, run_id } => {
            let run_id = run_id.trim().to_string();
            if run_id.is_empty() {
                ui.send(AppEvent::Log { page: "evidence", line: "Run id is required for evidence export.\n".into() }).ok();
                return Ok(());
            }
            ui.send(AppEvent::Log { page: "evidence", line: format!("Connecting to ObserveService at {}\n", cfg.observe_addr) }).ok();
            let mut client = ObserveServiceClient::new(connect(&cfg.observe_addr).await?);
            let resp = client.export_evidence_bundle(ExportEvidenceBundleRequest {
                run_id: Some(Id { value: run_id.clone() }),
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

        UiCommand::BuildRun { cfg, project_ref, variant, clean_first, gradle_args } => {
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
