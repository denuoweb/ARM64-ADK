# CLI Agent Notes (aadk-cli)

## Role and scope
The CLI is a lightweight sanity tool for the gRPC services. It exposes a small set of commands
for demo jobs, toolchains, targets, projects, and observe workflows; build support is still
minimal.

## Maintenance
Update this file whenever CLI behavior changes or when commits touching this crate are made.

## Key implementation details
- Implementation lives in crates/aadk-cli/src/main.rs.
- Uses clap for subcommands:
  - job start-demo / cancel
  - toolchain list-providers / list-sets / create-set / set-active / get-active
  - targets list / set-default / get-default / start-cuttlefish / stop-cuttlefish / cuttlefish-status / install-cuttlefish
  - project list-templates / list-recent / create / open / set-config / use-active-defaults
  - observe list-runs / export-support / export-evidence
- Each command connects directly to the target gRPC service using the default address env var.
- Observe export commands include optional metadata fields (project/target/toolchain ids), currently unset.

## Environment / config
- AADK_JOB_ADDR, AADK_TOOLCHAIN_ADDR, AADK_PROJECT_ADDR, AADK_TARGETS_ADDR, AADK_OBSERVE_ADDR (defaults to 127.0.0.1 ports).

## Prioritized TODO checklist by service
(Clients list includes UI and CLI items; some references below point to crates/aadk-ui.)
- P0: Replace demo-job UI/CLI with real job type selection/status views. main.rs (line 790) main.rs (line 41)
- P1: Add project recent list + job history viewer. main.rs (line 157)
- P2: Persist UI config (service addresses) and export logs. main.rs (line 34)
