# AADK Full Scaffold - Agent Overview

## Purpose
This repository is a GUI-first, multi-service gRPC scaffold for an Android DevKit style workflow.
It is intentionally minimal but complete enough to extend. The GTK4 UI and CLI are thin clients;
the service crates contain the real workflows. The project is designed around a JobService that
streams events to clients while long-running jobs execute in other services.

## Supported host
- Linux ARM64 (aarch64) is the only supported host for running the full stack (services/UI/Cuttlefish).
- x86_64 is intentionally out of scope because Android Studio already covers it.
- Toolchain catalog includes Linux ARM64 SDK/NDK artifacts plus Windows ARM64 NDK artifacts (r29/r28c/r27d);
  no darwin SDK/NDK artifacts are published in the custom catalogs.
- Cuttlefish install uses AADK_CUTTLEFISH_INSTALL_CMD when set; Debian-like hosts fall back to the
  android-cuttlefish apt repo install command.

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
- crates/aadk-util: Shared helpers (paths, time, service bootstrap, job history)
- crates/aadk-telemetry: Opt-in telemetry spooler (usage events + crash reports)
- crates/aadk-proto: Rust gRPC codegen for proto/aadk/v1
- proto/aadk/v1/*.proto: gRPC contracts
- scripts/dev/run-all.sh: local dev runner for all services (auto-exports ANDROID_SDK_ROOT/ANDROID_HOME and AADK_ADB_PATH when an SDK is detected)
- scripts/release/build.sh: release build + packaging helper
- scripts/release/aadk-start.sh: installed launcher (services + UI, logs to ~/.local/share/aadk/logs)
- docs/release.md: release build steps
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
- TargetService shells out to adb and optionally Cuttlefish tooling (KVM/GPU preflight, headless WebRTC fallback, images-dir fallback logging, env-control URLs).
- ToolchainService downloads SDK/NDK archives, verifies sha256, persists state under ~/.local/share/aadk.
- ToolchainService normalizes SDK cmdline-tools layout by creating cmdline-tools/latest links when
  archives ship a flat cmdline-tools/bin + lib layout.
- ToolchainService catalog pins SDK 36.0.0/35.0.2 and NDK r29/r28c/r27d/r26d; Linux ARM64 artifacts use
  linux-aarch64/aarch64-linux-android/aarch64_be-linux-musl, and Windows ARM64 NDK artifacts are included
  for r29/r28c/r27d (.7z).
- ObserveService persists run history and a run output inventory; run records include a summary pointer for output counts and last bundle.
- RunId is a first-class identifier for multi-service workflows; correlation_id remains a secondary grouping key.
- JobService StreamRunEvents aggregates run events across jobs using bounded buffering and best-effort timestamp ordering for late discovery.
- WorkflowService orchestrates workflow.pipeline runs and upserts run records to ObserveService.
- UI/CLI can export/import local state archives and invoke ReloadState RPCs to rehydrate service state after import.
- UI header New project runs reset-all-state then opens the project folder picker; Open project uses the picker and auto-opens existing projects.

## Shared data and locations
- Job state: ~/.local/share/aadk/state/jobs.json
- UI config: ~/.local/share/aadk/state/ui-config.json
- UI state: ~/.local/share/aadk/state/ui-state.json
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
- State export archives: ~/.local/share/aadk/state-exports/*.zip
- State operation queue/locks: ~/.local/share/aadk/state-ops
- Telemetry events/crashes: ~/.local/share/aadk/telemetry/<app>/*

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
- crates/aadk-telemetry/AGENTS.md
- proto/aadk/v1/AGENTS.md

## Prioritized TODO checklist by service

### ProjectService (aadk-project)
- None (workflow UI relies on existing create/open APIs).

### JobService (aadk-core)
- None (StreamRunEvents already supports workflow/run dashboards).

### ObserveService (aadk-observe)
- None (run output inventory is stored and exposed via ListRunOutputs with summary pointers).

### BuildService (aadk-build)
- None.

### ToolchainService (aadk-toolchain)
- None.

### TargetService (aadk-targets)
- None.

### Clients (aadk-ui, aadk-cli)
- Add picker integrations for workflow inputs (templates, toolchain sets, targets) to reduce manual ids.
- Add run filters (project/target/toolchain/result) and output shortcuts (open/export) to Evidence dashboards.
