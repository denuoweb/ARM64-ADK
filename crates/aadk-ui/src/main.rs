use std::{path::Path, sync::{Arc, mpsc}, thread, time::Duration};

use aadk_proto::aadk::v1::{
    build_service_client::BuildServiceClient,
    job_event::Payload as JobPayload,
    job_service_client::JobServiceClient,
    observe_service_client::ObserveServiceClient,
    project_service_client::ProjectServiceClient,
    target_service_client::TargetServiceClient,
    toolchain_service_client::ToolchainServiceClient,
    BuildRequest, BuildVariant, CancelJobRequest, CreateProjectRequest, ExportSupportBundleRequest,
    GetCuttlefishStatusRequest, Id, InstallApkRequest, InstallCuttlefishRequest,
    InstallToolchainRequest, KeyValue, LaunchRequest, ListAvailableRequest, ListInstalledRequest,
    ListProvidersRequest, ListRecentProjectsRequest, ListTargetsRequest, ListTemplatesRequest,
    OpenProjectRequest, StartCuttlefishRequest, StartJobRequest, StopCuttlefishRequest,
    StreamJobEventsRequest, StreamLogcatRequest, ToolchainKind, VerifyToolchainRequest,
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
    ToolchainVerifyInstalled { cfg: AppConfig },
    ProjectListTemplates { cfg: AppConfig },
    ProjectListRecent { cfg: AppConfig },
    ProjectCreate { cfg: AppConfig, name: String, path: String, template_id: String },
    ProjectOpen { cfg: AppConfig, path: String },
    TargetsList { cfg: AppConfig },
    TargetsInstallCuttlefish { cfg: AppConfig, force: bool },
    TargetsStartCuttlefish { cfg: AppConfig, show_full_ui: bool },
    TargetsStopCuttlefish { cfg: AppConfig },
    TargetsCuttlefishStatus { cfg: AppConfig },
    TargetsInstallApk { cfg: AppConfig, target_id: String, apk_path: String },
    TargetsLaunchApp { cfg: AppConfig, target_id: String, application_id: String, activity: String },
    TargetsStreamLogcat { cfg: AppConfig, target_id: String, filter: String },
    ObserveExportSupport { cfg: AppConfig },
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
    ProjectTemplates { templates: Vec<ProjectTemplateOption> },
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
                    AppEvent::ProjectTemplates { templates } => {
                        projects_for_events.set_templates(&templates);
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
}

#[derive(Clone, Debug)]
struct ProjectTemplateOption {
    id: String,
    name: String,
}

#[derive(Clone)]
struct ProjectsPage {
    page: Page,
    template_combo: gtk::ComboBoxText,
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
    row1.append(&list);
    row1.append(&list_sdk);
    row1.append(&list_ndk);

    let row2 = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let install_sdk = gtk::Button::with_label(&format!("Install SDK {SDK_VERSION} (verify)"));
    let install_ndk = gtk::Button::with_label(&format!("Install NDK {NDK_VERSION} (verify)"));
    let list_installed = gtk::Button::with_label("List Installed");
    let verify_installed = gtk::Button::with_label("Verify Installed");
    row2.append(&install_sdk);
    row2.append(&install_ndk);
    row2.append(&list_installed);
    row2.append(&verify_installed);

    actions.append(&row1);
    actions.append(&row2);
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

    let cfg_install_sdk = cfg.clone();
    let cmd_tx_install_sdk = cmd_tx.clone();
    install_sdk.connect_clicked(move |_| {
        let cfg = cfg_install_sdk.lock().unwrap().clone();
        cmd_tx_install_sdk
            .send(UiCommand::ToolchainInstall {
                cfg,
                provider_id: PROVIDER_SDK_ID.into(),
                version: SDK_VERSION.into(),
                verify: true,
            })
            .ok();
    });

    let cfg_install_ndk = cfg.clone();
    let cmd_tx_install_ndk = cmd_tx.clone();
    install_ndk.connect_clicked(move |_| {
        let cfg = cfg_install_ndk.lock().unwrap().clone();
        cmd_tx_install_ndk
            .send(UiCommand::ToolchainInstall {
                cfg,
                provider_id: PROVIDER_NDK_ID.into(),
                version: NDK_VERSION.into(),
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
    let list_recent = gtk::Button::with_label("List Recent");
    let open_btn = gtk::Button::with_label("Open Project");
    let create_btn = gtk::Button::with_label("Create Project");
    row.append(&refresh_templates);
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

    let cfg_refresh = cfg.clone();
    let cmd_tx_refresh = cmd_tx.clone();
    refresh_templates.connect_clicked(move |_| {
        let cfg = cfg_refresh.lock().unwrap().clone();
        cmd_tx_refresh.send(UiCommand::ProjectListTemplates { cfg }).ok();
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

    ProjectsPage { page, template_combo }
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
    let start = gtk::Button::with_label("Start Cuttlefish");
    let stop = gtk::Button::with_label("Stop Cuttlefish");
    let stream = gtk::Button::with_label("Stream Logcat (sample target)");
    row.append(&list);
    row.append(&status);
    row.append(&web_ui);
    row.append(&docs);
    row.append(&install);
    row.append(&start);
    row.append(&stop);
    row.append(&stream);
    page.container.insert_child_after(&row, Some(&page.container.first_child().unwrap()));

    let default_target = std::env::var("AADK_CUTTLEFISH_ADB_SERIAL")
        .unwrap_or_else(|_| "127.0.0.1:6520".into());

    let form = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let target_entry = gtk::Entry::builder()
        .text(default_target)
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

    page.container.insert_child_after(&form, Some(&row));

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
        let url = "https://source.android.com/docs/setup/create/cuttlefish";
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

    let cfg_install = cfg.clone();
    let cmd_tx_install = cmd_tx.clone();
    install.connect_clicked(move |_| {
        let cfg = cfg_install.lock().unwrap().clone();
        cmd_tx_install
            .send(UiCommand::TargetsInstallCuttlefish { cfg, force: false })
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

    TargetsPage { page, apk_entry }
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
    let page = make_page("Evidence — ObserveService (support bundle export)");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let export = gtk::Button::with_label("Export Support Bundle");
    row.append(&export);
    page.container.insert_child_after(&row, Some(&page.container.first_child().unwrap()));
    export.connect_clicked(move |_| {
        let cfg = cfg.lock().unwrap().clone();
        cmd_tx.send(UiCommand::ObserveExportSupport { cfg }).ok();
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

        UiCommand::TargetsInstallCuttlefish { cfg, force } => {
            ui.send(AppEvent::Log { page: "targets", line: format!("Connecting to TargetService at {}\n", cfg.targets_addr) }).ok();
            let mut client = TargetServiceClient::new(connect(&cfg.targets_addr).await?);
            let resp = client.install_cuttlefish(InstallCuttlefishRequest { force }).await?.into_inner();

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

        UiCommand::ObserveExportSupport { cfg } => {
            ui.send(AppEvent::Log { page: "evidence", line: format!("Connecting to ObserveService at {}\n", cfg.observe_addr) }).ok();
            let mut client = ObserveServiceClient::new(connect(&cfg.observe_addr).await?);
            let resp = client.export_support_bundle(ExportSupportBundleRequest {
                include_logs: true,
                include_config: true,
                include_toolchain_provenance: true,
                include_recent_runs: true,
                recent_runs_limit: 10,
            }).await?.into_inner();

            let job_id = resp.job_id.map(|i| i.value).unwrap_or_default();
            ui.send(AppEvent::Log { page: "evidence", line: format!("Support bundle job: {job_id}\nOutput path: {}\n", resp.output_path) }).ok();
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
