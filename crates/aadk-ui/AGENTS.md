# GTK UI Agent Notes (aadk-ui)

## Role and scope
The GTK4 UI is a thin client for the gRPC services. It provides pages for Home (demo job),
Toolchains, Projects, Targets, Console (build), and Evidence. It uses a background tokio runtime
thread to keep the GTK main thread responsive.

## Maintenance
Update this file whenever UI behavior changes or when commits touching this crate are made.

## Key implementation details
- Implementation lives in crates/aadk-ui/src/main.rs.
- AppConfig holds service addresses and pulls defaults from env:
  AADK_JOB_ADDR, AADK_TOOLCHAIN_ADDR, AADK_PROJECT_ADDR, AADK_BUILD_ADDR,
  AADK_TARGETS_ADDR, AADK_OBSERVE_ADDR.
- UiCommand enum defines all async work; a background worker executes these commands and emits
  AppEvent logs to update the UI.
- The UI mostly logs results rather than rendering structured data; it is intentionally minimal.

## Service coverage
- Home: starts demo job and streams events from JobService.
- Toolchains: list providers, install, verify, list installed.
- Projects: list templates only.
- Targets: list targets, install/start/stop Cuttlefish, logcat, install APK, launch app.
- Console: run builds via BuildService, stream job events.
- Evidence: list runs, export support bundles, export evidence bundles, stream job events.

## Environment / config
- All service addresses can be overridden via env vars listed above.
- If running on minimal GTK installs, GTK_A11Y=none can suppress the accessibility warning.

## Prioritized TODO checklist by service
(Clients list includes UI and CLI items; some references below point to crates/aadk-cli.)
- P0: Add project create/open flows with template selection and errors. main.rs (line 436) main.rs (line 946)
- P0: Replace demo-job UI/CLI with real job type selection/status views. main.rs (line 790) main.rs (line 41)
- P1: Add project recent list + job history viewer. main.rs (line 157)
- P1: Add toolchain set management UI/CLI commands. main.rs (line 829) main.rs (line 55)
- P2: Persist UI config (service addresses) and export logs. main.rs (line 34)
