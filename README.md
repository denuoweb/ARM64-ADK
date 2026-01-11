# AADK Full Scaffold (GUI-first, multi-service skeleton)

This is a **full scaffold** for an ARM64-friendly Android DevKit architecture:
- **GTK4 GUI** (thin client)
- **Multiple gRPC services** (Job, Toolchain, Project, Build, Targets, Observe)
- **Protobuf contracts** under `proto/aadk/v1/`

This scaffold is intentionally **minimal but complete**:
- The services return **real structured responses** for listing operations.
- **JobService** is fully implemented with:
  - `tokio::sync::broadcast::Sender<JobEvent>` per job
  - bounded replay `history: VecDeque<JobEvent>`
  - `include_history` replay + live streaming
  - cancellation via `watch`
- Toolchain install/verify, build execution, and target operations run real workflows and publish job/log events.
- ProjectService scaffolds from templates, persists recents, and writes per-project metadata.
- ObserveService persists run history and exports support/evidence bundles via JobService (log capture is
  still placeholder).

The goal is to give you a correct spine to extend into:
- toolchain installation (android-ndk-custom, android-sdk-custom)
- gradle builds
- adb target install/launch/logcat
- evidence/support bundles

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

Default service addresses:
- Job/Core:     127.0.0.1:50051
- Toolchain:    127.0.0.1:50052
- Project:      127.0.0.1:50053
- Build:        127.0.0.1:50054
- Targets:      127.0.0.1:50055
- Observe:      127.0.0.1:50056

Override any address with env vars, e.g.:
```bash
AADK_JOB_ADDR=127.0.0.1:60051 ./scripts/dev/run-all.sh
```

### 5) Run the GUI
Terminal B:
```bash
cargo run -p aadk-ui
```

If you see a GTK warning about the accessibility bus on minimal installs, either install `at-spi2-core`
or suppress it for local dev:
```bash
GTK_A11Y=none cargo run -p aadk-ui
```

In the GUI:
- **Home**: start a demo job + stream events
- **Toolchains**: list providers and install/verify SDK/NDK
- **Projects**: list templates, create/open projects, and list recents
- **Targets**: list targets from ADB + Cuttlefish and stream logcat
- **Console**: run Gradle builds and stream output
- **Evidence**: list runs, export support bundles, and export evidence bundles (job streaming)

### 6) Optional: CLI sanity checks
```bash
cargo run -p aadk-cli -- job start-demo
cargo run -p aadk-cli -- toolchain list-providers
cargo run -p aadk-cli -- targets list
cargo run -p aadk-cli -- observe list-runs
cargo run -p aadk-cli -- observe export-support
```

## Extending from here (recommended order)
1. Replace demo-only job dispatch in `aadk-core` with real worker routing + cancellation.
2. Harden BuildService (authoritative project resolution, artifact persistence, Gradle wrapper checks).
3. Expand ToolchainService providers/versions and host support.
4. Add TargetService provider abstraction and default target persistence.
5. Enrich ObserveService metadata collection and retention/cleanup.

## Development notes
- gRPC currently uses TCP loopback for simplicity.
  Switching to **Unix domain sockets** is a straightforward follow-on.
- The UI uses a background tokio runtime thread to avoid blocking the GTK main thread.
- Log display uses a **bounded buffer** and can be replaced with a fully virtualized widget later.
- ToolchainService downloads real SDK/NDK archives, verifies sha256, caches under `~/.local/share/aadk/downloads`,
  and installs under `~/.local/share/aadk/toolchains`.
- ProjectService stores recents under `~/.local/share/aadk/state/projects.json` and writes metadata to
  `<project>/.aadk/project.json`. Template registry comes from `AADK_PROJECT_TEMPLATES` or
  `crates/aadk-project/templates/registry.json`.
- ObserveService stores runs under `~/.local/share/aadk/state/observe.json` and writes bundles to
  `~/.local/share/aadk/bundles`.
- For offline dev, set `AADK_TOOLCHAIN_FIXTURES_DIR=/path/to/fixtures` to use local archives (`.tar.xz` or `.tar.zst`).
- Host selection uses Rust's `std::env::consts` values (`aarch64`) to choose the correct archive.
- Toolchain install/verify publishes progress via JobService, so `aadk-core` must be running for UI job streams.

## Cuttlefish target provider
TargetService can surface a local Cuttlefish instance as a `provider=cuttlefish` target. It:
- uses `cvd status` when available to detect running state + adb serial
- optionally issues `adb connect` to the configured serial
- reports state from adb (`device`, `offline`) or Cuttlefish (`running`, `stopped`, `error`)
- annotates details with adb state, API level, build/branch/paths, and raw status output

Configuration (env vars):
- `AADK_CUTTLEFISH_ENABLE=0` to disable detection
- `AADK_CVD_BIN=/path/to/cvd` to override the `cvd` command
- `AADK_LAUNCH_CVD_BIN=/path/to/launch_cvd` to override the launch command
- `AADK_STOP_CVD_BIN=/path/to/stop_cvd` to override the stop command
- `AADK_CUTTLEFISH_ADB_SERIAL=127.0.0.1:6520` to override the adb serial
- `AADK_CUTTLEFISH_CONNECT=0` to skip `adb connect`
- `AADK_CUTTLEFISH_WEBRTC_URL=https://localhost:8443` to override the WebRTC viewer URL
- `AADK_CUTTLEFISH_PAGE_SIZE_CHECK=0` to skip the kernel page-size preflight check
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
- `AADK_CUTTLEFISH_BRANCH=<branch>` (or `_16K`/`_4K`) to override the AOSP branch used for image fetch (default: `main-16k-with-phones` on 16K hosts, `aosp-main-throttled` on ARM)
- `AADK_CUTTLEFISH_TARGET=<target>` (or `_16K`/`_4K`) to override the AOSP target used for image fetch (default: `aosp_cf_arm64`/`aosp_cf_x86_64` on 16K hosts)
- `AADK_CUTTLEFISH_BUILD_ID=<id>` to pin a specific AOSP build id
- `AADK_ADB_PATH` or `ANDROID_SDK_ROOT` to locate `adb`

Notes:
- The UI and CLI pass `include_offline=true`, so a stopped Cuttlefish instance still appears.
- Start/stop/status actions are exposed in the UI and CLI (Targets → Start/Stop/Status Cuttlefish).
- Install Cuttlefish installs host prerequisites from the Android Cuttlefish Artifact Registry (apt) and resolves images (plus the CI host package) from `ci.android.com` public builds.
- On >4K page-size kernels, the default branch switches to `main-16k-with-phones` and the default target switches to `aosp_cf_arm64`/`aosp_cf_x86_64`.
- WebRTC viewer defaults to `https://localhost:8443`.
