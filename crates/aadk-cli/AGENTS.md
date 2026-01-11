# CLI Agent Notes (aadk-cli)

## Role and scope
The CLI is a lightweight sanity tool for the gRPC services. It exposes a small set of commands
for demo jobs, toolchain listing, and target operations. It is intentionally minimal and does
not yet cover project, build, or observe workflows.

## Key implementation details
- Implementation lives in crates/aadk-cli/src/main.rs.
- Uses clap for subcommands:
  - job start-demo / cancel
  - toolchain list-providers
  - targets list / start-cuttlefish / stop-cuttlefish / cuttlefish-status / install-cuttlefish
- Each command connects directly to the target gRPC service using the default address env var.

## Environment / config
- AADK_JOB_ADDR, AADK_TOOLCHAIN_ADDR, AADK_TARGETS_ADDR (defaults to 127.0.0.1 ports).

## Prioritized TODO checklist by service
(Clients list includes UI and CLI items; some references below point to crates/aadk-ui.)
- P0: Add project create/open flows with template selection and errors. main.rs (line 436) main.rs (line 946)
- P0: Replace demo-job UI/CLI with real job type selection/status views. main.rs (line 790) main.rs (line 41)
- P1: Add project recent list + job history viewer. main.rs (line 157)
- P1: Add toolchain set management UI/CLI commands. main.rs (line 829) main.rs (line 55)
- P1: Add observe run list + bundle export UI/CLI. main.rs (line 70) main.rs (line 21)
- P2: Persist UI config (service addresses) and export logs. main.rs (line 34)
