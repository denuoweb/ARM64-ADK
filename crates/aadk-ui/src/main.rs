mod commands;
mod config;
mod models;
mod pages;
mod ui_events;
mod utils;
mod worker;

use std::{sync::Arc, thread};

use glib::prelude::*;
use gtk::gdk;
use gtk::gdk::prelude::{DisplayExt, MonitorExt};
use gtk::gio::prelude::ListModelExt;
use gtk::prelude::*;
use gtk4 as gtk;
use tokio::sync::mpsc;

use aadk_telemetry as telemetry;
use commands::{AppEvent, UiCommand};
use config::AppConfig;
use models::ActiveContext;
use pages::{
    page_console, page_evidence, page_home, page_jobs_history, page_projects, page_settings,
    page_targets, page_toolchains, page_workflow, BuildPage, ProjectsPage, TargetsPage,
    SettingsPage, ToolchainsPage, WorkflowPage,
};
use ui_events::{UiEventQueue, DEFAULT_EVENT_QUEUE_SIZE};
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

#[derive(Clone)]
struct ContextBar {
    project_label: gtk::Label,
    toolchain_label: gtk::Label,
    target_label: gtk::Label,
    run_label: gtk::Label,
}

impl ContextBar {
    fn set_context(&self, ctx: &ActiveContext) {
        let project_ref = if ctx.project_id.trim().is_empty() {
            ctx.project_path.trim()
        } else {
            ctx.project_id.trim()
        };
        self.project_label
            .set_text(&format!("Project: {}", format_context_value(project_ref)));
        self.toolchain_label.set_text(&format!(
            "Toolchain set: {}",
            format_context_value(ctx.toolchain_set_id.trim())
        ));
        self.target_label.set_text(&format!(
            "Target: {}",
            format_context_value(ctx.target_id.trim())
        ));
        self.run_label
            .set_text(&format!("Run: {}", format_context_value(ctx.run_id.trim())));
    }
}

fn format_context_value(value: &str) -> &str {
    if value.is_empty() {
        "-"
    } else {
        value
    }
}

fn apply_active_context(
    ctx: &ActiveContext,
    context_bar: &ContextBar,
    workflow: &WorkflowPage,
    projects: &ProjectsPage,
    targets: &TargetsPage,
    toolchains: &ToolchainsPage,
    console: &BuildPage,
) {
    context_bar.set_context(ctx);
    workflow.set_context(ctx);
    projects.set_active_context(ctx);
    targets.set_target_id(&ctx.target_id);
    toolchains.set_active_set_id(&ctx.toolchain_set_id);
    console.set_project_ref(&ctx.project_ref());
}

fn set_tooltip<W: gtk::prelude::IsA<gtk::Widget>>(widget: &W, text: &str) {
    widget.set_tooltip_text(Some(text));
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
        let crash_enabled =
            telemetry_env_override("AADK_TELEMETRY_CRASH").unwrap_or(cfg.telemetry_crash_enabled);
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
    let pending_project_prompt = Arc::new(std::sync::Mutex::new(false));

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

    // Layout: context header + sidebar + stack
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .hexpand(true)
        .vexpand(true)
        .build();

    let sidebar = gtk::StackSidebar::builder()
        .stack(&stack)
        .width_request(220)
        .build();

    body.append(&sidebar);
    body.append(&stack);

    let context_frame = gtk::Frame::builder().label("Active context").build();
    context_frame.set_margin_top(8);
    context_frame.set_margin_bottom(8);
    context_frame.set_margin_start(12);
    context_frame.set_margin_end(12);

    let context_grid = gtk::Grid::builder()
        .row_spacing(4)
        .column_spacing(16)
        .build();
    let project_label = gtk::Label::builder()
        .label("Project: -")
        .xalign(0.0)
        .build();
    let toolchain_label = gtk::Label::builder()
        .label("Toolchain set: -")
        .xalign(0.0)
        .build();
    let target_label = gtk::Label::builder().label("Target: -").xalign(0.0).build();
    let run_label = gtk::Label::builder().label("Run: -").xalign(0.0).build();
    let context_bar = ContextBar {
        project_label: project_label.clone(),
        toolchain_label: toolchain_label.clone(),
        target_label: target_label.clone(),
        run_label: run_label.clone(),
    };

    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let new_project_btn = gtk::Button::with_label("New project");
    let open_project_btn = gtk::Button::with_label("Open project");
    set_tooltip(
        &new_project_btn,
        "What: Start a new project. Why: reset local state and pick a workspace. How: confirm reset, then choose a project folder.",
    );
    set_tooltip(&open_project_btn, "What: Open an existing project. Why: set the active project context. How: choose a project folder.");
    action_row.append(&new_project_btn);
    action_row.append(&open_project_btn);

    context_grid.attach(&project_label, 0, 0, 1, 1);
    context_grid.attach(&toolchain_label, 1, 0, 1, 1);
    context_grid.attach(&target_label, 0, 1, 1, 1);
    context_grid.attach(&run_label, 1, 1, 1, 1);
    context_grid.attach(&action_row, 2, 0, 1, 2);

    context_frame.set_child(Some(&context_grid));

    root.append(&context_frame);
    root.append(&body);

    // Pages
    let home = page_home(cfg.clone(), cmd_tx.clone());
    let workflow = page_workflow(cfg.clone(), cmd_tx.clone(), &window);
    let jobs_history = page_jobs_history(cfg.clone(), cmd_tx.clone());
    let toolchains = page_toolchains(cfg.clone(), cmd_tx.clone());
    let projects = page_projects(cfg.clone(), cmd_tx.clone(), &window);
    let targets = page_targets(cfg.clone(), cmd_tx.clone(), &window);
    let evidence = page_evidence(cfg.clone(), cmd_tx.clone());
    let console = page_console(cfg.clone(), cmd_tx.clone(), &window);
    let settings = page_settings(cfg.clone(), cmd_tx.clone(), &window);

    let stack_for_new = stack.clone();
    let cfg_reset = cfg.clone();
    let cmd_tx_reset = cmd_tx.clone();
    let window_reset = window.clone();
    let pending_project_prompt_reset = pending_project_prompt.clone();
    new_project_btn.connect_clicked(move |_| {
        stack_for_new.set_visible_child_name("projects");
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&window_reset)
            .modal(true)
            .message_type(gtk::MessageType::Warning)
            .text("Reset all local AADK state before starting a new project?")
            .secondary_text("This deletes cached state, job history, toolchains, downloads, bundles, and UI selections. Running jobs will keep going.")
            .build();
        dialog.add_buttons(&[
            ("Cancel", gtk::ResponseType::Cancel),
            ("Reset and continue", gtk::ResponseType::Accept),
        ]);
        let cmd_tx_confirm = cmd_tx_reset.clone();
        let cfg_confirm = cfg_reset.clone();
        let pending_confirm = pending_project_prompt_reset.clone();
        dialog.connect_response(move |dialog, response| {
            if response == gtk::ResponseType::Accept {
                let cfg = cfg_confirm.lock().unwrap().clone();
                if cmd_tx_confirm
                    .try_send(UiCommand::ResetAllState { cfg })
                    .is_ok()
                {
                    *pending_confirm.lock().unwrap() = true;
                }
            }
            dialog.close();
        });
        dialog.show();
    });

    let stack_for_open = stack.clone();
    let window_open = window.clone();
    let cfg_open = cfg.clone();
    let cmd_tx_open = cmd_tx.clone();
    let projects_open = projects.clone();
    open_project_btn.connect_clicked(move |_| {
        stack_for_open.set_visible_child_name("projects");
        projects_open.prompt_project_path(&window_open, &cfg_open, &cmd_tx_open);
    });

    {
        let cfg = cfg.lock().unwrap().clone();
        let ctx = cfg.active_context();
        apply_active_context(
            &ctx,
            &context_bar,
            &workflow,
            &projects,
            &targets,
            &toolchains,
            &console,
        );
    }

    stack.add_titled(&home.page.root, Some("home"), "Job Control");
    stack.add_titled(&workflow.page.root, Some("workflow"), "Workflow");
    stack.add_titled(&toolchains.page.root, Some("toolchains"), "Toolchains");
    stack.add_titled(&projects.page.root, Some("projects"), "Projects");
    stack.add_titled(&console.page.root, Some("console"), "Build");
    stack.add_titled(&targets.page.root, Some("targets"), "Targets");
    stack.add_titled(&jobs_history.root, Some("jobs"), "Job History");
    stack.add_titled(&evidence.root, Some("evidence"), "Evidence");
    stack.add_titled(&settings.page.root, Some("settings"), "Settings");

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
    let settings_for_events = settings.clone();
    let context_bar_for_events = context_bar.clone();
    let cfg_for_events = cfg.clone();
    let pending_project_prompt_for_events = pending_project_prompt.clone();
    let window_for_events = window.clone();
    let cmd_tx_for_events = cmd_tx.clone();

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
                        "settings" => settings_for_events.append(&line),
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
                        let ctx = cfg_for_events.lock().unwrap().active_context();
                        projects_for_events.set_active_context(&ctx);
                    }
                    AppEvent::ProjectTargets { targets } => {
                        projects_for_events.set_targets(&targets);
                        let ctx = cfg_for_events.lock().unwrap().active_context();
                        projects_for_events.set_active_context(&ctx);
                    }
                    AppEvent::ProjectSelected {
                        project_id,
                        project_path,
                    } => {
                        let ctx = {
                            let mut cfg = cfg_for_events.lock().unwrap();
                            if !project_id.trim().is_empty() {
                                cfg.active_project_id = project_id.clone();
                            }
                            if !project_path.trim().is_empty() || !project_id.trim().is_empty() {
                                cfg.active_project_path = project_path.clone();
                            }
                            let project_ref = if cfg.active_project_id.trim().is_empty() {
                                cfg.active_project_path.clone()
                            } else {
                                cfg.active_project_id.clone()
                            };
                            if !project_ref.trim().is_empty() {
                                cfg.last_job_project_id = project_ref;
                            }
                            if let Err(err) = cfg.save() {
                                eprintln!("Failed to persist UI config: {err}");
                            }
                            cfg.active_context()
                        };
                        apply_active_context(
                            &ctx,
                            &context_bar_for_events,
                            &workflow_for_events,
                            &projects_for_events,
                            &targets_for_events,
                            &toolchains_for_events,
                            &console_for_events,
                        );
                    }
                    AppEvent::UpdateActiveContext {
                        project_id,
                        project_path,
                        toolchain_set_id,
                        target_id,
                        run_id,
                    } => {
                        let ctx = {
                            let mut cfg = cfg_for_events.lock().unwrap();
                            let mut project_updated = false;
                            if let Some(value) = project_id {
                                cfg.active_project_id = value;
                                project_updated = true;
                            }
                            if let Some(value) = project_path {
                                cfg.active_project_path = value;
                                project_updated = true;
                            }
                            if let Some(value) = toolchain_set_id {
                                cfg.active_toolchain_set_id = value.clone();
                                cfg.last_job_toolchain_set_id = value;
                            }
                            if let Some(value) = target_id {
                                cfg.active_target_id = value.clone();
                                cfg.last_job_target_id = value;
                            }
                            if let Some(value) = run_id {
                                cfg.active_run_id = value;
                            }
                            if project_updated {
                                let project_ref = if cfg.active_project_id.trim().is_empty() {
                                    cfg.active_project_path.clone()
                                } else {
                                    cfg.active_project_id.clone()
                                };
                                cfg.last_job_project_id = project_ref;
                            }
                            if let Err(err) = cfg.save() {
                                eprintln!("Failed to persist UI config: {err}");
                            }
                            cfg.active_context()
                        };
                        apply_active_context(
                            &ctx,
                            &context_bar_for_events,
                            &workflow_for_events,
                            &projects_for_events,
                            &targets_for_events,
                            &toolchains_for_events,
                            &console_for_events,
                        );
                    }
                    AppEvent::ResetAllStateComplete { ok } => {
                        if ok {
                            let ctx = {
                                let mut cfg = cfg_for_events.lock().unwrap();
                                cfg.clear_cached_state();
                                if let Err(err) = cfg.save() {
                                    eprintln!("Failed to persist UI config: {err}");
                                }
                                cfg.active_context()
                            };
                            {
                                let mut state = state_for_events.lock().unwrap();
                                state.current_job_id = None;
                            }
                            home_page_for_events.reset_status();
                            apply_active_context(
                                &ctx,
                                &context_bar_for_events,
                                &workflow_for_events,
                                &projects_for_events,
                                &targets_for_events,
                                &toolchains_for_events,
                                &console_for_events,
                            );
                            home_page_for_events.page.clear();
                            workflow_for_events.page.clear();
                            jobs_for_events.clear();
                            toolchains_for_events.page.clear();
                            projects_for_events.page.clear();
                            targets_for_events.page.clear();
                            console_for_events.page.clear();
                            evidence_for_events.clear();
                            settings_for_events.clear();
                        }
                        let should_prompt = {
                            let mut pending = pending_project_prompt_for_events.lock().unwrap();
                            let should_prompt = *pending && ok;
                            *pending = false;
                            should_prompt
                        };
                        if should_prompt {
                            projects_for_events.prompt_project_path(
                                &window_for_events,
                                &cfg_for_events,
                                &cmd_tx_for_events,
                            );
                        }
                    }
                    AppEvent::ConfigReloaded { cfg } => {
                        {
                            let mut cfg_guard = cfg_for_events.lock().unwrap();
                            *cfg_guard = cfg.clone();
                        }
                        let ctx = cfg.active_context();
                        apply_active_context(
                            &ctx,
                            &context_bar_for_events,
                            &workflow_for_events,
                            &projects_for_events,
                            &targets_for_events,
                            &toolchains_for_events,
                            &console_for_events,
                        );
                        settings_for_events.apply_config(&cfg);
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
