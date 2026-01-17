# CLI Agent Notes (aadk-cli)

## Role and scope
The CLI is a lightweight sanity tool for the gRPC services. It exposes commands for job run/list/watch/history/export,
toolchains, targets, projects, observe workflows, and basic BuildService operations.

## Maintenance
Update this file whenever CLI behavior changes or when commits touching this crate are made.

## Key implementation details
- Implementation lives in crates/aadk-cli/src/main.rs.
- Uses clap for subcommands:
  - job run / list / watch / history / export / cancel
  - job watch-run (aggregated run stream by run_id/correlation_id)
  - toolchain list-providers / list-sets / create-set / set-active / get-active / update / uninstall / cleanup-cache
  - targets list / set-default / get-default / start-cuttlefish / stop-cuttlefish / cuttlefish-status / install-cuttlefish
  - project list-templates / list-recent / create / open / set-config / use-active-defaults
  - observe list-runs / list-outputs / export-support / export-evidence
  - build run / list-artifacts (module/variant_name/tasks + artifact filters, grouped by module)
  - workflow run-pipeline (multi-step orchestration with optional explicit steps)
  - state save / open / reload (export/import local state archives with exclusion flags)
- Each command connects directly to the target gRPC service using env/config defaults.
- Job export helpers and shared data-dir defaults are provided by `aadk-util`.
- CLI config persists to `~/.local/share/aadk/state/cli-config.json` with last job selections.
- Telemetry is opt-in via env (AADK_TELEMETRY, AADK_TELEMETRY_CRASH) and emits cli.command.* events.
- Observe export commands include optional metadata fields (project/target/toolchain ids), currently unset.
- Long-running commands accept --job-id, --correlation-id, and --run-id to attach to existing jobs and group workflows; job list filters can target correlation_id/run_id.
- State save/open commands block if the latest job is queued/running and serialize via ~/.local/share/aadk/state-ops; open reloads all services via ReloadState.
- Unused imports are cleaned in `main.rs` to keep CLI builds warning-free.
- Sources are kept rustfmt-formatted to align with workspace style.

## Environment / config
- AADK_JOB_ADDR, AADK_TOOLCHAIN_ADDR, AADK_PROJECT_ADDR, AADK_BUILD_ADDR, AADK_TARGETS_ADDR,
  AADK_OBSERVE_ADDR, AADK_WORKFLOW_ADDR (defaults to 127.0.0.1 ports).
- AADK_TELEMETRY and AADK_TELEMETRY_CRASH enable usage/crash reporting (local spool).

## Prioritized TODO checklist by service
(Clients list includes UI and CLI items; some references below point to crates/aadk-ui.)
- Add a workflow run helper that tails StreamRunEvents after run-pipeline.
