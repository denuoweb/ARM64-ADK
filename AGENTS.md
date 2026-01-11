# AADK Full Scaffold - Agent Overview

## Purpose
This repository is a GUI-first, multi-service gRPC scaffold for an Android DevKit style workflow.
It is intentionally minimal but complete enough to extend. The GTK4 UI and CLI are thin clients;
the service crates contain the real workflows. The project is designed around a JobService that
streams events to clients while long-running jobs execute in other services.

## Repository map
- crates/aadk-core: JobService (event streaming and job registry)
- crates/aadk-toolchain: ToolchainService (SDK/NDK provider, install, verify)
- crates/aadk-project: ProjectService (templates, create/open, recent list)
- crates/aadk-build: BuildService (Gradle builds and artifact listing)
- crates/aadk-targets: TargetService (ADB + Cuttlefish target management)
- crates/aadk-observe: ObserveService (run history and bundle export)
- crates/aadk-ui: GTK4 GUI client
- crates/aadk-cli: CLI sanity tool
- crates/aadk-proto: Rust gRPC codegen for proto/aadk/v1
- proto/aadk/v1/*.proto: gRPC contracts
- scripts/dev/run-all.sh: local dev runner for all services

## Runtime topology
Default addresses (override with env vars):
- Job/Core:     127.0.0.1:50051 (AADK_JOB_ADDR)
- Toolchain:    127.0.0.1:50052 (AADK_TOOLCHAIN_ADDR)
- Project:      127.0.0.1:50053 (AADK_PROJECT_ADDR)
- Build:        127.0.0.1:50054 (AADK_BUILD_ADDR)
- Targets:      127.0.0.1:50055 (AADK_TARGETS_ADDR)
- Observe:      127.0.0.1:50056 (AADK_OBSERVE_ADDR)

## Cross-service flows
- UI/CLI call gRPC services; they do not implement business logic.
- JobService is the event bus. Toolchain/Build/Targets publish job events; UI streams events.
- BuildService resolves project paths via ProjectService (recent list) or AADK_PROJECT_ROOT.
- TargetService shells out to adb and optionally Cuttlefish tooling.
- ToolchainService downloads SDK/NDK archives, verifies sha256, persists state under ~/.local/share/aadk.
- ObserveService is currently a placeholder; it should eventually use JobService and persist run history.

## Shared data and locations
- Toolchain state: ~/.local/share/aadk/state/toolchains.json
- Toolchain downloads: ~/.local/share/aadk/downloads
- Toolchain installs: ~/.local/share/aadk/toolchains
- Observe bundle outputs currently under /tmp (placeholder behavior)

## Per-service AGENT files
- crates/aadk-project/AGENTS.md
- crates/aadk-core/AGENTS.md
- crates/aadk-observe/AGENTS.md
- crates/aadk-build/AGENTS.md
- crates/aadk-toolchain/AGENTS.md
- crates/aadk-targets/AGENTS.md
- crates/aadk-ui/AGENTS.md
- crates/aadk-cli/AGENTS.md
- proto/aadk/v1/AGENTS.md

## Prioritized TODO checklist by service

### ProjectService (aadk-project)
- P0: Replace hard-coded templates with a real template registry + validation. main.rs (line 32) main.rs (line 46)
- P0: Implement create_project to scaffold files on disk, handle collisions, and return real job tracking. main.rs (line 55)
- P0: Implement open_project to read actual metadata/config instead of fabricating IDs. main.rs (line 82)
- P0: Persist projects/recent list to durable storage and support paging tokens. main.rs (line 110)
- P1: Persist set_project_config (toolchain/target defaults) with input validation. main.rs (line 121)
- P2: Add template defaults resolution (minSdk/compileSdk) with schema errors. main.rs (line 32)

### JobService (aadk-core)
- P0: Replace demo-only job flow with real job dispatch (build/toolchain/targets/observe). main.rs (line 193)
- P0: Propagate cancellation to actual workers, not just demo jobs. main.rs (line 118) main.rs (line 257)
- P1: Persist job state/history across restarts. main.rs (line 35)
- P1: Return explicit errors for unknown job types instead of leaving them queued. main.rs (line 236)
- P2: Add retention/cleanup policy for job history. main.rs (line 84)
- P2: Expand progress/metrics payloads per job type. main.rs (line 100)

### ObserveService (aadk-observe)
- P0: Implement real run storage + pagination for list_runs. main.rs (line 17)
- P0: Generate real support/evidence bundles (zip actual files). main.rs (line 24) main.rs (line 35)
- P0: Execute bundle work through JobService with progress/log streaming. main.rs (line 24)
- P1: Capture run metadata (project/target/timestamps) for filtering. main.rs (line 17)
- P2: Add retention/cleanup for bundles and temp dirs. main.rs (line 24)

### BuildService (aadk-build)
- P0: Make project resolution authoritative (real ProjectService IDs) with robust error mapping. main.rs (line 242)
- P0: Populate artifact hashes instead of empty sha256. main.rs (line 516)
- P1: Persist build records/artifacts per job so list_artifacts is backed by stored results. main.rs (line 800)
- P1: Harden Gradle invocation (wrapper detection, env setup, error surfacing). main.rs (line 529)
- P2: Expand variant/module support and artifact filtering. main.rs (line 293)

### ToolchainService (aadk-toolchain)
- P0: Replace fixed providers/versions with provider discovery + version catalog. main.rs (line 162) main.rs (line 1163)
- P0: Expand host support beyond linux/aarch64; add fallback behavior. main.rs (line 224) main.rs (line 263)
- P1: Persist toolchain sets and reference them in set_active. main.rs (line 1496) main.rs (line 1511)
- P1: Add uninstall/update operations and cached-artifact cleanup. main.rs (line 1161)
- P2: Strengthen verification (signatures/provenance, not just hash). main.rs (line 1263)

### TargetService (aadk-targets)
- P0: Persist default target across restarts (currently in-memory). main.rs (line 2818)
- P0: Add provider abstraction beyond adb/cuttlefish discovery. main.rs (line 1193)
- P1: Enrich target metadata/health reporting across providers. main.rs (line 1074)
- P1: Normalize/validate target IDs and address formats consistently. main.rs (line 1050)
- P2: Add target inventory persistence and reconciliation on startup. main.rs (line 2809)

### Clients (aadk-ui, aadk-cli)
- P0: Add project create/open flows with template selection and errors. main.rs (line 436) main.rs (line 946)
- P0: Replace demo-job UI/CLI with real job type selection/status views. main.rs (line 790) main.rs (line 41)
- P1: Add project recent list + job history viewer. main.rs (line 157)
- P1: Add toolchain set management UI/CLI commands. main.rs (line 829) main.rs (line 55)
- P1: Add observe run list + bundle export UI/CLI. main.rs (line 70) main.rs (line 21)
- P2: Persist UI config (service addresses) and export logs. main.rs (line 34)
