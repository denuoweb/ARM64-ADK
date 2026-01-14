# AADK Full Scaffold

GUI-first, multi-service gRPC scaffold for an Android DevKit style workflow. The GTK UI and CLI
are thin clients; all real work lives in the service crates. JobService is the event bus that
streams job state/progress/logs to clients.

## Architecture at a glance
- GTK4 UI and CLI call gRPC services; they do not implement business logic.
- JobService stores job records, replays history, and streams live events (including run-level aggregation).
- Toolchain/Build/Targets/Observe services create jobs and publish events to JobService.
- ProjectService is the source of truth for project metadata and template scaffolding.
- WorkflowService orchestrates multi-step pipelines and upserts run records for observability.

## Source map (main entry points)
- JobService: `crates/aadk-core/src/main.rs`
- WorkflowService: `crates/aadk-workflow/src/main.rs`
- ToolchainService: `crates/aadk-toolchain/src/main.rs`
- ProjectService: `crates/aadk-project/src/main.rs`
- BuildService: `crates/aadk-build/src/main.rs`
- TargetService: `crates/aadk-targets/src/main.rs`
- ObserveService: `crates/aadk-observe/src/main.rs`
- GTK UI: `crates/aadk-ui/src/main.rs`
- CLI: `crates/aadk-cli/src/main.rs`
- Proto contracts: `proto/aadk/v1`
- Rust gRPC types: `crates/aadk-proto`
- Dev runner: `scripts/dev/run-all.sh`
- Agent notes: `AGENTS.md` and `crates/*/AGENTS.md`

## Runtime topology
Default addresses (override via env):
- Job/Core:     127.0.0.1:50051 (AADK_JOB_ADDR)
- Toolchain:    127.0.0.1:50052 (AADK_TOOLCHAIN_ADDR)
- Project:      127.0.0.1:50053 (AADK_PROJECT_ADDR)
- Build:        127.0.0.1:50054 (AADK_BUILD_ADDR)
- Targets:      127.0.0.1:50055 (AADK_TARGETS_ADDR)
- Observe:      127.0.0.1:50056 (AADK_OBSERVE_ADDR)
- Workflow:     127.0.0.1:50057 (AADK_WORKFLOW_ADDR)

## Data and state locations
- Jobs: `~/.local/share/aadk/state/jobs.json`
- UI config: `~/.local/share/aadk/state/ui-config.json`
- CLI config: `~/.local/share/aadk/state/cli-config.json`
- Toolchains: `~/.local/share/aadk/state/toolchains.json`
- Toolchain downloads: `~/.local/share/aadk/downloads`
- Toolchain installs: `~/.local/share/aadk/toolchains`
- Projects: `~/.local/share/aadk/state/projects.json`
- Project metadata: `<project>/.aadk/project.json`
- Builds: `~/.local/share/aadk/state/builds.json`
- Observe runs: `~/.local/share/aadk/state/observe.json`
- Observe bundles: `~/.local/share/aadk/bundles`
- UI/CLI log exports: `~/.local/share/aadk/state/*-job-export-*.json`

## Third-party inventory (downloaded on demand)
This repo does not bundle third-party toolchains; services download or invoke them when requested.
- Android SDK/NDK custom archives (ToolchainService catalog in `crates/aadk-toolchain/catalog.json`,
  override with `AADK_TOOLCHAIN_CATALOG`):
  - SDK (aarch64-linux-musl):
    - 36.0.0 (2025-11-19): `https://github.com/HomuHomu833/android-sdk-custom/releases/download/36.0.0/android-sdk-aarch64-linux-musl.tar.xz`
    - 35.0.2 (2025-10-11): `https://github.com/HomuHomu833/android-sdk-custom/releases/download/35.0.2/android-sdk-aarch64-linux-musl.tar.xz`
  - SDK (aarch64_be-linux-musl):
    - 36.0.0 (2025-11-19): `https://github.com/HomuHomu833/android-sdk-custom/releases/download/36.0.0/android-sdk-aarch64_be-linux-musl.tar.xz`
    - 35.0.2 (2025-10-11): `https://github.com/HomuHomu833/android-sdk-custom/releases/download/35.0.2/android-sdk-aarch64_be-linux-musl.tar.xz`
  - NDK (aarch64-linux-musl):
    - r29 (2025-09-08): `https://github.com/HomuHomu833/android-ndk-custom/releases/download/r29/android-ndk-r29-aarch64-linux-musl.tar.xz`
    - r28c (2025-07-19): `https://github.com/HomuHomu833/android-ndk-custom/releases/download/r28/android-ndk-r28c-aarch64-linux-musl.tar.xz`
    - r27d (2025-07-19): `https://github.com/HomuHomu833/android-ndk-custom/releases/download/r27/android-ndk-r27d-aarch64-linux-musl.tar.xz`
    - r26d (2025-07-19): `https://github.com/HomuHomu833/android-ndk-custom/releases/download/r26/android-ndk-r26d-aarch64-linux-musl.tar.xz`
  - NDK (aarch64-linux-android):
    - r29 (2025-09-08): `https://github.com/HomuHomu833/android-ndk-custom/releases/download/r29/android-ndk-r29-aarch64-linux-android.tar.xz`
    - r28c (2025-07-19): `https://github.com/HomuHomu833/android-ndk-custom/releases/download/r28/android-ndk-r28c-aarch64-linux-android.tar.xz`
    - r27d (2025-07-19): `https://github.com/HomuHomu833/android-ndk-custom/releases/download/r27/android-ndk-r27d-aarch64-linux-android.tar.xz`
    - r26d (2025-07-19): `https://github.com/HomuHomu833/android-ndk-custom/releases/download/r26/android-ndk-r26d-aarch64-linux-android.tar.xz`
  - NDK (aarch64_be-linux-musl):
    - r29 (2025-09-08): `https://github.com/HomuHomu833/android-ndk-custom/releases/download/r29/android-ndk-r29-aarch64_be-linux-musl.tar.xz`
    - r28c (2025-07-19): `https://github.com/HomuHomu833/android-ndk-custom/releases/download/r28/android-ndk-r28c-aarch64_be-linux-musl.tar.xz`
    - r27d (2025-07-19): `https://github.com/HomuHomu833/android-ndk-custom/releases/download/r27/android-ndk-r27d-aarch64_be-linux-musl.tar.xz`
    - r26d (2025-07-19): `https://github.com/HomuHomu833/android-ndk-custom/releases/download/r26/android-ndk-r26d-aarch64_be-linux-musl.tar.xz`
  - These repos are MIT licensed; review upstream Android SDK/NDK terms if you plan to redistribute.
- Cuttlefish host packages from `https://us-apt.pkg.dev/projects/android-cuttlefish-artifacts`.
- Cuttlefish images from `ci.android.com` / `android-ci.googleusercontent.com`.
- Gradle via `gradlew` or system `gradle`.
- adb/platform-tools from the Android SDK or Cuttlefish host tools.

## Quick start (Debian 13 aarch64)

### 1) System dependencies
```bash
sudo apt update
sudo apt install -y \
  build-essential pkg-config \
  libgtk-4-dev \
  protobuf-compiler \
  git curl \
  xz-utils zstd
```

### 2) Rust toolchain (if needed)
```bash
curl https://sh.rustup.rs -sSf | sh
source "$HOME/.cargo/env"
rustup default stable
```

### 3) Build everything
```bash
cargo build
```

### 4) Run all services
Terminal A:
```bash
./scripts/dev/run-all.sh
```

Override any address with env vars:
```bash
AADK_JOB_ADDR=127.0.0.1:60051 ./scripts/dev/run-all.sh
```

### 5) Run the GUI
Terminal B:
```bash
cargo run -p aadk-ui
```

If you see a GTK warning about the accessibility bus on minimal installs, either install
`at-spi2-core` or suppress it for local dev:
```bash
GTK_A11Y=none cargo run -p aadk-ui
```

### 6) Optional: CLI sanity checks
```bash
cargo run -p aadk-cli -- job start-demo
cargo run -p aadk-cli -- toolchain list-providers
cargo run -p aadk-cli -- toolchain list-sets
cargo run -p aadk-cli -- targets list
cargo run -p aadk-cli -- observe list-runs
cargo run -p aadk-cli -- observe export-support
cargo run -p aadk-cli -- project use-active-defaults <project_id>
```

## What is implemented today

### JobService (aadk-core)
- Persisted job registry with bounded history, retention cleanup, and broadcast streaming.
- `include_history` replay followed by live event streaming.
- StreamRunEvents aggregates run events across jobs with bounded buffering and best-effort timestamp ordering.
- ListJobs/ListJobHistory APIs with pagination and filters (type/state/time/run_id, event kinds).
- Validates job types; demo job runner remains for smoke tests while services publish real jobs.
- Supports run_id + correlation_id grouping (StartJob + ListJobs filter) and reserves workflow.pipeline for multi-step orchestration.

### ToolchainService (aadk-toolchain)
- Provider catalog with host-aware artifacts (override via `AADK_TOOLCHAIN_CATALOG`).
- Installs, updates, uninstalls, and verifies SDK/NDK toolchains with JobService events; supports cache cleanup.
- Verification validates provenance, catalog entries, artifact size, signatures and transparency log entries (when configured), and layout; supports fixture archives via `AADK_TOOLCHAIN_FIXTURES_DIR`.

### ProjectService (aadk-project)
- Template registry backed by JSON (`AADK_PROJECT_TEMPLATES` or default registry).
- Template defaults (minSdk/compileSdk) resolved from registry/Gradle files with schema validation.
- Create/open project, scaffold files on disk, store metadata and recents.
- Exposes GetProject for authoritative project resolution.

### BuildService (aadk-build)
- Resolves project paths via ProjectService IDs (or accepts direct paths) and persists build/artifact records with module/variant/task selections.
- Runs Gradle with wrapper checks and GRADLE_USER_HOME defaults; validates module/variant via Gradle model introspection and streams logs.
- Scans build outputs for APK/AAB/AAR/mapping/test results, parses output metadata, tags metadata (module/variant/build_type/flavors/abi/density/task/artifact_type), and supports artifact filters with sha256.

### TargetService (aadk-targets)
- Enumerates targets via provider pipeline (ADB + Cuttlefish), normalizes IDs, enriches health metadata, and persists inventory + default target.
- Install APK, launch/stop app, stream logcat, and manage Cuttlefish; publishes job events.

### ObserveService (aadk-observe)
- Persists run history and paginated listing with run_id/correlation_id and project/target/toolchain ids.
- Exports support/evidence bundles as JobService jobs with progress/log streaming and retention.
- Support bundles include job log history plus config/state snapshots.
- UpsertRun supports best-effort run tracking from multi-service pipelines.

### WorkflowService (aadk-workflow)
- Runs workflow.pipeline to orchestrate project creation/opening, toolchain verify, build, install, launch, and bundle export steps.
- Emits job progress/logs for each step and waits for step jobs to complete before proceeding.
- Uses run_id to correlate jobs and upserts run records to ObserveService.

### GTK UI (aadk-ui)
- Home: run jobs with type/params/ids, watch streams, live status panel.
- Job History: list jobs and event history with filters; export logs.
- Toolchains: list/install/verify/update/uninstall, cache cleanup, list toolchain sets.
- Projects: list templates, create/open, list recents, set config, use active defaults.
- Targets: list targets, install APK, launch, logcat, Cuttlefish controls.
- Console: run Gradle builds with module/variant/task selection, list artifacts with filters grouped by module, stream logs.
- Evidence: list runs, export support/evidence bundles and stream job events.
- Toolchains/Projects/Targets/Console/Evidence pages include job_id reuse and correlation_id inputs for multi-job workflows.

### CLI (aadk-cli)
- Job run/list/watch/history/export + demo start/cancel + watch-run (aggregated run stream).
- Toolchain list-providers/list-sets/update/uninstall/cleanup-cache.
- Targets list/start/stop/status/install Cuttlefish.
- Projects list-templates/list-recent/create/open/use-active-defaults.
- Observe list-runs/export-support/export-evidence.
- Build run/list-artifacts with module/variant/tasks + artifact filters.
- Workflow run-pipeline to orchestrate multi-step flows.
- Long-running commands accept --job-id/--correlation-id/--run-id for workflow grouping.

## Extending from here (recommended order)
1. Add a GTK UI page for workflow.pipeline inputs and run stream visualization.
2. Add a RunId-aware dashboard view that groups multi-service jobs and surface bundle exports.


## Development notes
- gRPC uses TCP loopback for simplicity; Unix domain sockets are a straightforward follow-on.
- UI uses a background tokio runtime to keep the GTK main thread responsive.
- Job workflows publish progress metrics via JobService; run `aadk-core` to see UI streams.

## Why AADK (ARM64 gap)
AADK targets efficient, ARM64-first Android development tooling (not an IDE clone), because the
official Android Studio stack does not cover ARM64 hosts today.

Primary sources behind this gap:
- Android Studio Linux docs: "Linux machines with ARM-based CPUs aren't supported," and the CPU
  requirement lists "x86_64 CPU architecture." https://developer.android.com/studio/platform/install
- Google issue tracker: "Android Studio is not available on Windows Arm." https://issuetracker.google.com/issues/351408627
- Google issue tracker: "Emulator is not available on Windows/Linux arm64." https://issuetracker.google.com/issues/386749845
- .NET Android workload supports Windows ARM64, but it is a .NET stack and not a full Android Studio
  replacement. https://github.com/dotnet/android/blob/main/Documentation/guides/WindowsOnArm64.md

Community ARM64 tooling references:
- android-tools (platform tools like adb/fastboot) for aarch64 in Arch Linux ARM. https://archlinuxarm.org/packages/aarch64/android-tools
- android-sdk-tools community repo building platform-tools/build-tools with aarch64 testing. https://github.com/Lzhiyong/android-sdk-tools
- AndroidIDE tools repo (JDK + Android SDK tooling for AndroidIDE). https://github.com/AndroidIDEOfficial/androidide-tools

## Cuttlefish target provider
TargetService can surface a local Cuttlefish instance as a `provider=cuttlefish` target. It:
- uses `cvd status` when available to detect running state and adb serial
- optionally issues `adb connect` to the configured serial
- reports state from adb (`device`, `offline`) or Cuttlefish (`running`, `stopped`, `error`)
- annotates details with adb state, API level, build/branch/paths, and raw status output

Prerequisites (per Android Cuttlefish docs):
- KVM virtualization is required. Check `/dev/kvm` (or `find /dev -name kvm` on ARM64); enable nested virtualization on cloud hosts.
- Ensure the user is in `kvm`, `cvdnetwork`, and `render` groups; re-login or reboot after group changes.
- Host tools and images should come from the same build id (Install Cuttlefish enforces this).

Defaults (when not overridden):
- Branch: `aosp-android-latest-release` for 4K hosts; `main-16k-with-phones` for 16K.
- Targets: `aosp_cf_arm64_only_phone-userdebug` (ARM64), `aosp_cf_x86_64_only_phone-userdebug` (x86_64), or `aosp_cf_riscv64_phone-userdebug` (riscv64). 16K defaults to `aosp_cf_arm64` / `aosp_cf_x86_64`.

GPU acceleration:
- Android 11+ guests use accelerated graphics when the host supports it; otherwise SwiftShader is used.
- Host requirements: EGL driver with `GL_KHR_surfaceless_context`, OpenGL ES, and Vulkan support.
- Use `AADK_CUTTLEFISH_GPU_MODE=gfxstream` (OpenGL+Vulkan passthrough) or `AADK_CUTTLEFISH_GPU_MODE=drm_virgl` (OpenGL only) to set `--gpu_mode=...` when starting.

WebRTC streaming:
- Launch with `--start_webrtc=true` (TargetService sets this automatically when "show full UI" is selected; override via `AADK_CUTTLEFISH_START_ARGS`).
- Web UI is available at `https://localhost:8443` by default (`AADK_CUTTLEFISH_WEBRTC_URL` overrides).
- Remote access requires firewall access for TCP 8443 and TCP/UDP 15550-15599.

Environment control (REST/CLI):
- REST endpoint: `https://localhost:1443` (`AADK_CUTTLEFISH_ENV_URL` overrides).
- Example REST paths:
  - `GET /devices/DEVICE_ID/services`
  - `GET /devices/DEVICE_ID/services/SERVICE_NAME`
  - `POST /devices/DEVICE_ID/services/SERVICE_NAME/METHOD_NAME` with JSON-formatted proto payload.
- CLI equivalents: `cvd env ls`, `cvd env type SERVICE_NAME REQUEST_TYPE`, `cvd env call SERVICE_NAME METHOD_NAME '{...}'`.
- Services: `GnssGrpcProxy`, `OpenwrtControlService`, `WmediumdService`, `CasimirControlService`.

Wi-Fi:
- Cuttlefish uses Wmediumd to simulate wireless medium.
- Android 14+ uses `WmediumdService` (via env control REST/CLI); Android 13 or lower uses `wmediumd_control`.
- OpenWRT AP: default device id is `cvd-1`; default WAN IP is `192.168.94.2` or `192.168.96.2` when no launch options are provided.
- OpenWRT access: `ssh root@OPENWRT_WAN_IP_ADDRESS` or `https://localhost:1443/devices/DEVICE_ID/openwrt`.

Bluetooth:
- Rootcanal is controlled from the Web UI command console.
- Commands: `list`, `add DEVICE_TYPE [ARGS]`, `del DEVICE_INDEX`, `add_phy PHY_TYPE`, `del_phy PHY_INDEX`,
  `add_device_to_phy DEVICE_INDEX PHY_INDEX`, `del_device_from_phy DEVICE_INDEX PHY_INDEX`,
  `add_remote HOSTNAME PORT PHY_TYPE`.
- Device types: `beacon`, `scripted_beacon`, `keyboard`, `loopback`, `sniffer`.

Configuration (env vars):
- `AADK_CUTTLEFISH_ENABLE=0` to disable detection
- `AADK_CVD_BIN=/path/to/cvd` to override the `cvd` command
- `AADK_LAUNCH_CVD_BIN=/path/to/launch_cvd` to override the launch command
- `AADK_STOP_CVD_BIN=/path/to/stop_cvd` to override the stop command
- `AADK_CUTTLEFISH_ADB_SERIAL=127.0.0.1:6520` to override the adb serial
- `AADK_CUTTLEFISH_CONNECT=0` to skip `adb connect`
- `AADK_CUTTLEFISH_WEBRTC_URL=https://localhost:8443` to override the WebRTC viewer URL
- `AADK_CUTTLEFISH_ENV_URL=https://localhost:1443` to override the environment control endpoint
- `AADK_CUTTLEFISH_PAGE_SIZE_CHECK=0` to skip the kernel page-size preflight check
- `AADK_CUTTLEFISH_KVM_CHECK=0` to skip the KVM availability/access check
- `AADK_CUTTLEFISH_GPU_MODE=gfxstream|drm_virgl` to set the GPU acceleration mode
- `AADK_CUTTLEFISH_HOME=/path` (or `_16K`/`_4K`) to set the base Cuttlefish home directory
- `AADK_CUTTLEFISH_IMAGES_DIR=/path` (or `_16K`/`_4K`) to override the images directory
- `AADK_CUTTLEFISH_HOST_DIR=/path` (or `_16K`/`_4K`) to override the host tools directory
- `AADK_CUTTLEFISH_START_CMD="..."` to override the start command
- `AADK_CUTTLEFISH_START_ARGS="..."` to append args to `cvd start`/`launch_cvd`
- `AADK_CUTTLEFISH_STOP_CMD="..."` to override the stop command
- `AADK_CUTTLEFISH_INSTALL_CMD="..."` to override the host install command
- `AADK_CUTTLEFISH_INSTALL_HOST=0` to skip host package installation
- `AADK_CUTTLEFISH_INSTALL_IMAGES=0` to skip image downloads
- `AADK_CUTTLEFISH_ADD_GROUPS=0` to skip adding the user to kvm/cvdnetwork/render
- `AADK_CUTTLEFISH_BRANCH=<branch>` (or `_16K`/`_4K`) to override the AOSP branch used for image fetch
- `AADK_CUTTLEFISH_TARGET=<target>` (or `_16K`/`_4K`) to override the AOSP target used for image fetch
- `AADK_CUTTLEFISH_BUILD_ID=<id>` to pin a specific AOSP build id
- `AADK_ADB_PATH` or `ANDROID_SDK_ROOT` to locate `adb`

### Pinning Cuttlefish builds
To pin images to a known build, set `AADK_CUTTLEFISH_BUILD_ID` (optionally with branch/target):
```bash
AADK_CUTTLEFISH_BRANCH=aosp-android-latest-release \
AADK_CUTTLEFISH_TARGET=aosp_cf_arm64_only_phone-userdebug \
AADK_CUTTLEFISH_BUILD_ID=12345678 \
./scripts/dev/run-all.sh
```
The GTK UI also exposes branch/target/build id fields plus a "Resolve Build ID" button so you can
confirm the resolved build before running Install Cuttlefish.

Notes:
- The UI and CLI pass `include_offline=true`, so a stopped Cuttlefish instance still appears.
- Start/stop/status actions are exposed in the UI and CLI (Targets -> Start/Stop/Status Cuttlefish).
- Install Cuttlefish uses public artifacts from `ci.android.com`.
