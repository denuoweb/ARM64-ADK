mod commands;
mod config;
mod models;
mod pages;
mod ui_events;
mod utils;
mod worker;

use std::{
    sync::Arc,
    thread,
};

use gtk::gdk;
use gtk::gdk::prelude::{DisplayExt, MonitorExt};
use gtk::gio::prelude::ListModelExt;
use gtk::prelude::*;
use gtk4 as gtk;
use glib::prelude::*;
use tokio::sync::mpsc;

use commands::{AppEvent, UiCommand};
use config::AppConfig;
use pages::{
    page_console, page_evidence, page_home, page_jobs_history, page_projects, page_settings,
    page_targets, page_toolchains, page_workflow,
};
use ui_events::{UiEventQueue, DEFAULT_EVENT_QUEUE_SIZE};
use worker::{handle_command, AppState};
use aadk_telemetry as telemetry;

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

fn telemetry_env_override(name: &str) -> Option<bool> {
    match std::env::var(name) {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        },
        Err(_) => None,
    }
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
    let (usage_enabled, crash_enabled, install_id) = {
        let mut cfg = cfg.lock().unwrap();
        let usage_enabled =
            telemetry_env_override("AADK_TELEMETRY").unwrap_or(cfg.telemetry_usage_enabled);
        let crash_enabled = telemetry_env_override("AADK_TELEMETRY_CRASH")
            .unwrap_or(cfg.telemetry_crash_enabled);
        cfg.telemetry_usage_enabled = usage_enabled;
        cfg.telemetry_crash_enabled = crash_enabled;
        if (usage_enabled || crash_enabled) && cfg.telemetry_install_id.trim().is_empty() {
            cfg.telemetry_install_id = telemetry::generate_install_id();
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        (
            usage_enabled,
            crash_enabled,
            cfg.telemetry_install_id.clone(),
        )
    };
    telemetry::init(telemetry::TelemetryOptions {
        app_name: "aadk-ui",
        app_version: env!("CARGO_PKG_VERSION"),
        usage_enabled,
        crash_enabled,
        install_id: if install_id.trim().is_empty() {
            None
        } else {
            Some(install_id)
        },
    });
    telemetry::event("app.start", &[]);
    let state = Arc::new(std::sync::Mutex::new(AppState::default()));

    let (cmd_tx, cmd_rx) = mpsc::channel::<UiCommand>(128);
    let (event_queue, mut notify_rx) = UiEventQueue::new(DEFAULT_EVENT_QUEUE_SIZE);
    let ui_events = event_queue.sender();

    // Background thread with tokio runtime; holds a private copy of AppState for worker actions.
    // State mutations are pushed to GTK via AppEvent.
    let mut cmd_rx = cmd_rx;
    thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");

        rt.block_on(async move {
            let mut worker_state = AppState::default();
            let mut stream_tasks = tokio::task::JoinSet::new();

            loop {
                tokio::select! {
                    cmd = cmd_rx.recv() => {
                        let Some(cmd) = cmd else { break };
                        let cmd_name = cmd.name();
                        telemetry::event("ui.command.start", &[("command", cmd_name)]);
                        let result = handle_command(
                            cmd,
                            &mut worker_state,
                            ui_events.clone(),
                            &mut stream_tasks,
                        )
                        .await;
                        match result {
                            Ok(()) => {
                                telemetry::event(
                                    "ui.command.result",
                                    &[("command", cmd_name), ("result", "ok")],
                                );
                            }
                            Err(err) => {
                                telemetry::event(
                                    "ui.command.result",
                                    &[("command", cmd_name), ("result", "err")],
                                );
                                eprintln!("worker error: {err}");
                            }
                        }
                    }
                    Some(result) = stream_tasks.join_next(), if !stream_tasks.is_empty() => {
                        if let Err(err) = result {
                            if !err.is_cancelled() {
                                eprintln!("stream task error: {err}");
                            }
                        }
                    }
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

    stack.connect_visible_child_notify(|stack| {
        if let Some(name) = stack.visible_child_name() {
            telemetry::event("ui.page.view", &[("page", name.as_str())]);
        }
    });

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
    let event_queue_for_events = event_queue.clone();
    glib::MainContext::default().spawn_local(async move {
        while notify_rx.recv().await.is_some() {
            for ev in event_queue_for_events.drain() {
                match ev {
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
                }
            }
        }
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
                cmd_tx.try_send(UiCommand::HomeCancelCurrent { cfg }).ok();
            }
        });
    }

    window.set_child(Some(&root));
    window.present();
}
