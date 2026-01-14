# AADK Full Scaffold - Agent Overview

## Purpose
This repository is a GUI-first, multi-service gRPC scaffold for an Android DevKit style workflow.
It is intentionally minimal but complete enough to extend. The GTK4 UI and CLI are thin clients;
the service crates contain the real workflows. The project is designed around a JobService that
streams events to clients while long-running jobs execute in other services.

## Supported host
- Linux ARM64 (aarch64) only.
- Other host architectures are not supported; x86_64 is intentionally out of scope because Android Studio already covers it.
- Toolchain catalog entries and Cuttlefish host tooling are pinned to ARM64 hosts.

## Maintenance
Keep this file and the per-service AGENTS.md files in sync with code changes. When Codex changes
files, commits, or pushes, update the relevant AGENTS.md entries and adjust TODO lists to remove
completed items or move them into the implementation notes.

## Repository map
- crates/aadk-core: JobService (event streaming and job registry)
- crates/aadk-workflow: WorkflowService (multi-step pipeline orchestration)
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
- Workflow:     127.0.0.1:50057 (AADK_WORKFLOW_ADDR)

## Cross-service flows
- UI/CLI call gRPC services; they do not implement business logic.
- JobService is the event bus. Toolchain/Build/Targets publish job events; UI streams events.
- Progress payloads include job-specific metrics for build/project/toolchain/target/observe workflows.
- BuildService resolves project paths via ProjectService GetProject for ids; direct paths are accepted.
- BuildService loads a Gradle model snapshot after project evaluation to validate modules/variants.
- TargetService shells out to adb and optionally Cuttlefish tooling (KVM/GPU preflight, WebRTC defaults, env-control URLs).
- ToolchainService downloads SDK/NDK archives, verifies sha256, persists state under ~/.local/share/aadk.
- ToolchainService catalog pins linux-aarch64 SDK 36.0.0/35.0.2 and NDK r29/r28c/r27d/r26d for the
  custom providers, with aarch64-linux-musl, aarch64-linux-android, and aarch64_be-linux-musl
  artifacts where available.
- ObserveService persists run history and uses JobService history plus state/env snapshots for bundle contents.
- RunId is a first-class identifier for multi-service workflows; correlation_id remains a secondary grouping key.
- JobService StreamRunEvents aggregates run events across jobs using bounded buffering and best-effort timestamp ordering for late discovery.
- WorkflowService orchestrates workflow.pipeline runs and upserts run records to ObserveService.

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
- crates/aadk-workflow/AGENTS.md
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

### TargetService (aadk-targets)

### Clients (aadk-ui, aadk-cli)
- GTK UI now includes per-tab overview/connection copy and verbose tooltips for inputs and selections.
- Core flow actions in the UI surface connection/RPC failures in the page log so bad inputs are visible.
- Sidebar order is Job Control, Toolchains, Projects, Build, Targets, Job History, Evidence, Settings (Home -> Job Control, Console -> Build).
Completed job flow expansions are tracked in README.
