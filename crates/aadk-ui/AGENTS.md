# GTK UI Agent Notes (aadk-ui)

## Role and scope
The GTK4 UI is a thin client for the gRPC services. It provides pages for Job Control (job run/status),
Job History, Toolchains, Projects, Targets, Build, and Evidence. It uses a background tokio runtime
thread to keep the GTK main thread responsive.

## Maintenance
Update this file whenever UI behavior changes or when commits touching this crate are made.

## Key implementation details
- Implementation lives in crates/aadk-ui/src/main.rs.
- AppConfig holds service addresses and pulls defaults from env:
  AADK_JOB_ADDR, AADK_TOOLCHAIN_ADDR, AADK_PROJECT_ADDR, AADK_BUILD_ADDR,
  AADK_TARGETS_ADDR, AADK_OBSERVE_ADDR, AADK_WORKFLOW_ADDR.
- AppConfig persists to `~/.local/share/aadk/state/ui-config.json` with last job selections.
- UiCommand enum defines all async work; a background worker executes these commands and emits
  AppEvent logs to update the UI.
- The UI mostly logs results rather than rendering structured data; it is intentionally minimal.
- Core flow actions log connection/RPC failures to the page output so bad inputs and service errors are visible.
- Page construction now includes a per-tab header, overview, and connections blurb; control layouts insert after the intro block.
- All interactive fields and selections include verbose tooltips describing what, why, and how to use them.
- Sidebar order is Job Control, Toolchains, Projects, Build, Targets, Job History, Evidence, Settings (Home -> Job Control, Console -> Build).
- Toolchains page fetches available SDK/NDK versions and populates dropdowns; install/verify actions
  use the selected version and default to the latest SDK_VERSION/NDK_VERSION.
- The Cuttlefish docs button opens https://source.android.com/docs/devices/cuttlefish/get-started.
- The Targets page includes an "Open Cuttlefish Env" button using AADK_CUTTLEFISH_ENV_URL (default https://localhost:1443).
- Observe export requests include optional metadata fields (project/target/toolchain ids), currently unset in the UI.
- Toolchains/Projects/Targets/Build/Evidence pages include a "Use job id" toggle plus
  correlation id entry to attach work to existing jobs and grouped workflows; the UI derives run_id
  from correlation_id for run-aware services.

## Service coverage
- Job Control: start arbitrary jobs (including workflow.pipeline) with params/ids + optional correlation id, watch job streams, live status panel.
- Job History: list jobs and event history with filters; export logs.
- Toolchains: list providers/available/installed/sets, install/verify, update/uninstall, cache cleanup, create/set/get toolchain sets.
- Projects: list templates, create/open, list recent, set project config, use active defaults.
- Targets: list targets, set/get default target, resolve Cuttlefish build ids, install/start/stop Cuttlefish, logcat, install APK, launch app.
- Build: run builds via BuildService with module/variant_name/tasks overrides, list artifacts with filters (grouped by module), stream job events.
- Evidence: list runs, export support bundles, export evidence bundles, stream job events.

## Environment / config
- All service addresses can be overridden via env vars listed above.
- If running on minimal GTK installs, GTK_A11Y=none can suppress the accessibility warning.

## Implementation notes
- Job Control page event routing now keeps the HomePage handle so status labels and log output update together.

## Prioritized TODO checklist by service
(Clients list includes UI and CLI items; some references below point to crates/aadk-cli.)
Completed UI job flow expansions are tracked in README.
