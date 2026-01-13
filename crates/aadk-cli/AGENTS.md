# CLI Agent Notes (aadk-cli)

## Role and scope
The CLI is a lightweight sanity tool for the gRPC services. It exposes commands for job run/list/watch/history/export,
toolchains, targets, projects, observe workflows, and basic BuildService operations.

## Maintenance
Update this file whenever CLI behavior changes or when commits touching this crate are made.

## Key implementation details
- Implementation lives in crates/aadk-cli/src/main.rs.
- Uses clap for subcommands:
  - job start-demo / run / list / watch / history / export / cancel
  - toolchain list-providers / list-sets / create-set / set-active / get-active / update / uninstall / cleanup-cache
  - targets list / set-default / get-default / start-cuttlefish / stop-cuttlefish / cuttlefish-status / install-cuttlefish
  - project list-templates / list-recent / create / open / set-config / use-active-defaults
  - observe list-runs / export-support / export-evidence
  - build run / list-artifacts (module/variant_name/tasks + artifact filters, grouped by module)
- Each command connects directly to the target gRPC service using env/config defaults.
- CLI config persists to `~/.local/share/aadk/state/cli-config.json` with last job selections.
- Observe export commands include optional metadata fields (project/target/toolchain ids), currently unset.
- Long-running commands accept --job-id and --correlation-id to attach to existing jobs and group workflows; job list filters can target correlation_id.

## Environment / config
- AADK_JOB_ADDR, AADK_TOOLCHAIN_ADDR, AADK_PROJECT_ADDR, AADK_BUILD_ADDR, AADK_TARGETS_ADDR, AADK_OBSERVE_ADDR (defaults to 127.0.0.1 ports).

## Prioritized TODO checklist by service
(Clients list includes UI and CLI items; some references below point to crates/aadk-ui.)
Completed UI/CLI job flow expansions are tracked in README.
