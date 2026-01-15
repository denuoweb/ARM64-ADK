mod commands;
mod config;
mod models;
mod pages;
mod utils;
mod worker;

use std::{
    sync::{mpsc, Arc},
    thread,
    time::Duration,
};

use gtk::gdk;
use gtk::gdk::prelude::{DisplayExt, MonitorExt};
use gtk::gio::prelude::ListModelExt;
use gtk::prelude::*;
use gtk4 as gtk;

use commands::{AppEvent, UiCommand};
use config::AppConfig;
use pages::{
    page_console, page_evidence, page_home, page_jobs_history, page_projects, page_settings,
    page_targets, page_toolchains, page_workflow,
};
use worker::{handle_command, AppState};

fn main() {
    let app = gtk::Application::builder()
        .application_id("dev.aadk.ui.full_scaffold")
        .build();

    app.connect_activate(build_ui);
    app.run();
}

fn default_window_size() -> (i32, i32) {
    let base_width = 1100;
    let base_height = 700;
    let mut width = base_width;
    let mut height = base_height;

    if let Some(display) = gdk::Display::default() {
        let monitors = display.monitors();
        if let Some(item) = monitors.item(0) {
            if let Ok(monitor) = item.downcast::<gdk::Monitor>() {
                let geometry = monitor.geometry();
                let max_width = (geometry.width() as f32 * 0.9) as i32;
                let max_height = (geometry.height() as f32 * 0.9) as i32;
                if max_width > 0 {
                    width = width.min(max_width);
                }
                if max_height > 0 {
                    height = height.min(max_height);
                }
            }
        }
    }

    (width, height)
}

fn build_ui(app: &gtk::Application) {
    let (default_width, default_height) = default_window_size();
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("AADK UI — Full Scaffold")
        .default_width(default_width)
        .default_height(default_height)
        .resizable(true)
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
    let workflow = page_workflow(cfg.clone(), cmd_tx.clone(), &window);
    let jobs_history = page_jobs_history(cfg.clone(), cmd_tx.clone());
    let toolchains = page_toolchains(cfg.clone(), cmd_tx.clone());
    let projects = page_projects(cfg.clone(), cmd_tx.clone(), &window);
    let targets = page_targets(cfg.clone(), cmd_tx.clone(), &window);
    let evidence = page_evidence(cfg.clone(), cmd_tx.clone());
    let console = page_console(cfg.clone(), cmd_tx.clone(), &window);
    let settings = page_settings(cfg.clone());

    stack.add_titled(&home.page.root, Some("home"), "Job Control");
    stack.add_titled(&workflow.root, Some("workflow"), "Workflow");
    stack.add_titled(&toolchains.page.root, Some("toolchains"), "Toolchains");
    stack.add_titled(&projects.page.root, Some("projects"), "Projects");
    stack.add_titled(&console.page.root, Some("console"), "Build");
    stack.add_titled(&targets.page.root, Some("targets"), "Targets");
    stack.add_titled(&jobs_history.root, Some("jobs"), "Job History");
    stack.add_titled(&evidence.root, Some("evidence"), "Evidence");
    stack.add_titled(&settings.root, Some("settings"), "Settings");

    // Clone page handles for event routing closure.
    let home_page_for_events = home.clone();
    let workflow_for_events = workflow.clone();
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
                        "workflow" => workflow_for_events.append(&line),
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
                    AppEvent::ProjectSelected {
                        project_id,
                        project_path,
                    } => {
                        let project_ref = if project_id.trim().is_empty() {
                            project_path.clone()
                        } else {
                            project_id.clone()
                        };
                        projects_for_events.set_project_id(&project_id);
                        console_for_events.set_project_ref(&project_ref);
                        let mut cfg = cfg_for_events.lock().unwrap();
                        if !project_ref.trim().is_empty() {
                            cfg.last_job_project_id = project_ref;
                            if let Err(err) = cfg.save() {
                                eprintln!("Failed to persist UI config: {err}");
                            }
                        }
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
