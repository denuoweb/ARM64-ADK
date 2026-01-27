# GTK UI Agent Notes (aadk-ui)

## Role and scope
The GTK4 UI is a thin client for the gRPC services. It provides pages for Job Control (job run/status),
Workflow pipelines, Job History, Toolchains, Projects, Targets, Build, and Evidence. It uses a
background tokio runtime thread to keep the GTK main thread responsive.

## Maintenance
Update this file whenever UI behavior changes or when commits touching this crate are made.

## Key implementation details
- Entry point and GTK wiring live in `crates/aadk-ui/src/main.rs`, with modules split into
  `config.rs`, `commands.rs`, `models.rs`, `pages.rs`, `ui_events.rs`, `ui_state.rs`, `utils.rs`, and `worker.rs`.
- AppConfig holds service addresses and pulls defaults from env:
  AADK_JOB_ADDR, AADK_TOOLCHAIN_ADDR, AADK_PROJECT_ADDR, AADK_BUILD_ADDR,
  AADK_TARGETS_ADDR, AADK_OBSERVE_ADDR, AADK_WORKFLOW_ADDR.
- AppConfig persists to `~/.local/share/aadk/state/ui-config.json` with service endpoints, active context, last job selections, and telemetry opt-in.
- Job log export helpers (default paths + history fetch) and shared data-dir utilities come from `aadk-util`.
- UiCommand/AppEvent live in `commands.rs`; a background worker in `worker.rs` executes commands and
  emits AppEvent logs to update the UI via the bounded queue in `ui_events.rs`.
- AppEvent delivery uses a bounded queue notified via tokio mpsc; a MainContext spawn_local loop
  drains the queue on the GTK thread, dropping log lines before non-log events and coalescing HomeProgress updates.
- The UI mostly logs results rather than rendering structured data; it is intentionally minimal.
- Core flow actions log connection/RPC failures to the page output so bad inputs and service errors are visible.
- The main window default size is clamped to 90% of the primary monitor so it stays on-screen.
- Each page is wrapped in a scroller so tall control layouts remain usable on smaller screens.
- Job stream output for service pages prints summary lines for state/progress/completion and decodes log chunks to text instead of raw payload bytes.
- Settings includes opt-in telemetry toggles for usage and crash reporting (env overrides: AADK_TELEMETRY/AADK_TELEMETRY_CRASH).
- Settings includes local state archive Save/Open/Reload controls with archive exclusions and zip path selection.
- Home job streams run on cancellable worker tasks; new watch requests abort the previous stream and progress updates are throttled.
- Workflow page runs workflow.pipeline with explicit inputs, optional step overrides, and run-level StreamRunEvents output.
- Projects auto-fill the project id after create/open and sync the Build project field to the latest selection.
- Page construction now includes a per-tab header, overview, and connections blurb; control layouts insert after the intro block.
- All interactive fields and selections include verbose tooltips describing what, why, and how to use them.
- Evidence page includes run dashboards: list runs with job ids, list jobs by run, stream run events, and export job logs alongside bundles.
- Evidence page can list run outputs with bundle/artifact filters and surfaces run output summary counts.
- Sidebar order is Job Control, Workflow, Toolchains, Projects, Build, Targets, Job History, Evidence, Settings (Home -> Job Control, Console -> Build).
- Settings now includes WorkflowService alongside the other service endpoints.
- Toolchains page fetches available SDK/NDK versions and populates dropdowns; install/verify actions
  use the selected version and default to the latest SDK_VERSION/NDK_VERSION.
- Toolchains page includes a "Use latest installed" shortcut to create and activate a toolchain set from the most recently installed SDK/NDK.
- The Cuttlefish docs button opens https://source.android.com/docs/devices/cuttlefish/get-started.
- The Targets page includes an "Open Cuttlefish Env" button using AADK_CUTTLEFISH_ENV_URL (default https://localhost:1443).
- Observe export requests include optional metadata fields (project/target/toolchain ids), currently unset in the UI.
- Toolchains/Projects/Targets/Build/Evidence pages include a "Use job id" toggle plus
  correlation id entry to attach work to existing jobs and grouped workflows; the UI derives run_id
  from correlation_id for run-aware services.
- Log text views apply a ring buffer to cap memory by line/character counts.
- The UI tracks an active context (project/toolchain set/target/run) persisted in ui-config, surfaces
  it in the header, and applies it to Workflow/Projects/Targets/Build fields.
- Per-tab UI state (inputs + log buffers) persists to `~/.local/share/aadk/state/ui-state.json`;
  reset-all-state clears it alongside cached UI fields while preserving installed toolchains/downloads
  and Cuttlefish data (keeps `state/toolchains.json`).
- Workflow run responses, toolchain active set updates, and target default updates sync the active context.
- The header "New project" action runs reset-all-state (clearing local state/logs while preserving `toolchains`, `downloads`, `cuttlefish`, and `state/toolchains.json`) and then opens the project folder picker; "Open project" opens the folder picker and auto-opens existing projects.

## Service coverage
- Job Control: start arbitrary jobs (including workflow.pipeline) with params/ids + optional correlation id, watch job streams, live status panel.
- Workflow: run workflow.pipeline with step inputs and stream run-level events.
- Job History: list jobs and event history with filters; export logs.
- Toolchains: list providers/available/installed/sets, install/verify, update/uninstall, cache cleanup, create/set/get toolchain sets.
- Projects: list templates, create/open, list recent, set project config, use active defaults.
- Targets: list targets, set/get default target, resolve Cuttlefish build ids, install/start/stop Cuttlefish, logcat, install APK, launch app.
- Build: run builds via BuildService with module/variant_name/tasks overrides, list artifacts with filters (grouped by module), stream job events.
- Evidence: list runs and outputs, export support bundles, export evidence bundles, stream job events.

## Environment / config
- All service addresses can be overridden via env vars listed above.
- If running on minimal GTK installs, GTK_A11Y=none can suppress the accessibility warning.

## Implementation notes
- Job Control page event routing now keeps the HomePage handle so status labels and log output update together.
- Project config updates treat empty/"none" toolchain or target selections as unset before sending.
- Telemetry emits app.start, ui.page.view, and ui.command.* events when opt-in is enabled.
- Settings shows telemetry log/crash locations and provides buttons to open the folders.
- State archive operations block if the latest job is queued/running and serialize via a FIFO queue under ~/.local/share/aadk/state-ops.
- Open State reloads all services via ReloadState RPCs and refreshes the UI config from disk.
- Reset-all-state (triggered by the header New project flow) clears the Evidence log buffer via `Page::clear` since Evidence is a bare `Page`.
- Project open/create and target install actions now prompt for a folder/APK when the path is blank, and project folder picks auto-open existing projects when metadata is present.
- Opening existing projects resets the Projects template selection to None; Targets defaults no longer force the SampleConsole application id and attempt to infer it from app Gradle or manifest when unset.
- Projects page removed an unused project-id setter to keep builds warning-free.
- Targets list auto-fills the active target id when missing, and Cuttlefish start completion updates it from adb_serial.
- Targets launch can infer the application id from an APK path rooted in a project when the field is empty, and APK selection/build output can auto-fill the app id (Targets and Workflow pages; Workflow includes an APK browse button and auto-enables Install APK when a path is chosen).
- State archive Save/Open dialogs clone their queue callbacks per click so GTK can reuse the button handlers.
- Project templates auto-load on app startup and whenever the Projects tab becomes visible.
- UI sources are kept rustfmt-formatted to align with workspace style.
- UI log persistence now updates `ui-state.json` per AppEvent log line instead of scraping text buffers on close.

## Prioritized TODO checklist by service
(Clients list includes UI and CLI items; some references below point to crates/aadk-cli.)
- Add template/toolchain/target pickers to the Workflow page to avoid manual id entry.
- Add run filters (project/target/toolchain/result) and output shortcuts (open/export) to the Evidence dashboard.
