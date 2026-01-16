use std::{path::Path, sync::Arc};

use aadk_proto::aadk::v1::{
    ArtifactFilter, ArtifactType, BuildVariant, KeyValue, RunOutputKind, ToolchainKind,
    WorkflowPipelineOptions,
};
use aadk_telemetry as telemetry;
use gtk::gio::prelude::FileExt;
use gtk::prelude::*;
use gtk4 as gtk;
use tokio::sync::mpsc;

use crate::commands::UiCommand;
use crate::config::AppConfig;
use crate::models::{ActiveContext, ProjectTemplateOption, TargetOption, ToolchainSetOption};
use crate::utils::parse_list_tokens;

#[derive(Clone)]
pub(crate) struct Page {
    pub(crate) root: gtk::ScrolledWindow,
    pub(crate) container: gtk::Box,
    pub(crate) intro: gtk::Box,
    pub(crate) buffer: gtk::TextBuffer,
    pub(crate) textview: gtk::TextView,
}

impl Page {
    pub(crate) fn append(&self, s: &str) {
        const MAX_CHARS: i32 = 200_000;
        const TRIM_CHARS: i32 = 20_000;
        const MAX_LINES: i32 = 2_000;

        let mut end = self.buffer.end_iter();
        self.buffer.insert(&mut end, s);

        let line_count = self.buffer.line_count();
        if line_count > MAX_LINES {
            let mut start = self.buffer.start_iter();
            let mut cut = self.buffer.start_iter();
            cut.forward_lines(line_count - MAX_LINES);
            self.buffer.delete(&mut start, &mut cut);
        }

        if self.buffer.char_count() > MAX_CHARS {
            let mut start = self.buffer.start_iter();
            let mut cut = self.buffer.start_iter();
            cut.forward_chars(TRIM_CHARS);
            self.buffer.delete(&mut start, &mut cut);
        }

        let mut end = self.buffer.end_iter();
        self.textview.scroll_to_iter(&mut end, 0.0, false, 0.0, 0.0);
    }

    pub(crate) fn clear(&self) {
        self.buffer.set_text("");
    }
}

#[derive(Clone)]
pub(crate) struct HomePage {
    pub(crate) page: Page,
    pub(crate) cancel_btn: gtk::Button,
    pub(crate) job_id_label: gtk::Label,
    pub(crate) state_label: gtk::Label,
    pub(crate) progress_label: gtk::Label,
    pub(crate) result_label: gtk::Label,
}

impl HomePage {
    pub(crate) fn append(&self, s: &str) {
        self.page.append(s);
    }

    pub(crate) fn set_job_id(&self, job_id: Option<&str>) {
        let label = job_id.unwrap_or("-");
        self.job_id_label.set_text(&format!("job_id: {label}"));
    }

    pub(crate) fn set_state(&self, state: &str) {
        self.state_label.set_text(&format!("state: {state}"));
    }

    pub(crate) fn set_progress(&self, progress: &str) {
        self.progress_label
            .set_text(&format!("progress: {progress}"));
    }

    pub(crate) fn set_result(&self, result: &str) {
        self.result_label.set_text(&format!("result: {result}"));
    }

    pub(crate) fn reset_status(&self) {
        self.set_job_id(None);
        self.set_state("-");
        self.set_progress("-");
        self.set_result("-");
    }
}

#[derive(Clone)]
pub(crate) struct TargetsPage {
    pub(crate) page: Page,
    pub(crate) apk_entry: gtk::Entry,
    pub(crate) cuttlefish_build_entry: gtk::Entry,
    pub(crate) target_entry: gtk::Entry,
}

#[derive(Clone)]
pub(crate) struct ToolchainsPage {
    pub(crate) page: Page,
    pub(crate) sdk_version_combo: gtk::ComboBoxText,
    pub(crate) ndk_version_combo: gtk::ComboBoxText,
    pub(crate) active_set_entry: gtk::Entry,
}

#[derive(Clone)]
pub(crate) struct BuildPage {
    pub(crate) page: Page,
    pub(crate) project_entry: gtk::Entry,
}

#[derive(Clone)]
pub(crate) struct ProjectsPage {
    pub(crate) page: Page,
    pub(crate) template_combo: gtk::ComboBoxText,
    pub(crate) toolchain_set_combo: gtk::ComboBoxText,
    pub(crate) target_combo: gtk::ComboBoxText,
    pub(crate) project_id_entry: gtk::Entry,
}

#[derive(Clone)]
pub(crate) struct WorkflowPage {
    pub(crate) page: Page,
    pub(crate) run_id_entry: gtk::Entry,
    pub(crate) project_id_entry: gtk::Entry,
    pub(crate) project_path_entry: gtk::Entry,
    pub(crate) toolchain_set_entry: gtk::Entry,
    pub(crate) target_id_entry: gtk::Entry,
}

impl TargetsPage {
    pub(crate) fn append(&self, s: &str) {
        self.page.append(s);
    }

    pub(crate) fn set_target_id(&self, target_id: &str) {
        self.target_entry.set_text(target_id.trim());
    }

    pub(crate) fn set_apk_path(&self, path: &str) {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            self.apk_entry.set_text(trimmed);
        }
    }

    pub(crate) fn set_cuttlefish_build_id(&self, build_id: &str) {
        let trimmed = build_id.trim();
        if !trimmed.is_empty() {
            self.cuttlefish_build_entry.set_text(trimmed);
        }
    }
}

impl ToolchainsPage {
    pub(crate) fn append(&self, s: &str) {
        self.page.append(s);
    }

    pub(crate) fn set_active_set_id(&self, set_id: &str) {
        self.active_set_entry.set_text(set_id.trim());
    }

    pub(crate) fn set_available_versions(&self, provider_id: &str, versions: &[String]) {
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

impl BuildPage {
    pub(crate) fn append(&self, s: &str) {
        self.page.append(s);
    }

    pub(crate) fn set_project_ref(&self, project_ref: &str) {
        self.project_entry.set_text(project_ref.trim());
    }
}

impl ProjectsPage {
    pub(crate) fn append(&self, s: &str) {
        self.page.append(s);
    }

    pub(crate) fn set_templates(&self, templates: &[ProjectTemplateOption]) {
        self.template_combo.remove_all();
        for tmpl in templates {
            let label = format!("{} ({})", tmpl.name, tmpl.id);
            self.template_combo.append(Some(tmpl.id.as_str()), &label);
        }
        if !templates.is_empty() {
            self.template_combo.set_active(Some(0));
        }
    }

    pub(crate) fn set_toolchain_sets(&self, sets: &[ToolchainSetOption]) {
        self.toolchain_set_combo.remove_all();
        self.toolchain_set_combo.append(Some("none"), "None");
        for set in sets {
            self.toolchain_set_combo
                .append(Some(set.id.as_str()), &set.label);
        }
        self.toolchain_set_combo.set_active(Some(0));
    }

    pub(crate) fn set_targets(&self, targets: &[TargetOption]) {
        self.target_combo.remove_all();
        self.target_combo.append(Some("none"), "None");
        for target in targets {
            self.target_combo
                .append(Some(target.id.as_str()), &target.label);
        }
        self.target_combo.set_active(Some(0));
    }

    pub(crate) fn set_project_id(&self, project_id: &str) {
        if !project_id.trim().is_empty() {
            self.project_id_entry.set_text(project_id);
        }
    }

    pub(crate) fn set_active_context(&self, ctx: &ActiveContext) {
        let project_id = ctx.project_id.trim();
        if project_id.is_empty() {
            self.project_id_entry.set_text("");
        } else {
            self.project_id_entry.set_text(project_id);
        }

        let toolchain_set_id = ctx.toolchain_set_id.trim();
        if toolchain_set_id.is_empty() {
            self.toolchain_set_combo.set_active_id(Some("none"));
        } else {
            self.toolchain_set_combo
                .set_active_id(Some(toolchain_set_id));
        }

        let target_id = ctx.target_id.trim();
        if target_id.is_empty() {
            self.target_combo.set_active_id(Some("none"));
        } else {
            self.target_combo.set_active_id(Some(target_id));
        }
    }
}

impl WorkflowPage {
    pub(crate) fn append(&self, s: &str) {
        self.page.append(s);
    }

    pub(crate) fn set_context(&self, ctx: &ActiveContext) {
        self.run_id_entry.set_text(ctx.run_id.trim());
        self.project_id_entry.set_text(ctx.project_id.trim());
        self.project_path_entry.set_text(ctx.project_path.trim());
        self.toolchain_set_entry
            .set_text(ctx.toolchain_set_id.trim());
        self.target_id_entry.set_text(ctx.target_id.trim());
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

fn set_tooltip<W: gtk::prelude::IsA<gtk::Widget>>(widget: &W, text: &str) {
    widget.set_tooltip_text(Some(text));
}

fn make_page(title: &str, description: &str, connections: &str) -> Page {
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

    let description_label = gtk::Label::builder()
        .label(description)
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["dim-label"])
        .build();

    let connections_label = gtk::Label::builder()
        .label(connections)
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["dim-label"])
        .build();

    let intro = gtk::Box::new(gtk::Orientation::Vertical, 4);
    intro.append(&header);
    intro.append(&description_label);
    intro.append(&connections_label);

    let log_scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .build();

    let textview = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .build();

    let buffer = textview.buffer();
    log_scroller.set_child(Some(&textview));

    container.append(&intro);
    container.append(&log_scroller);

    let root = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    root.set_child(Some(&container));

    Page {
        root,
        container,
        intro,
        buffer,
        textview,
    }
}

const KNOWN_JOB_TYPES: &[&str] = &[
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

pub(crate) fn page_home(
    cfg: Arc<std::sync::Mutex<AppConfig>>,
    cmd_tx: mpsc::Sender<UiCommand>,
) -> HomePage {
    let page = make_page(
        "Job Control - Start and monitor jobs",
        "Overview: Start and watch jobs across the system. Use this page to kick off any JobService job with parameters, project/target/toolchain ids, and an optional correlation id.",
        "Connections: Jobs started here appear in Job History. Use Projects, Toolchains, Targets, and Build to gather ids and inputs; use Evidence to export run bundles; Settings controls service addresses.",
    );
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
    set_tooltip(&job_type_entry, "What: Job type string sent to JobService (for example build.run or workflow.pipeline). Why: JobService uses this to route the request to the right service. How: type a known job type or pick one from the dropdown to fill this field.");
    set_tooltip(&job_type_combo, "What: Known job types this UI is aware of. Why: helps avoid typos and discover common workflows. How: select one to copy it into the Job type field.");

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
    set_tooltip(&params_view, "What: Job parameters as key=value lines. Why: many jobs require additional inputs beyond ids. How: enter one key=value per line; omit '=' to send a key with an empty value.");

    let project_id_label = gtk::Label::builder()
        .label("Project id")
        .xalign(0.0)
        .build();
    let project_id_entry = gtk::Entry::builder()
        .placeholder_text("optional project id")
        .hexpand(true)
        .build();
    set_tooltip(&project_id_entry, "What: Project id to attach to the job. Why: lets services resolve a project without a filesystem path. How: copy an id from the Projects tab or Job History.");

    let target_id_label = gtk::Label::builder().label("Target id").xalign(0.0).build();
    let target_id_entry = gtk::Entry::builder()
        .placeholder_text("optional target id")
        .hexpand(true)
        .build();
    set_tooltip(&target_id_entry, "What: Target id or adb serial to attach to the job. Why: target-aware jobs use it to route device actions. How: copy from Targets or Job History.");

    let toolchain_id_label = gtk::Label::builder()
        .label("Toolchain set id")
        .xalign(0.0)
        .build();
    let toolchain_id_entry = gtk::Entry::builder()
        .placeholder_text("optional toolchain set id")
        .hexpand(true)
        .build();
    set_tooltip(&toolchain_id_entry, "What: Toolchain set id to attach to the job. Why: build and project workflows can use it to select SDK/NDK. How: copy from Toolchains or Projects.");
    let correlation_id_label = gtk::Label::builder()
        .label("Correlation id")
        .xalign(0.0)
        .build();
    let correlation_id_entry = gtk::Entry::builder()
        .placeholder_text("optional correlation id")
        .hexpand(true)
        .build();
    set_tooltip(&correlation_id_entry, "What: Correlation id to group multiple jobs into a run. Why: ObserveService uses it to derive run_id for evidence bundles. How: set a stable string and reuse it across related jobs.");

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
    set_tooltip(&start_btn, "What: Start the job with the provided fields. Why: submits to JobService and begins streaming events. How: fill inputs then click.");
    set_tooltip(&cancel_btn, "What: Cancel the current tracked job. Why: stop a long running job from Job Control. How: click after a job is started or watched.");
    set_tooltip(&watch_entry, "What: Job id to watch. Why: stream logs and status for an existing job. How: paste a job id from Job History or other tabs.");
    set_tooltip(&watch_btn, "What: Start streaming events for the job id. Why: see live progress without starting a new job. How: enter a job id and click.");

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
    let progress_label = gtk::Label::builder()
        .label("progress: -")
        .xalign(0.0)
        .build();
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
        .insert_child_after(&controls, Some(&page.intro));

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
            params_view.buffer().set_text(&cfg.last_job_params);
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
            .try_send(UiCommand::HomeStartJob {
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
            .try_send(UiCommand::HomeWatchJob { cfg, job_id })
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

pub(crate) fn page_workflow(
    cfg: Arc<std::sync::Mutex<AppConfig>>,
    cmd_tx: mpsc::Sender<UiCommand>,
    parent: &gtk::ApplicationWindow,
) -> WorkflowPage {
    let page = make_page(
        "Workflow - Pipeline orchestration",
        "Overview: Run workflow.pipeline with explicit step inputs and watch run-level events as the pipeline fans out across services.",
        "Connections: WorkflowService creates a pipeline job and delegates to Project/Toolchain/Build/Targets/Observe. Run streams come from JobService. Settings controls the WorkflowService address.",
    );

    let controls = gtk::Box::new(gtk::Orientation::Vertical, 8);

    let identity_frame = gtk::Frame::builder().label("Run identity").build();
    let identity_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let run_id_entry = gtk::Entry::builder()
        .placeholder_text("optional run id")
        .hexpand(true)
        .build();
    let correlation_id_entry = gtk::Entry::builder()
        .placeholder_text("optional correlation id")
        .hexpand(true)
        .build();
    let use_job_id_check = gtk::CheckButton::with_label("Use job id");
    let job_id_entry = gtk::Entry::builder()
        .placeholder_text("job id")
        .hexpand(true)
        .build();
    let include_history_check = gtk::CheckButton::with_label("Include run history");
    include_history_check.set_active(true);

    set_tooltip(&run_id_entry, "What: Optional RunId to assign to the pipeline. Why: use a stable id for run dashboards. How: enter a custom id or leave blank to auto-generate.");
    set_tooltip(&correlation_id_entry, "What: Correlation id for grouping jobs. Why: helps group pipeline jobs with other work. How: set a stable string to reuse across related jobs.");
    set_tooltip(&use_job_id_check, "What: Reuse an existing job id instead of creating a new pipeline job. Why: attach pipeline output to a known job stream. How: enable and enter a job id.");
    set_tooltip(&job_id_entry, "What: Existing job id for pipeline. Why: attach pipeline results to a known job. How: paste a job id from Job Control or Job History.");
    set_tooltip(&include_history_check, "What: Include existing run history in the stream. Why: show earlier events when attaching to an existing run. How: enable to replay history before live events.");

    identity_grid.attach(&gtk::Label::new(Some("Run id")), 0, 0, 1, 1);
    identity_grid.attach(&run_id_entry, 1, 0, 1, 1);
    identity_grid.attach(&gtk::Label::new(Some("Correlation id")), 0, 1, 1, 1);
    identity_grid.attach(&correlation_id_entry, 1, 1, 1, 1);
    identity_grid.attach(&use_job_id_check, 0, 2, 1, 1);
    identity_grid.attach(&job_id_entry, 1, 2, 1, 1);
    identity_grid.attach(&include_history_check, 1, 3, 1, 1);

    identity_frame.set_child(Some(&identity_grid));
    controls.append(&identity_frame);

    let project_frame = gtk::Frame::builder().label("Project + Toolchain").build();
    let project_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let template_id_entry = gtk::Entry::builder()
        .placeholder_text("template id (e.g. tmpl-sample-console)")
        .hexpand(true)
        .build();
    let project_path_entry = gtk::Entry::builder()
        .placeholder_text("project path (create/open)")
        .hexpand(true)
        .build();
    let project_browse = gtk::Button::with_label("Browse...");
    let project_name_entry = gtk::Entry::builder()
        .placeholder_text("project name (optional)")
        .hexpand(true)
        .build();
    let project_id_entry = gtk::Entry::builder()
        .placeholder_text("project id (optional)")
        .hexpand(true)
        .build();
    let toolchain_id_entry = gtk::Entry::builder()
        .placeholder_text("toolchain id (verify)")
        .hexpand(true)
        .build();
    let toolchain_set_entry = gtk::Entry::builder()
        .placeholder_text("toolchain set id (build/project)")
        .hexpand(true)
        .build();
    let target_id_entry = gtk::Entry::builder()
        .placeholder_text("target id (adb serial)")
        .hexpand(true)
        .build();

    set_tooltip(&template_id_entry, "What: Project template id for create steps. Why: create_project needs a template id. How: copy from Projects or registry.");
    set_tooltip(&project_path_entry, "What: Project path for create/open steps. Why: project.create/open needs a filesystem path. How: enter a path or use Browse.");
    set_tooltip(
        &project_browse,
        "What: Pick a project folder. Why: avoid typing full paths. How: select a folder.",
    );
    set_tooltip(&project_name_entry, "What: Project name for create. Why: create_project needs a name. How: enter a friendly name or leave blank to infer.");
    set_tooltip(&project_id_entry, "What: Existing project id. Why: build steps can use an id instead of a path. How: paste from Projects or Job History.");
    set_tooltip(&toolchain_id_entry, "What: Toolchain id to verify. Why: toolchain.verify step needs an installed toolchain id. How: copy from Toolchains or Job History.");
    set_tooltip(&toolchain_set_entry, "What: Toolchain set id for build/project steps. Why: pipeline passes this to project.create and build. How: copy from Toolchains or Projects.");
    set_tooltip(&target_id_entry, "What: Target id/adb serial for install/launch. Why: target steps need a device. How: copy from Targets.");

    project_grid.attach(&gtk::Label::new(Some("Template id")), 0, 0, 1, 1);
    project_grid.attach(&template_id_entry, 1, 0, 1, 1);
    project_grid.attach(&gtk::Label::new(Some("Project path")), 0, 1, 1, 1);
    let path_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    path_row.append(&project_path_entry);
    path_row.append(&project_browse);
    project_grid.attach(&path_row, 1, 1, 1, 1);
    project_grid.attach(&gtk::Label::new(Some("Project name")), 0, 2, 1, 1);
    project_grid.attach(&project_name_entry, 1, 2, 1, 1);
    project_grid.attach(&gtk::Label::new(Some("Project id")), 0, 3, 1, 1);
    project_grid.attach(&project_id_entry, 1, 3, 1, 1);
    project_grid.attach(&gtk::Label::new(Some("Toolchain id")), 0, 4, 1, 1);
    project_grid.attach(&toolchain_id_entry, 1, 4, 1, 1);
    project_grid.attach(&gtk::Label::new(Some("Toolchain set id")), 0, 5, 1, 1);
    project_grid.attach(&toolchain_set_entry, 1, 5, 1, 1);
    project_grid.attach(&gtk::Label::new(Some("Target id")), 0, 6, 1, 1);
    project_grid.attach(&target_id_entry, 1, 6, 1, 1);

    project_frame.set_child(Some(&project_grid));
    controls.append(&project_frame);

    let build_frame = gtk::Frame::builder().label("Build + Launch").build();
    let build_grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let variant_combo = gtk::DropDown::from_strings(&["debug", "release"]);
    variant_combo.set_selected(0);
    let variant_name_entry = gtk::Entry::builder()
        .placeholder_text("variant name override (e.g. demoDebug)")
        .hexpand(true)
        .build();
    let module_entry = gtk::Entry::builder()
        .placeholder_text("module (e.g. app or :app)")
        .hexpand(true)
        .build();
    let tasks_entry = gtk::Entry::builder()
        .placeholder_text("tasks (comma/space separated)")
        .hexpand(true)
        .build();
    let apk_path_entry = gtk::Entry::builder()
        .placeholder_text("apk path for install")
        .hexpand(true)
        .build();
    let application_id_entry = gtk::Entry::builder()
        .placeholder_text("application id (com.example.app)")
        .hexpand(true)
        .build();
    let activity_entry = gtk::Entry::builder()
        .placeholder_text("activity (optional)")
        .hexpand(true)
        .build();

    set_tooltip(&variant_combo, "What: Base build variant. Why: used when Variant name is empty. How: choose debug or release.");
    set_tooltip(&variant_name_entry, "What: Explicit variant name override. Why: override debug/release with a custom variant. How: enter demoDebug or similar.");
    set_tooltip(
        &module_entry,
        "What: Gradle module. Why: limit build to a module. How: enter app or :app.",
    );
    set_tooltip(&tasks_entry, "What: Explicit Gradle tasks. Why: override default tasks. How: enter space or comma separated tasks.");
    set_tooltip(&apk_path_entry, "What: APK path for install step. Why: targets.install needs an APK. How: paste a path or leave empty to skip install.");
    set_tooltip(&application_id_entry, "What: Application id for launch. Why: targets.launch uses it to start the app. How: enter com.example.app.");
    set_tooltip(&activity_entry, "What: Optional activity for launch. Why: open a specific activity. How: enter the full activity class or leave blank.");

    build_grid.attach(&gtk::Label::new(Some("Variant")), 0, 0, 1, 1);
    build_grid.attach(&variant_combo, 1, 0, 1, 1);
    build_grid.attach(&gtk::Label::new(Some("Variant name")), 0, 1, 1, 1);
    build_grid.attach(&variant_name_entry, 1, 1, 1, 1);
    build_grid.attach(&gtk::Label::new(Some("Module")), 0, 2, 1, 1);
    build_grid.attach(&module_entry, 1, 2, 1, 1);
    build_grid.attach(&gtk::Label::new(Some("Tasks")), 0, 3, 1, 1);
    build_grid.attach(&tasks_entry, 1, 3, 1, 1);
    build_grid.attach(&gtk::Label::new(Some("APK path")), 0, 4, 1, 1);
    build_grid.attach(&apk_path_entry, 1, 4, 1, 1);
    build_grid.attach(&gtk::Label::new(Some("Application id")), 0, 5, 1, 1);
    build_grid.attach(&application_id_entry, 1, 5, 1, 1);
    build_grid.attach(&gtk::Label::new(Some("Activity")), 0, 6, 1, 1);
    build_grid.attach(&activity_entry, 1, 6, 1, 1);

    build_frame.set_child(Some(&build_grid));
    controls.append(&build_frame);

    let steps_frame = gtk::Frame::builder().label("Pipeline steps").build();
    let steps_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let auto_infer_check = gtk::CheckButton::with_label("Auto-infer steps from inputs");
    auto_infer_check.set_active(true);
    set_tooltip(&auto_infer_check, "What: Let the pipeline infer which steps to run. Why: reduces manual toggles. How: leave enabled to infer steps from the filled inputs.");

    let create_check = gtk::CheckButton::with_label("Create project");
    let open_check = gtk::CheckButton::with_label("Open project");
    let verify_check = gtk::CheckButton::with_label("Verify toolchain");
    let build_check = gtk::CheckButton::with_label("Build");
    let install_check = gtk::CheckButton::with_label("Install APK");
    let launch_check = gtk::CheckButton::with_label("Launch app");
    let support_check = gtk::CheckButton::with_label("Export support bundle");
    let evidence_check = gtk::CheckButton::with_label("Export evidence bundle");

    set_tooltip(&create_check, "What: Run project.create. Why: scaffold a project before build. How: enable when you want to create from a template.");
    set_tooltip(&open_check, "What: Run project.open. Why: open an existing project. How: enable when you have a project path.");
    set_tooltip(&verify_check, "What: Run toolchain.verify. Why: ensure SDK/NDK installs are valid. How: enable to verify a toolchain id.");
    set_tooltip(
        &build_check,
        "What: Run build.run. Why: produce APKs/AABs. How: enable when you have a project id/path.",
    );
    set_tooltip(&install_check, "What: Run targets.install. Why: install APK on a device. How: enable with target id and apk path.");
    set_tooltip(&launch_check, "What: Run targets.launch. Why: start the app on the target. How: enable with target id and application id.");
    set_tooltip(&support_check, "What: Run observe.support_bundle. Why: export a support bundle after the run. How: enable to capture logs and config.");
    set_tooltip(&evidence_check, "What: Run observe.evidence_bundle. Why: export a run-specific evidence bundle. How: enable and set Run id.");

    let step_row_a = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    step_row_a.append(&create_check);
    step_row_a.append(&open_check);
    step_row_a.append(&verify_check);
    step_row_a.append(&build_check);
    let step_row_b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    step_row_b.append(&install_check);
    step_row_b.append(&launch_check);
    step_row_b.append(&support_check);
    step_row_b.append(&evidence_check);

    steps_box.append(&auto_infer_check);
    steps_box.append(&step_row_a);
    steps_box.append(&step_row_b);
    steps_frame.set_child(Some(&steps_box));
    controls.append(&steps_frame);

    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let run_btn = gtk::Button::with_label("Run pipeline");
    let stream_btn = gtk::Button::with_label("Stream run events");
    set_tooltip(&run_btn, "What: Start the workflow pipeline. Why: orchestrate multi-service steps in order. How: fill inputs and click.");
    set_tooltip(&stream_btn, "What: Stream run-level events. Why: watch pipeline progress across jobs. How: enter run id or correlation id and click.");
    action_row.append(&run_btn);
    action_row.append(&stream_btn);
    controls.append(&action_row);

    page.container
        .insert_child_after(&controls, Some(&page.intro));

    {
        let cfg = cfg.lock().unwrap().clone();
        if !cfg.last_job_id.is_empty() {
            job_id_entry.set_text(&cfg.last_job_id);
        }
        if !cfg.last_correlation_id.is_empty() {
            correlation_id_entry.set_text(&cfg.last_correlation_id);
        }
    }

    let parent_window = parent.clone();
    let project_path_entry_browse = project_path_entry.clone();
    project_browse.connect_clicked(move |_| {
        let dialog = gtk::FileChooserNative::new(
            Some("Select Project Folder"),
            Some(&parent_window),
            gtk::FileChooserAction::SelectFolder,
            Some("Open"),
            Some("Cancel"),
        );

        let current = project_path_entry_browse.text().to_string();
        if !current.trim().is_empty() {
            let folder = gtk::gio::File::for_path(current.trim());
            let _ = dialog.set_current_folder(Some(&folder));
        }

        let project_entry_dialog = project_path_entry_browse.clone();
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
    let run_id_entry_run = run_id_entry.clone();
    let correlation_id_entry_run = correlation_id_entry.clone();
    let use_job_id_run = use_job_id_check.clone();
    let job_id_entry_run = job_id_entry.clone();
    let include_history_run = include_history_check.clone();
    let project_id_entry_run = project_id_entry.clone();
    let project_path_entry_run = project_path_entry.clone();
    let project_name_entry_run = project_name_entry.clone();
    let template_id_entry_run = template_id_entry.clone();
    let toolchain_id_entry_run = toolchain_id_entry.clone();
    let toolchain_set_entry_run = toolchain_set_entry.clone();
    let target_id_entry_run = target_id_entry.clone();
    let variant_combo_run = variant_combo.clone();
    let variant_name_entry_run = variant_name_entry.clone();
    let module_entry_run = module_entry.clone();
    let tasks_entry_run = tasks_entry.clone();
    let apk_path_entry_run = apk_path_entry.clone();
    let application_id_entry_run = application_id_entry.clone();
    let activity_entry_run = activity_entry.clone();
    let auto_infer_run = auto_infer_check.clone();
    let create_check_run = create_check.clone();
    let open_check_run = open_check.clone();
    let verify_check_run = verify_check.clone();
    let build_check_run = build_check.clone();
    let install_check_run = install_check.clone();
    let launch_check_run = launch_check.clone();
    let support_check_run = support_check.clone();
    let evidence_check_run = evidence_check.clone();
    run_btn.connect_clicked(move |_| {
        let job_id_raw = job_id_entry_run.text().to_string();
        let correlation_id = correlation_id_entry_run.text().to_string();
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
        let run_id = run_id_entry_run.text().to_string();
        let project_id = project_id_entry_run.text().to_string();
        let project_path = project_path_entry_run.text().to_string();
        let project_name = project_name_entry_run.text().to_string();
        let template_id = template_id_entry_run.text().to_string();
        let toolchain_id = toolchain_id_entry_run.text().to_string();
        let toolchain_set_id = toolchain_set_entry_run.text().to_string();
        let target_id = target_id_entry_run.text().to_string();
        let build_variant = match variant_combo_run.selected() {
            1 => BuildVariant::Release,
            _ => BuildVariant::Debug,
        };
        let variant_name = variant_name_entry_run.text().to_string();
        let module = module_entry_run.text().to_string();
        let tasks = parse_list_tokens(&tasks_entry_run.text());
        let apk_path = apk_path_entry_run.text().to_string();
        let application_id = application_id_entry_run.text().to_string();
        let activity = activity_entry_run.text().to_string();
        let options = if auto_infer_run.is_active() {
            None
        } else {
            Some(WorkflowPipelineOptions {
                verify_toolchain: verify_check_run.is_active(),
                create_project: create_check_run.is_active(),
                open_project: open_check_run.is_active(),
                build: build_check_run.is_active(),
                install_apk: install_check_run.is_active(),
                launch_app: launch_check_run.is_active(),
                export_support_bundle: support_check_run.is_active(),
                export_evidence_bundle: evidence_check_run.is_active(),
            })
        };

        cmd_tx_run
            .try_send(UiCommand::WorkflowRunPipeline {
                cfg,
                run_id,
                correlation_id,
                job_id,
                project_id,
                project_path,
                project_name,
                template_id,
                toolchain_id,
                toolchain_set_id,
                target_id,
                build_variant,
                module,
                variant_name,
                tasks,
                apk_path,
                application_id,
                activity,
                options,
                stream_history: include_history_run.is_active(),
            })
            .ok();
    });

    let cfg_stream = cfg.clone();
    let cmd_tx_stream = cmd_tx.clone();
    let run_id_entry_stream = run_id_entry.clone();
    let correlation_id_entry_stream = correlation_id_entry.clone();
    let include_history_stream = include_history_check.clone();
    stream_btn.connect_clicked(move |_| {
        let cfg = cfg_stream.lock().unwrap().clone();
        cmd_tx_stream
            .try_send(UiCommand::StreamRunEvents {
                cfg,
                run_id: run_id_entry_stream.text().to_string(),
                correlation_id: correlation_id_entry_stream.text().to_string(),
                include_history: include_history_stream.is_active(),
                page: "workflow",
            })
            .ok();
    });

    WorkflowPage {
        page,
        run_id_entry,
        project_id_entry,
        project_path_entry,
        toolchain_set_entry,
        target_id_entry,
    }
}

pub(crate) fn page_jobs_history(
    cfg: Arc<std::sync::Mutex<AppConfig>>,
    cmd_tx: mpsc::Sender<UiCommand>,
) -> Page {
    let page = make_page(
        "Job History - JobService query and exports",
        "Overview: Query JobService for jobs and event history with filters, and export logs to JSON for sharing or troubleshooting.",
        "Connections: Job Control, Workflow, Toolchains, Projects, Targets, Build, and Evidence create jobs that show up here. Use job ids and correlation ids from this tab when watching jobs or exporting Evidence. Settings changes the JobService endpoint.",
    );
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
    set_tooltip(&job_types_entry, "What: Comma/space list of job types to include. Why: narrows the job list. How: enter values like build.run workflow.pipeline.");
    set_tooltip(&states_entry, "What: Job state filter. Why: focus on failures or running jobs. How: use queued,running,success,failed,cancelled (case-insensitive).");
    set_tooltip(&created_after_entry, "What: Lower bound for job creation time in unix millis. Why: limit results to recent jobs. How: paste an epoch millis value.");
    set_tooltip(&created_before_entry, "What: Upper bound for job creation time in unix millis. Why: limit results to older jobs. How: paste an epoch millis value.");
    set_tooltip(&finished_after_entry, "What: Lower bound for job finish time in unix millis. Why: filter completed jobs by finish time. How: paste an epoch millis value.");
    set_tooltip(&finished_before_entry, "What: Upper bound for job finish time in unix millis. Why: filter completed jobs by finish time. How: paste an epoch millis value.");
    set_tooltip(&correlation_id_entry, "What: Correlation id to match. Why: group jobs by run or workflow. How: paste a correlation id from Job Control or Job History.");
    set_tooltip(&page_size_entry, "What: Maximum jobs per page. Why: control result size and output volume. How: enter an integer, for example 50.");
    set_tooltip(&page_token_entry, "What: Pagination token from a previous response. Why: continue listing from where you left off. How: paste the token string.");
    set_tooltip(
        &list_btn,
        "What: List jobs using the filters. Why: query JobService. How: set filters and click.",
    );

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
    set_tooltip(&job_id_entry, "What: Job id to fetch event history for. Why: history is per job. How: paste a job id from the list or Job Control.");
    set_tooltip(&kinds_entry, "What: Event kinds to include. Why: focus on state, progress, or log events. How: use state,progress,log,completed,failed.");
    set_tooltip(&after_entry, "What: Lower bound for event timestamps in unix millis. Why: limit history size. How: paste an epoch millis value.");
    set_tooltip(&before_entry, "What: Upper bound for event timestamps in unix millis. Why: limit history size. How: paste an epoch millis value.");
    set_tooltip(&history_page_size_entry, "What: Maximum events per page. Why: control output volume. How: enter an integer, for example 200.");
    set_tooltip(&history_page_token_entry, "What: Pagination token for event history. Why: continue listing events. How: paste the token string.");
    set_tooltip(&output_path_entry, "What: Optional output path for exported logs. Why: save logs to a specific file. How: leave blank to use the default export path.");
    set_tooltip(&list_history_btn, "What: List event history for a job. Why: review the full timeline. How: enter a job id and click.");
    set_tooltip(&export_btn, "What: Export job logs as JSON. Why: share or archive logs. How: enter a job id and optional output path, then click.");

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
        .insert_child_after(&controls, Some(&page.intro));

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
            .try_send(UiCommand::JobsList {
                cfg,
                job_types: job_types_entry_list.text().to_string(),
                states: states_entry_list.text().to_string(),
                created_after: created_after_entry_list.text().to_string(),
                created_before: created_before_entry_list.text().to_string(),
                finished_after: finished_after_entry_list.text().to_string(),
                finished_before: finished_before_entry_list.text().to_string(),
                correlation_id: correlation_id_entry_list.text().to_string(),
                run_id: String::new(),
                page_size,
                page_token: page_token_entry_list.text().to_string(),
                page: "jobs",
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
        let page_size = history_page_size_entry_history
            .text()
            .parse::<u32>()
            .unwrap_or(200);
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
            .try_send(UiCommand::JobsHistory {
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
            .try_send(UiCommand::JobsExportLogs {
                cfg,
                job_id,
                output_path: output_path_entry_export.text().to_string(),
                page: "jobs",
            })
            .ok();
    });

    page
}

const PROVIDER_SDK_ID: &str = "provider-android-sdk-custom";
const PROVIDER_NDK_ID: &str = "provider-android-ndk-custom";
const SDK_VERSION: &str = "36.0.0";
const NDK_VERSION: &str = "r29";

pub(crate) fn page_toolchains(
    cfg: Arc<std::sync::Mutex<AppConfig>>,
    cmd_tx: mpsc::Sender<UiCommand>,
) -> ToolchainsPage {
    let page = make_page(
        "Toolchains - SDK/NDK management and sets",
        "Overview: Discover providers, list available versions, install or verify SDK/NDK toolchains, and manage toolchain sets.",
        "Connections: Projects can point at toolchain sets; Build runs use installed toolchains; Toolchain jobs stream in Job Control and Job History; Evidence bundles can include toolchain provenance; Settings controls ToolchainService address.",
    );
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
    set_tooltip(&use_job_id_check, "What: Reuse an existing job id instead of creating a new job. Why: attach this toolchain action to an existing job stream. How: enable it and fill Job id below.");
    set_tooltip(&job_id_entry, "What: Existing job id to attach or export. Why: stream results into a known job or export its logs. How: paste from Job Control or Job History.");
    set_tooltip(&correlation_id_entry, "What: Correlation id to group toolchain work into a run. Why: enables Observe run tracking across services. How: set a stable string and reuse it.");
    job_grid.attach(&use_job_id_check, 0, 0, 1, 1);
    job_grid.attach(&job_id_entry, 1, 0, 1, 1);
    job_grid.attach(&gtk::Label::new(Some("Correlation id")), 0, 1, 1, 1);
    job_grid.attach(&correlation_id_entry, 1, 1, 1, 1);

    let row1 = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let list = gtk::Button::with_label("List providers");
    let list_sdk = gtk::Button::with_label("List available SDKs");
    let list_ndk = gtk::Button::with_label("List available NDKs");
    let list_sets = gtk::Button::with_label("List sets");
    set_tooltip(&list, "What: List toolchain providers. Why: discover provider ids and supported kinds. How: click to query ToolchainService.");
    set_tooltip(&list_sdk, "What: List available SDK versions. Why: populate the SDK version dropdown. How: click then select a version.");
    set_tooltip(&list_ndk, "What: List available NDK versions. Why: populate the NDK version dropdown. How: click then select a version.");
    set_tooltip(&list_sets, "What: List toolchain sets. Why: see existing SDK/NDK pairings. How: click to query ToolchainService.");
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
    set_tooltip(&sdk_version_combo, "What: SDK version to install. Why: install requires an explicit version. How: select a version from the available list.");
    set_tooltip(&ndk_version_combo, "What: NDK version to install. Why: install requires an explicit version. How: select a version from the available list.");
    let label_sdk_version = gtk::Label::builder()
        .label("SDK version")
        .xalign(0.0)
        .build();
    let label_ndk_version = gtk::Label::builder()
        .label("NDK version")
        .xalign(0.0)
        .build();
    version_grid.attach(&label_sdk_version, 0, 0, 1, 1);
    version_grid.attach(&sdk_version_combo, 1, 0, 1, 1);
    version_grid.attach(&label_ndk_version, 0, 1, 1, 1);
    version_grid.attach(&ndk_version_combo, 1, 1, 1, 1);

    let row2 = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let install_sdk = gtk::Button::with_label("Install SDK");
    let install_ndk = gtk::Button::with_label("Install NDK");
    let list_installed = gtk::Button::with_label("List installed");
    let verify_installed = gtk::Button::with_label("Verify installed");
    set_tooltip(&install_sdk, "What: Install the selected SDK version. Why: makes the SDK available for builds. How: pick a version, set optional job/correlation ids, then click.");
    set_tooltip(&install_ndk, "What: Install the selected NDK version. Why: builds with native code require the NDK. How: pick a version, set optional job/correlation ids, then click.");
    set_tooltip(&list_installed, "What: List installed toolchains. Why: verify what is already available. How: click to query ToolchainService.");
    set_tooltip(&verify_installed, "What: Verify installed toolchains against expected hashes. Why: detect corruption or mismatches. How: click to start a verification job.");
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
    set_tooltip(&toolchain_id_entry, "What: Toolchain id to update or uninstall. Why: target a specific installed toolchain. How: paste an id from the installed list output.");
    set_tooltip(&update_version_entry, "What: Target version to update to. Why: upgrade or switch the toolchain version. How: enter a version string.");
    set_tooltip(&verify_update_check, "What: Verify hashes after update. Why: ensure downloaded artifacts are intact. How: leave checked for safety.");
    set_tooltip(&remove_cached_check, "What: Remove cached artifact during update/uninstall. Why: force a fresh download or free space. How: check to delete cache entries.");
    set_tooltip(&force_uninstall_check, "What: Force uninstall even if in use. Why: clean up stubborn installs. How: check only if you know it is safe.");
    set_tooltip(&update_btn, "What: Update the specified toolchain. Why: move to a newer version. How: fill Toolchain id and Target version, set options, then click.");
    set_tooltip(&uninstall_btn, "What: Uninstall the specified toolchain. Why: remove unused versions. How: fill Toolchain id and options, then click.");
    set_tooltip(&cleanup_btn, "What: Clean cached downloads. Why: reclaim disk space or reset downloads. How: set dry run/remove all options and click.");
    set_tooltip(&dry_run_check, "What: Dry run for cache cleanup. Why: preview what would be deleted. How: leave checked to inspect output first.");
    set_tooltip(&remove_all_check, "What: Remove all cached artifacts. Why: full cache reset. How: check only when you want a complete purge.");

    let label_toolchain_id = gtk::Label::builder()
        .label("Toolchain id")
        .xalign(0.0)
        .build();
    let label_update_version = gtk::Label::builder()
        .label("Target version")
        .xalign(0.0)
        .build();
    let label_cache = gtk::Label::builder()
        .label("Cache cleanup")
        .xalign(0.0)
        .build();

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
    let create_set_latest_btn = gtk::Button::with_label("Use latest installed");
    let active_set_entry = gtk::Entry::builder()
        .placeholder_text("Active toolchain set id")
        .hexpand(true)
        .build();
    let set_active_btn = gtk::Button::with_label("Set active");
    let get_active_btn = gtk::Button::with_label("Get active");
    set_tooltip(&sdk_set_entry, "What: SDK toolchain id for the set. Why: sets tie specific SDK/NDK versions together. How: paste an SDK toolchain id.");
    set_tooltip(&ndk_set_entry, "What: NDK toolchain id for the set. Why: sets tie specific SDK/NDK versions together. How: paste an NDK toolchain id.");
    set_tooltip(&display_name_entry, "What: Display name for the toolchain set. Why: human-friendly label used in other tabs. How: type a short descriptive name.");
    set_tooltip(&create_set_btn, "What: Create a new toolchain set. Why: use a consistent SDK+NDK pair in projects and builds. How: fill ids and name, then click.");
    set_tooltip(&create_set_latest_btn, "What: Create and activate a toolchain set from the most recently installed SDK+NDK. Why: minimize clicks after installing toolchains. How: install SDK/NDK, optionally set a display name, then click.");
    set_tooltip(&active_set_entry, "What: Toolchain set id to mark active. Why: projects can use the active set by default. How: paste a set id and click Set active.");
    set_tooltip(&set_active_btn, "What: Set the active toolchain set. Why: update default toolchain selection. How: enter a set id and click.");
    set_tooltip(&get_active_btn, "What: Fetch the current active toolchain set. Why: confirm the default toolchain selection. How: click to query ToolchainService.");

    let label_sdk_set = gtk::Label::builder().label("SDK id").xalign(0.0).build();
    let label_ndk_set = gtk::Label::builder().label("NDK id").xalign(0.0).build();
    let label_display = gtk::Label::builder()
        .label("Display name")
        .xalign(0.0)
        .build();
    let label_active = gtk::Label::builder()
        .label("Active set")
        .xalign(0.0)
        .build();

    set_grid.attach(&label_sdk_set, 0, 0, 1, 1);
    set_grid.attach(&sdk_set_entry, 1, 0, 1, 1);
    set_grid.attach(&label_ndk_set, 0, 1, 1, 1);
    set_grid.attach(&ndk_set_entry, 1, 1, 1, 1);
    set_grid.attach(&label_display, 0, 2, 1, 1);
    set_grid.attach(&display_name_entry, 1, 2, 1, 1);
    let set_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    set_actions.append(&create_set_btn);
    set_actions.append(&create_set_latest_btn);
    set_grid.attach(&set_actions, 1, 3, 1, 1);

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
    page.container
        .insert_child_after(&actions, Some(&page.intro));

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
        cmd_tx_list
            .try_send(UiCommand::ToolchainListProviders { cfg })
            .ok();
    });

    let cfg_sdk = cfg.clone();
    let cmd_tx_sdk = cmd_tx.clone();
    list_sdk.connect_clicked(move |_| {
        let cfg = cfg_sdk.lock().unwrap().clone();
        cmd_tx_sdk
            .try_send(UiCommand::ToolchainListAvailable {
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
            .try_send(UiCommand::ToolchainListAvailable {
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
            .try_send(UiCommand::ToolchainListSets { cfg })
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
            .try_send(UiCommand::ToolchainInstall {
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
            .try_send(UiCommand::ToolchainInstall {
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
            .try_send(UiCommand::ToolchainListInstalled {
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
            .try_send(UiCommand::ToolchainVerifyInstalled {
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
            .try_send(UiCommand::ToolchainUpdate {
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
            .try_send(UiCommand::ToolchainUninstall {
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
            .try_send(UiCommand::ToolchainCleanupCache {
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
            .try_send(UiCommand::ToolchainCreateSet {
                cfg,
                sdk_toolchain_id: if sdk_id.trim().is_empty() {
                    None
                } else {
                    Some(sdk_id)
                },
                ndk_toolchain_id: if ndk_id.trim().is_empty() {
                    None
                } else {
                    Some(ndk_id)
                },
                display_name,
            })
            .ok();
    });

    let cfg_create_latest = cfg.clone();
    let cmd_tx_create_latest = cmd_tx.clone();
    let display_entry_latest = display_name_entry.clone();
    create_set_latest_btn.connect_clicked(move |_| {
        let cfg = cfg_create_latest.lock().unwrap().clone();
        cmd_tx_create_latest
            .try_send(UiCommand::ToolchainCreateActiveLatest {
                cfg,
                display_name: display_entry_latest.text().to_string(),
            })
            .ok();
    });

    let cfg_set_active = cfg.clone();
    let cmd_tx_set_active = cmd_tx.clone();
    let active_entry_set = active_set_entry.clone();
    set_active_btn.connect_clicked(move |_| {
        let cfg = cfg_set_active.lock().unwrap().clone();
        cmd_tx_set_active
            .try_send(UiCommand::ToolchainSetActive {
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
            .try_send(UiCommand::ToolchainGetActive { cfg })
            .ok();
    });
    {
        let cfg = cfg.lock().unwrap().clone();
        cmd_tx
            .try_send(UiCommand::ToolchainListAvailable {
                cfg: cfg.clone(),
                provider_id: PROVIDER_SDK_ID.into(),
            })
            .ok();
        cmd_tx
            .try_send(UiCommand::ToolchainListAvailable {
                cfg,
                provider_id: PROVIDER_NDK_ID.into(),
            })
            .ok();
    }
    ToolchainsPage {
        page,
        sdk_version_combo,
        ndk_version_combo,
        active_set_entry,
    }
}

pub(crate) fn page_projects(
    cfg: Arc<std::sync::Mutex<AppConfig>>,
    cmd_tx: mpsc::Sender<UiCommand>,
    parent: &gtk::ApplicationWindow,
) -> ProjectsPage {
    let page = make_page(
        "Projects - Templates, create/open, defaults",
        "Overview: Create or open projects from templates, list recent workspaces, and set per-project defaults like toolchain sets and default targets.",
        "Connections: Build runs reference these projects by id or path; Targets can use default target ids set here; Toolchains supply toolchain sets; jobs appear in Job Control and Job History; Settings controls ProjectService address.",
    );
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let refresh_templates = gtk::Button::with_label("Refresh templates");
    let refresh_defaults = gtk::Button::with_label("Refresh defaults");
    let list_recent = gtk::Button::with_label("List recent");
    let open_btn = gtk::Button::with_label("Open project");
    let create_btn = gtk::Button::with_label("Create project");
    set_tooltip(&refresh_templates, "What: Reload template list. Why: reflect new or updated templates. How: click to query ProjectService.");
    set_tooltip(&refresh_defaults, "What: Reload active defaults. Why: keep dropdowns in sync with Toolchain/Target defaults. How: click to fetch from ProjectService.");
    set_tooltip(&list_recent, "What: List recent projects. Why: reuse recent workspaces. How: click to query ProjectService.");
    set_tooltip(&open_btn, "What: Open a project by path. Why: register it in ProjectService and recents. How: enter a path and click.");
    set_tooltip(&create_btn, "What: Create a new project from a template. Why: bootstrap a new workspace. How: select template, name, and path, then click.");
    row.append(&refresh_templates);
    row.append(&refresh_defaults);
    row.append(&list_recent);
    row.append(&open_btn);
    row.append(&create_btn);
    page.container.insert_child_after(&row, Some(&page.intro));

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
    set_tooltip(&use_job_id_check, "What: Reuse an existing job id instead of creating a new job. Why: attach this project action to an existing job stream. How: enable it and fill Job id below.");
    set_tooltip(&job_id_entry, "What: Existing job id to attach. Why: stream results into a known job. How: paste from Job Control or Job History.");
    set_tooltip(&correlation_id_entry, "What: Correlation id to group project work into a run. Why: enables Observe run tracking across services. How: set a stable string and reuse it.");
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
    set_tooltip(&template_combo, "What: Template to create the project from. Why: ProjectService uses the template id to generate files. How: refresh templates, then pick one.");
    set_tooltip(&name_entry, "What: Project name. Why: used in generated project metadata. How: type a short, descriptive name.");
    set_tooltip(&path_entry, "What: Filesystem path for the project root. Why: ProjectService creates or opens the project here. How: enter a directory path or use Browse.");
    set_tooltip(&browse_btn, "What: Open a folder picker. Why: choose the project directory accurately. How: click and select a folder.");

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
    set_tooltip(&project_id_entry, "What: Project id to update config for. Why: set defaults on a specific project. How: copy an id from ProjectService output or Job History (auto-filled after create/open).");
    set_tooltip(&toolchain_set_combo, "What: Default toolchain set for the project. Why: builds and workflows can use it when none is specified. How: pick a set (or None).");
    set_tooltip(&default_target_combo, "What: Default target for the project. Why: target-aware workflows can use it when none is specified. How: pick a target (or None).");
    set_tooltip(&config_btn, "What: Persist project defaults. Why: saves toolchain set and target selections in ProjectService. How: choose values and click.");
    set_tooltip(&use_defaults_btn, "What: Apply the active defaults to this project. Why: sync project config with current global defaults. How: enter project id and click.");

    let label_project_id = gtk::Label::builder()
        .label("Project id")
        .xalign(0.0)
        .build();
    let label_toolchain = gtk::Label::builder()
        .label("Toolchain set")
        .xalign(0.0)
        .build();
    let label_target = gtk::Label::builder()
        .label("Default target")
        .xalign(0.0)
        .build();

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
        cmd_tx_refresh
            .try_send(UiCommand::ProjectListTemplates { cfg })
            .ok();
    });

    let cfg_defaults = cfg.clone();
    let cmd_tx_defaults = cmd_tx.clone();
    refresh_defaults.connect_clicked(move |_| {
        let cfg = cfg_defaults.lock().unwrap().clone();
        cmd_tx_defaults
            .try_send(UiCommand::ProjectLoadDefaults { cfg })
            .ok();
    });

    let cfg_recent = cfg.clone();
    let cmd_tx_recent = cmd_tx.clone();
    list_recent.connect_clicked(move |_| {
        let cfg = cfg_recent.lock().unwrap().clone();
        cmd_tx_recent
            .try_send(UiCommand::ProjectListRecent { cfg })
            .ok();
    });

    let cfg_open = cfg.clone();
    let cmd_tx_open = cmd_tx.clone();
    let path_entry_open = path_entry.clone();
    open_btn.connect_clicked(move |_| {
        let cfg = cfg_open.lock().unwrap().clone();
        cmd_tx_open
            .try_send(UiCommand::ProjectOpen {
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
            .try_send(UiCommand::ProjectCreate {
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
            .try_send(UiCommand::ProjectSetConfig {
                cfg,
                project_id,
                toolchain_set_id: if toolchain_set_id.trim().is_empty()
                    || toolchain_set_id == "none"
                {
                    None
                } else {
                    Some(toolchain_set_id)
                },
                default_target_id: if default_target_id.trim().is_empty()
                    || default_target_id == "none"
                {
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
            .try_send(UiCommand::ProjectUseActiveDefaults {
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
        project_id_entry,
    }
}

pub(crate) fn page_targets(
    cfg: Arc<std::sync::Mutex<AppConfig>>,
    cmd_tx: mpsc::Sender<UiCommand>,
    parent: &gtk::ApplicationWindow,
) -> TargetsPage {
    let page = make_page(
        "Targets - Devices, ADB, and Cuttlefish",
        "Overview: Manage ADB targets and Cuttlefish instances, install APKs, and launch apps via TargetService.",
        "Connections: Build runs produce APKs used here; Projects can set a default target; target jobs stream in Job Control and Job History; Evidence exports can capture target run context; Settings controls TargetService address.",
    );
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let list = gtk::Button::with_label("List targets");
    let stream = gtk::Button::with_label("Stream logcat (sample)");
    set_tooltip(&list, "What: List registered targets. Why: discover target ids and status. How: click to query TargetService.");
    set_tooltip(&stream, "What: Stream sample logcat output. Why: verify log streaming from a target. How: click to start a sample stream (uses target-sample-pixel).");
    row.append(&list);
    row.append(&stream);
    page.container.insert_child_after(&row, Some(&page.intro));

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
    set_tooltip(&use_job_id_check, "What: Reuse an existing job id instead of creating a new job. Why: attach this target action to an existing job stream. How: enable it and fill Job id below.");
    set_tooltip(&job_id_entry, "What: Existing job id to attach. Why: stream results into a known job. How: paste from Job Control or Job History.");
    set_tooltip(&correlation_id_entry, "What: Correlation id to group target work into a run. Why: enables Observe run tracking across services. How: set a stable string and reuse it.");
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
    set_tooltip(&status, "What: Check Cuttlefish status. Why: verify host readiness and running state. How: click to query TargetService.");
    set_tooltip(&web_ui, "What: Open the Cuttlefish WebRTC UI in your browser. Why: access the virtual device UI. How: click; uses AADK_CUTTLEFISH_WEBRTC_URL or https://localhost:8443.");
    set_tooltip(&env_ui, "What: Open the Cuttlefish environment control UI. Why: manage environment toggles. How: click; uses AADK_CUTTLEFISH_ENV_URL or https://localhost:1443.");
    set_tooltip(&docs, "What: Open Cuttlefish documentation. Why: reference setup instructions. How: click to open the official docs.");
    set_tooltip(&install, "What: Install Cuttlefish build tools. Why: required before starting virtual devices. How: fill branch/target/build id as needed and click.");
    set_tooltip(&resolve_build, "What: Resolve a Cuttlefish build id. Why: convert branch/target hints into a build id. How: fill branch/target/build id fields and click.");
    set_tooltip(&start, "What: Start a Cuttlefish instance. Why: launch a virtual device. How: set optional job/correlation ids and click.");
    set_tooltip(&stop, "What: Stop the Cuttlefish instance. Why: shut down a running virtual device. How: set optional job/correlation ids and click.");

    let cuttlefish_buttons = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .column_homogeneous(true)
        .build();
    let cuttlefish_header = gtk::Label::builder()
        .label("Cuttlefish")
        .xalign(0.0)
        .build();
    cuttlefish_buttons.attach(&cuttlefish_header, 0, 0, 4, 1);
    cuttlefish_buttons.attach(&status, 0, 1, 1, 1);
    cuttlefish_buttons.attach(&web_ui, 1, 1, 1, 1);
    cuttlefish_buttons.attach(&env_ui, 2, 1, 1, 1);
    cuttlefish_buttons.attach(&docs, 3, 1, 1, 1);
    cuttlefish_buttons.attach(&install, 0, 2, 1, 1);
    cuttlefish_buttons.attach(&resolve_build, 1, 2, 1, 1);
    cuttlefish_buttons.attach(&start, 2, 2, 1, 1);
    cuttlefish_buttons.attach(&stop, 3, 2, 1, 1);

    let default_target_serial =
        std::env::var("AADK_CUTTLEFISH_ADB_SERIAL").unwrap_or_else(|_| "127.0.0.1:6520".into());

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
    set_tooltip(&cuttlefish_branch_entry, "What: Android build branch for Cuttlefish (optional). Why: used to resolve build ids or install tools. How: leave blank for defaults or enter a branch name.");
    set_tooltip(&cuttlefish_target_entry, "What: Cuttlefish target name (optional). Why: used to resolve or install builds. How: leave blank for defaults or enter a target like aosp_cf_x86_64_phone.");
    set_tooltip(&cuttlefish_build_entry, "What: Cuttlefish build id (optional). Why: start or install a specific build. How: paste a build id or click Resolve build.");

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
    set_tooltip(&target_entry, "What: Target id or adb serial. Why: install/launch actions run against this target. How: use the default or paste from List targets.");
    set_tooltip(&apk_entry, "What: Absolute path to an APK file. Why: used by the install action. How: browse or paste a file path.");
    set_tooltip(&apk_browse, "What: Open file picker for APK. Why: avoid typing the file path. How: click and select an .apk file.");
    set_tooltip(&app_id_entry, "What: Android application id (package name). Why: used to launch the app. How: enter a package like com.example.app.");
    set_tooltip(&activity_entry, "What: Activity class to launch. Why: defines the app entry point. How: enter .MainActivity or a full class name.");

    let label_target = gtk::Label::builder().label("Target id").xalign(0.0).build();
    let label_apk = gtk::Label::builder().label("APK path").xalign(0.0).build();
    let label_app_id = gtk::Label::builder()
        .label("Application id")
        .xalign(0.0)
        .build();
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
    set_tooltip(&install_apk, "What: Install APK on the target. Why: deploy build output for testing. How: set Target id and APK path, then click.");
    set_tooltip(&launch_app, "What: Launch the app on the target. Why: verify install and run. How: set Target id, application id, and activity, then click.");
    action_row.append(&install_apk);
    action_row.append(&launch_app);
    form.attach(&action_row, 1, 4, 1, 1);

    let default_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let set_default_btn = gtk::Button::with_label("Set default");
    let get_default_btn = gtk::Button::with_label("Get default");
    set_tooltip(&set_default_btn, "What: Set the default target. Why: other pages use it when no target id is specified. How: enter Target id and click.");
    set_tooltip(&get_default_btn, "What: Get the current default target. Why: confirm which target is used by default. How: click to query TargetService.");
    default_row.append(&set_default_btn);
    default_row.append(&get_default_btn);
    form.attach(&default_row, 1, 5, 1, 1);

    page.container
        .insert_child_after(&cuttlefish_buttons, Some(&job_grid));
    page.container
        .insert_child_after(&cuttlefish_grid, Some(&cuttlefish_buttons));
    page.container
        .insert_child_after(&form, Some(&cuttlefish_grid));

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
        cmd_tx_list.try_send(UiCommand::TargetsList { cfg }).ok();
    });

    let cfg_stream = cfg.clone();
    let cmd_tx_stream = cmd_tx.clone();
    stream.connect_clicked(move |_| {
        let cfg = cfg_stream.lock().unwrap().clone();
        cmd_tx_stream
            .try_send(UiCommand::TargetsStreamLogcat {
                cfg,
                target_id: "target-sample-pixel".into(),
                filter: "".into(),
            })
            .ok();
    });

    let cfg_status = cfg.clone();
    let cmd_tx_status = cmd_tx.clone();
    status.connect_clicked(move |_| {
        let cfg = cfg_status.lock().unwrap().clone();
        cmd_tx_status
            .try_send(UiCommand::TargetsCuttlefishStatus { cfg })
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
            .try_send(UiCommand::TargetsResolveCuttlefishBuild {
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
        match gtk::gio::AppInfo::launch_default_for_uri(&url, None::<&gtk::gio::AppLaunchContext>) {
            Ok(_) => page_web.append(&format!("Opened Cuttlefish UI: {url}\n")),
            Err(err) => page_web.append(&format!("Failed to open Cuttlefish UI: {err}\n")),
        }
    });

    let page_env = page.clone();
    env_ui.connect_clicked(move |_| {
        let url = std::env::var("AADK_CUTTLEFISH_ENV_URL")
            .unwrap_or_else(|_| "https://localhost:1443".into());
        match gtk::gio::AppInfo::launch_default_for_uri(&url, None::<&gtk::gio::AppLaunchContext>) {
            Ok(_) => page_env.append(&format!("Opened Cuttlefish env control: {url}\n")),
            Err(err) => page_env.append(&format!("Failed to open Cuttlefish env control: {err}\n")),
        }
    });

    let page_docs = page.clone();
    docs.connect_clicked(move |_| {
        let url = "https://source.android.com/docs/devices/cuttlefish/get-started";
        match gtk::gio::AppInfo::launch_default_for_uri(url, None::<&gtk::gio::AppLaunchContext>) {
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
            .try_send(UiCommand::TargetsStartCuttlefish {
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
            .try_send(UiCommand::TargetsStopCuttlefish {
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
            .try_send(UiCommand::TargetsSetDefault {
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
            .try_send(UiCommand::TargetsGetDefault { cfg })
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
            .try_send(UiCommand::TargetsInstallCuttlefish {
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
            .try_send(UiCommand::TargetsInstallApk {
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
            .try_send(UiCommand::TargetsLaunchApp {
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
        target_entry,
    }
}

pub(crate) fn page_console(
    cfg: Arc<std::sync::Mutex<AppConfig>>,
    cmd_tx: mpsc::Sender<UiCommand>,
    parent: &gtk::ApplicationWindow,
) -> BuildPage {
    let page = make_page(
        "Build - BuildService (Gradle) runner",
        "Overview: Run Gradle builds with module/variant/task overrides and list artifacts from the build output.",
        "Connections: Projects provide ids/paths; Toolchains provide SDK/NDK; Targets use APKs produced here; build jobs stream in Job Control and Job History; Evidence exports run bundles; Settings controls BuildService address.",
    );

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
    set_tooltip(&project_entry, "What: Project path or project id. Why: BuildService resolves the project root from this reference. How: paste a filesystem path or a recent project id (auto-filled after create/open).");
    set_tooltip(&project_browse, "What: Open a folder picker for the project. Why: choose a valid project path quickly. How: click and select a folder.");
    set_tooltip(&module_entry, "What: Gradle module to build (optional). Why: target a single module instead of the whole project. How: enter app or :app.");
    set_tooltip(&variant_name_entry, "What: Explicit variant name override. Why: use a custom variant beyond debug/release. How: enter a variant like demoDebug.");
    set_tooltip(&tasks_entry, "What: Explicit Gradle tasks. Why: override the default task selection. How: enter space or comma separated tasks.");
    set_tooltip(&args_entry, "What: Extra Gradle args. Why: pass flags or properties to Gradle. How: enter args like --stacktrace -Pfoo=bar.");
    set_tooltip(&variant_combo, "What: Base build variant (debug/release). Why: used when Variant name is empty. How: choose from the dropdown.");
    set_tooltip(&clean_check, "What: Clean before build. Why: ensure a fresh build with no stale outputs. How: check to run clean first.");
    set_tooltip(&run, "What: Start the build job. Why: run BuildService and stream logs. How: fill inputs and click.");

    let label_project = gtk::Label::builder().label("Project").xalign(0.0).build();
    let label_module = gtk::Label::builder().label("Module").xalign(0.0).build();
    let label_variant = gtk::Label::builder().label("Variant").xalign(0.0).build();
    let label_variant_name = gtk::Label::builder()
        .label("Variant name")
        .xalign(0.0)
        .build();
    let label_tasks = gtk::Label::builder().label("Tasks").xalign(0.0).build();
    let label_args = gtk::Label::builder()
        .label("Gradle args")
        .xalign(0.0)
        .build();

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

    page.container.insert_child_after(&form, Some(&page.intro));

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
    set_tooltip(&use_job_id_check, "What: Reuse an existing job id instead of creating a new job. Why: attach this build to an existing job stream. How: enable it and fill Job id below.");
    set_tooltip(&job_id_entry, "What: Existing job id to attach. Why: stream results into a known job. How: paste from Job Control or Job History.");
    set_tooltip(&correlation_id_entry, "What: Correlation id to group build work into a run. Why: enables Observe run tracking across services. How: set a stable string and reuse it.");
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
    set_tooltip(&artifact_modules_entry, "What: Module filter list. Why: narrow artifact listing to specific modules. How: enter comma/space separated module names.");
    set_tooltip(&artifact_variant_entry, "What: Variant filter override. Why: list artifacts for a specific variant; overrides the dropdown if set. How: enter a variant name or leave blank.");
    set_tooltip(&artifact_types_entry, "What: Artifact types filter. Why: limit results to apk/aab/aar/mapping/test. How: enter a comma/space list of types.");
    set_tooltip(&artifact_name_entry, "What: Substring to match artifact name. Why: filter results by name. How: enter a partial name.");
    set_tooltip(&artifact_path_entry, "What: Substring to match artifact path. Why: filter results by path. How: enter a partial path.");
    set_tooltip(&list_artifacts, "What: List artifacts for the project. Why: find build outputs to install or share. How: set filters and click.");

    let label_artifact_modules = gtk::Label::builder().label("Modules").xalign(0.0).build();
    let label_artifact_variant = gtk::Label::builder().label("Variant").xalign(0.0).build();
    let label_artifact_types = gtk::Label::builder().label("Types").xalign(0.0).build();
    let label_artifact_name = gtk::Label::builder()
        .label("Name contains")
        .xalign(0.0)
        .build();
    let label_artifact_path = gtk::Label::builder()
        .label("Path contains")
        .xalign(0.0)
        .build();

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

    page.container
        .insert_child_after(&artifacts_label, Some(&job_grid));
    page.container
        .insert_child_after(&artifact_form, Some(&artifacts_label));

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
            .try_send(UiCommand::BuildRun {
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
            .try_send(UiCommand::BuildListArtifacts {
                cfg,
                project_ref,
                variant,
                filter,
            })
            .ok();
    });

    BuildPage {
        page,
        project_entry,
    }
}

pub(crate) fn page_evidence(
    cfg: Arc<std::sync::Mutex<AppConfig>>,
    cmd_tx: mpsc::Sender<UiCommand>,
) -> Page {
    let page = make_page(
        "Evidence - ObserveService runs and bundles",
        "Overview: List runs and outputs, group jobs by run, stream run-level events, and export support/evidence bundles or job logs.",
        "Connections: Jobs started in Job Control, Workflow, Build, Toolchains, Projects, and Targets populate runs here. Use job ids or correlation ids from Job History. Settings controls ObserveService address.",
    );
    let primary_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let list_runs = gtk::Button::with_label("List runs");
    let export_support = gtk::Button::with_label("Export support bundle");
    let export_evidence = gtk::Button::with_label("Export evidence bundle");
    set_tooltip(&list_runs, "What: List runs from ObserveService. Why: discover run ids for evidence exports. How: click to query.");
    set_tooltip(&export_support, "What: Export a support bundle. Why: capture logs and config for troubleshooting. How: set options and click.");
    set_tooltip(&export_evidence, "What: Export an evidence bundle for a run. Why: capture run artifacts for audit or sharing. How: enter run id and click.");
    primary_row.append(&list_runs);
    primary_row.append(&export_support);
    primary_row.append(&export_evidence);
    page.container
        .insert_child_after(&primary_row, Some(&page.intro));

    let secondary_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let list_jobs = gtk::Button::with_label("List jobs for run");
    let stream_run = gtk::Button::with_label("Stream run events");
    let list_outputs = gtk::Button::with_label("List outputs");
    let export_job_logs = gtk::Button::with_label("Export job logs");
    set_tooltip(&list_jobs, "What: List jobs for a run id or correlation id. Why: group pipeline jobs together. How: enter run id/correlation id and click.");
    set_tooltip(&stream_run, "What: Stream run-level events. Why: watch pipeline progress across jobs. How: enter run id or correlation id and click.");
    set_tooltip(&list_outputs, "What: List outputs (bundles/artifacts) for a run. Why: discover bundle paths and build artifacts tied to the run. How: set filters and click.");
    set_tooltip(&export_job_logs, "What: Export job logs to JSON. Why: share job details alongside run bundles. How: enter a job id and optional path, then click.");
    secondary_row.append(&list_jobs);
    secondary_row.append(&stream_run);
    secondary_row.append(&list_outputs);
    secondary_row.append(&export_job_logs);
    page.container
        .insert_child_after(&secondary_row, Some(&primary_row));

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
    let output_path_entry = gtk::Entry::builder()
        .placeholder_text("job log export path (optional)")
        .hexpand(true)
        .build();
    set_tooltip(&use_job_id_check, "What: Reuse an existing job id instead of creating a new job. Why: attach this export to an existing job stream. How: enable it and fill Job id below.");
    set_tooltip(&job_id_entry, "What: Existing job id to attach. Why: stream results into a known job. How: paste from Job Control or Job History.");
    set_tooltip(&correlation_id_entry, "What: Correlation id to group work into a run. Why: filter run dashboards and evidence exports. How: set a stable string and reuse it.");
    set_tooltip(&output_path_entry, "What: Optional output path for job log exports. Why: save logs to a specific file. How: leave blank to use the default export path.");
    job_grid.attach(&use_job_id_check, 0, 0, 1, 1);
    job_grid.attach(&job_id_entry, 1, 0, 1, 1);
    job_grid.attach(&gtk::Label::new(Some("Correlation id")), 0, 1, 1, 1);
    job_grid.attach(&correlation_id_entry, 1, 1, 1, 1);
    job_grid.attach(&gtk::Label::new(Some("Log export path")), 0, 2, 1, 1);
    job_grid.attach(&output_path_entry, 1, 2, 1, 1);
    page.container
        .insert_child_after(&job_grid, Some(&secondary_row));

    let form = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let run_id_entry = gtk::Entry::builder()
        .placeholder_text("Run id for evidence bundle")
        .hexpand(true)
        .build();
    let output_kind_combo = gtk::ComboBoxText::new();
    output_kind_combo.append_text("Any");
    output_kind_combo.append_text("Bundles");
    output_kind_combo.append_text("Artifacts");
    output_kind_combo.set_active(Some(0));
    let output_type_entry = gtk::Entry::builder()
        .placeholder_text("output type (support_bundle, apk, ...)")
        .hexpand(true)
        .build();
    let output_path_entry = gtk::Entry::builder()
        .placeholder_text("output path contains")
        .hexpand(true)
        .build();
    let output_label_entry = gtk::Entry::builder()
        .placeholder_text("output label contains")
        .hexpand(true)
        .build();
    let recent_limit_entry = gtk::Entry::builder().text("10").hexpand(true).build();
    let include_history = gtk::CheckButton::with_label("Include run history in stream");
    include_history.set_active(true);

    let include_logs = gtk::CheckButton::with_label("Include logs");
    include_logs.set_active(true);
    let include_config = gtk::CheckButton::with_label("Include config");
    include_config.set_active(true);
    let include_toolchain = gtk::CheckButton::with_label("Include toolchain provenance");
    include_toolchain.set_active(true);
    let include_recent = gtk::CheckButton::with_label("Include recent runs");
    include_recent.set_active(true);
    set_tooltip(&run_id_entry, "What: Run id for dashboards and evidence exports. Why: run-level actions need a run id. How: copy from List runs or Job History run_id.");
    set_tooltip(&output_kind_combo, "What: Output kind filter. Why: narrow to bundles or artifacts. How: pick Any/Bundles/Artifacts.");
    set_tooltip(&output_type_entry, "What: Output type filter. Why: match bundle types or artifact kinds. How: enter support_bundle, evidence_bundle, apk, aab, etc.");
    set_tooltip(
        &output_path_entry,
        "What: Output path substring. Why: narrow to specific files. How: enter part of a path.",
    );
    set_tooltip(&output_label_entry, "What: Output label substring. Why: narrow to specific outputs. How: enter part of a label.");
    set_tooltip(&recent_limit_entry, "What: Limit for recent runs included in support bundle. Why: control bundle size. How: enter an integer, for example 10.");
    set_tooltip(&include_history, "What: Include previous run events in the stream. Why: replay history when attaching to existing runs. How: enable to replay before live events.");
    set_tooltip(&include_logs, "What: Include job logs. Why: logs are essential for troubleshooting. How: check to include.");
    set_tooltip(&include_config, "What: Include config snapshot. Why: capture environment and service settings. How: check to include.");
    set_tooltip(&include_toolchain, "What: Include toolchain provenance. Why: record SDK/NDK versions used. How: check to include.");
    set_tooltip(&include_recent, "What: Include recent runs. Why: add context around the target run. How: check and set a limit.");

    let label_run_id = gtk::Label::builder()
        .label("Evidence run id")
        .xalign(0.0)
        .build();
    let label_limit = gtk::Label::builder()
        .label("Recent runs limit")
        .xalign(0.0)
        .build();
    let label_output_kind = gtk::Label::builder()
        .label("Output kind")
        .xalign(0.0)
        .build();
    let label_output_type = gtk::Label::builder()
        .label("Output type")
        .xalign(0.0)
        .build();
    let label_output_path = gtk::Label::builder()
        .label("Path contains")
        .xalign(0.0)
        .build();
    let label_output_label = gtk::Label::builder()
        .label("Label contains")
        .xalign(0.0)
        .build();

    form.attach(&label_run_id, 0, 0, 1, 1);
    form.attach(&run_id_entry, 1, 0, 1, 1);
    form.attach(&label_limit, 0, 1, 1, 1);
    form.attach(&recent_limit_entry, 1, 1, 1, 1);
    form.attach(&include_history, 1, 2, 1, 1);

    let checkbox_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    checkbox_row.append(&include_logs);
    checkbox_row.append(&include_config);
    checkbox_row.append(&include_toolchain);
    checkbox_row.append(&include_recent);
    form.attach(&checkbox_row, 1, 3, 1, 1);
    form.attach(&label_output_kind, 0, 4, 1, 1);
    form.attach(&output_kind_combo, 1, 4, 1, 1);
    form.attach(&label_output_type, 0, 5, 1, 1);
    form.attach(&output_type_entry, 1, 5, 1, 1);
    form.attach(&label_output_path, 0, 6, 1, 1);
    form.attach(&output_path_entry, 1, 6, 1, 1);
    form.attach(&label_output_label, 0, 7, 1, 1);
    form.attach(&output_label_entry, 1, 7, 1, 1);

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
    let run_id_entry_list = run_id_entry.clone();
    let correlation_id_entry_list = correlation_id_entry.clone();
    list_runs.connect_clicked(move |_| {
        let cfg = cfg_list.lock().unwrap().clone();
        cmd_tx_list
            .try_send(UiCommand::ObserveListRuns {
                cfg,
                run_id: run_id_entry_list.text().to_string(),
                correlation_id: correlation_id_entry_list.text().to_string(),
                result: String::new(),
                page_size: 25,
                page_token: String::new(),
                page: "evidence",
            })
            .ok();
    });

    let cfg_list_jobs = cfg.clone();
    let cmd_tx_list_jobs = cmd_tx.clone();
    let run_id_entry_list_jobs = run_id_entry.clone();
    let correlation_id_entry_list_jobs = correlation_id_entry.clone();
    list_jobs.connect_clicked(move |_| {
        let cfg = cfg_list_jobs.lock().unwrap().clone();
        cmd_tx_list_jobs
            .try_send(UiCommand::JobsList {
                cfg,
                job_types: String::new(),
                states: String::new(),
                created_after: String::new(),
                created_before: String::new(),
                finished_after: String::new(),
                finished_before: String::new(),
                correlation_id: correlation_id_entry_list_jobs.text().to_string(),
                run_id: run_id_entry_list_jobs.text().to_string(),
                page_size: 200,
                page_token: String::new(),
                page: "evidence",
            })
            .ok();
    });

    let cfg_stream = cfg.clone();
    let cmd_tx_stream = cmd_tx.clone();
    let run_id_entry_stream = run_id_entry.clone();
    let correlation_id_entry_stream = correlation_id_entry.clone();
    let include_history_stream = include_history.clone();
    stream_run.connect_clicked(move |_| {
        let cfg = cfg_stream.lock().unwrap().clone();
        cmd_tx_stream
            .try_send(UiCommand::StreamRunEvents {
                cfg,
                run_id: run_id_entry_stream.text().to_string(),
                correlation_id: correlation_id_entry_stream.text().to_string(),
                include_history: include_history_stream.is_active(),
                page: "evidence",
            })
            .ok();
    });

    let cfg_outputs = cfg.clone();
    let cmd_tx_outputs = cmd_tx.clone();
    let run_id_entry_outputs = run_id_entry.clone();
    let output_kind_combo_outputs = output_kind_combo.clone();
    let output_type_entry_outputs = output_type_entry.clone();
    let output_path_entry_outputs = output_path_entry.clone();
    let output_label_entry_outputs = output_label_entry.clone();
    list_outputs.connect_clicked(move |_| {
        let cfg = cfg_outputs.lock().unwrap().clone();
        let kind = match output_kind_combo_outputs.active() {
            Some(1) => RunOutputKind::Bundle as i32,
            Some(2) => RunOutputKind::Artifact as i32,
            _ => RunOutputKind::Unspecified as i32,
        };
        cmd_tx_outputs
            .try_send(UiCommand::ObserveListOutputs {
                cfg,
                run_id: run_id_entry_outputs.text().to_string(),
                kind,
                output_type: output_type_entry_outputs.text().to_string(),
                path_contains: output_path_entry_outputs.text().to_string(),
                label_contains: output_label_entry_outputs.text().to_string(),
                page_size: 50,
                page_token: String::new(),
                page: "evidence",
            })
            .ok();
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
        let limit = recent_limit_support.text().parse::<u32>().unwrap_or(10);
        cmd_tx_support
            .try_send(UiCommand::ObserveExportSupport {
                cfg,
                include_logs: include_logs_support.is_active(),
                include_config: include_config_support.is_active(),
                include_toolchain_provenance: include_toolchain_support.is_active(),
                include_recent_runs: include_recent_support.is_active(),
                recent_runs_limit: limit,
                job_id,
                correlation_id,
                page: "evidence",
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
            .try_send(UiCommand::ObserveExportEvidence {
                cfg,
                run_id: run_id_evidence.text().to_string(),
                job_id,
                correlation_id,
                page: "evidence",
            })
            .ok();
    });

    let cfg_export_jobs = cfg.clone();
    let cmd_tx_export_jobs = cmd_tx.clone();
    let job_id_entry_export = job_id_entry.clone();
    let output_path_entry_export = output_path_entry.clone();
    export_job_logs.connect_clicked(move |_| {
        let job_id = job_id_entry_export.text().to_string();
        {
            let mut cfg = cfg_export_jobs.lock().unwrap();
            if !job_id.trim().is_empty() {
                cfg.last_job_id = job_id.clone();
            }
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }
        let cfg = cfg_export_jobs.lock().unwrap().clone();
        cmd_tx_export_jobs
            .try_send(UiCommand::JobsExportLogs {
                cfg,
                job_id,
                output_path: output_path_entry_export.text().to_string(),
                page: "evidence",
            })
            .ok();
    });

    page
}

pub(crate) fn page_settings(cfg: Arc<std::sync::Mutex<AppConfig>>) -> Page {
    let page = make_page(
        "Settings - Service endpoints",
        "Overview: Edit the host:port addresses the UI uses to connect to each service.",
        "Connections: Every other tab depends on these addresses; if a page cannot connect, update it here.",
    );

    let form = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .build();

    let add_row = |row: i32,
                   label: &str,
                   tooltip: &str,
                   initial: String,
                   setter: Box<dyn Fn(String) + 'static>| {
        let l = gtk::Label::builder().label(label).xalign(0.0).build();
        let e = gtk::Entry::builder().text(initial).hexpand(true).build();
        set_tooltip(&e, tooltip);
        e.connect_changed(move |ent| setter(ent.text().to_string()));
        form.attach(&l, 0, row, 1, 1);
        form.attach(&e, 1, row, 1, 1);
    };

    let cfg1 = cfg.clone();
    add_row(
        0,
        "JobService",
        "What: JobService address (host:port). Why: Job Control and Job History use it for job control/logs. How: enter an address like 127.0.0.1:50051.",
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
        "What: ToolchainService address (host:port). Why: Toolchains tab uses it for SDK/NDK operations. How: enter an address like 127.0.0.1:50052.",
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
        "What: ProjectService address (host:port). Why: Projects tab uses it to create/open projects. How: enter an address like 127.0.0.1:50053.",
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
        "What: BuildService address (host:port). Why: Build tab uses it for builds and artifacts. How: enter an address like 127.0.0.1:50054.",
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
        "What: TargetService address (host:port). Why: Targets tab uses it for ADB and Cuttlefish actions. How: enter an address like 127.0.0.1:50055.",
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
        "What: ObserveService address (host:port). Why: Evidence tab uses it for runs and bundles. How: enter an address like 127.0.0.1:50056.",
        cfg.lock().unwrap().observe_addr.clone(),
        Box::new(move |v| {
            let mut cfg = cfg6.lock().unwrap();
            cfg.observe_addr = v;
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }),
    );

    let cfg7 = cfg.clone();
    add_row(
        6,
        "WorkflowService",
        "What: WorkflowService address (host:port). Why: Workflow tab uses it for pipeline runs. How: enter an address like 127.0.0.1:50057.",
        cfg.lock().unwrap().workflow_addr.clone(),
        Box::new(move |v| {
            let mut cfg = cfg7.lock().unwrap();
            cfg.workflow_addr = v;
            if let Err(err) = cfg.save() {
                eprintln!("Failed to persist UI config: {err}");
            }
        }),
    );

    page.container.insert_child_after(&form, Some(&page.intro));

    let telemetry_frame = gtk::Frame::builder()
        .label("Telemetry (opt-in)")
        .build();
    let telemetry_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let usage_check = gtk::CheckButton::with_label("Send usage analytics");
    let crash_check = gtk::CheckButton::with_label("Send crash reports");
    let install_label = gtk::Label::builder().xalign(0.0).build();
    set_tooltip(
        &usage_check,
        "What: Send anonymous usage event counts. Why: helps prioritize fixes. How: opt in to enable.",
    );
    set_tooltip(
        &crash_check,
        "What: Send crash summaries on next launch. Why: helps debug stability issues. How: opt in to enable.",
    );

    {
        let cfg = cfg.lock().unwrap();
        usage_check.set_active(cfg.telemetry_usage_enabled);
        crash_check.set_active(cfg.telemetry_crash_enabled);
        install_label.set_text(&telemetry_label_text(&cfg.telemetry_install_id));
    }

    let install_label_usage = install_label.clone();
    let cfg_usage = cfg.clone();
    usage_check.connect_toggled(move |check| {
        let enabled = check.is_active();
        let mut cfg = cfg_usage.lock().unwrap();
        cfg.telemetry_usage_enabled = enabled;
        if (enabled || cfg.telemetry_crash_enabled) && cfg.telemetry_install_id.trim().is_empty() {
            cfg.telemetry_install_id = telemetry::generate_install_id();
        }
        if let Err(err) = cfg.save() {
            eprintln!("Failed to persist UI config: {err}");
        }
        let install_id = cfg.telemetry_install_id.clone();
        drop(cfg);
        telemetry::set_usage_enabled(enabled);
        if !install_id.trim().is_empty() {
            telemetry::set_install_id(Some(install_id.clone()));
        }
        install_label_usage.set_text(&telemetry_label_text(&install_id));
    });

    let install_label_crash = install_label.clone();
    let cfg_crash = cfg.clone();
    crash_check.connect_toggled(move |check| {
        let enabled = check.is_active();
        let mut cfg = cfg_crash.lock().unwrap();
        cfg.telemetry_crash_enabled = enabled;
        if (enabled || cfg.telemetry_usage_enabled) && cfg.telemetry_install_id.trim().is_empty() {
            cfg.telemetry_install_id = telemetry::generate_install_id();
        }
        if let Err(err) = cfg.save() {
            eprintln!("Failed to persist UI config: {err}");
        }
        let install_id = cfg.telemetry_install_id.clone();
        drop(cfg);
        telemetry::set_crash_enabled(enabled);
        if !install_id.trim().is_empty() {
            telemetry::set_install_id(Some(install_id.clone()));
        }
        install_label_crash.set_text(&telemetry_label_text(&install_id));
    });

    telemetry_box.append(&usage_check);
    telemetry_box.append(&crash_check);
    telemetry_box.append(&install_label);
    telemetry_frame.set_child(Some(&telemetry_box));
    page.container
        .insert_child_after(&telemetry_frame, Some(&form));

    page
}

fn telemetry_label_text(install_id: &str) -> String {
    if install_id.trim().is_empty() {
        "Install ID: not set".to_string()
    } else {
        format!("Install ID: {install_id}")
    }
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
