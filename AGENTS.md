# AADK Full Scaffold - Agent Overview

## Purpose
This repository is a GUI-first, multi-service gRPC scaffold for an Android DevKit style workflow.
It is intentionally minimal but complete enough to extend. The GTK4 UI and CLI are thin clients;
the service crates contain the real workflows. The project is designed around a JobService that
streams events to clients while long-running jobs execute in other services.

## Maintenance
Keep this file and the per-service AGENTS.md files in sync with code changes. When Codex changes
files, commits, or pushes, update the relevant AGENTS.md entries and adjust TODO lists to remove
completed items or move them into the implementation notes.

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
- SampleConsole: Minimal Compose sample app (Sample Console) bundled with AADK
- CS492_Assignment1_RosenauJ/CS492A1RosenauJ: Course assignment sample app

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
- Progress payloads include job-specific metrics for build/project/toolchain/target/observe workflows.
- BuildService resolves project paths via ProjectService GetProject for ids; direct paths are accepted.
- TargetService shells out to adb and optionally Cuttlefish tooling (KVM/GPU preflight, WebRTC defaults, env-control URLs).
- ToolchainService downloads SDK/NDK archives, verifies sha256, persists state under ~/.local/share/aadk.
- ObserveService persists run history and uses JobService history plus state/env snapshots for bundle contents.

## Shared data and locations
- Job state: ~/.local/share/aadk/state/jobs.json
- UI config: ~/.local/share/aadk/state/ui-config.json
- CLI config: ~/.local/share/aadk/state/cli-config.json
- Project state: ~/.local/share/aadk/state/projects.json
- Build state: ~/.local/share/aadk/state/builds.json
- Toolchain state: ~/.local/share/aadk/state/toolchains.json
- Toolchain downloads: ~/.local/share/aadk/downloads
- Toolchain installs: ~/.local/share/aadk/toolchains
- Target state: ~/.local/share/aadk/state/targets.json
- Observe state: ~/.local/share/aadk/state/observe.json
- Observe bundle outputs: ~/.local/share/aadk/bundles
- UI/CLI log exports: ~/.local/share/aadk/state/*-job-export-*.json

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

### JobService (aadk-core)

### ObserveService (aadk-observe)

### BuildService (aadk-build)

### ToolchainService (aadk-toolchain)
- P2: Add transparency log validation for toolchain artifacts. main.rs (line 1108)

### TargetService (aadk-targets)

### Clients (aadk-ui, aadk-cli)
Completed job flow expansions are tracked in README.
