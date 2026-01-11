# AADK Full Scaffold (GUI-first, multi-service skeleton)

This is a **full scaffold** for an ARM64-friendly Android DevKit architecture:
- **GTK4 GUI** (thin client)
- **Multiple gRPC services** (Job, Toolchain, Project, Build, Targets, Observe)
- **Protobuf contracts** under `proto/aadk/v1/`

This scaffold is intentionally **minimal but complete**:
- The services return **real structured responses** for listing operations.
- Long-running operations are **stubbed** (they return a `job_id` and print a note),
  except **JobService**, which is fully implemented with:
  - `tokio::sync::broadcast::Sender<JobEvent>` per job
  - bounded replay `history: VecDeque<JobEvent>`
  - `include_history` replay + live streaming
  - cancellation via `watch`

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
- **Toolchains**: list toolchain providers
- **Targets**: list targets from ADB + Cuttlefish and stream logcat
- **Console**: show how build/run actions would be wired
- **Evidence**: shows how an evidence job would be triggered

### 6) Optional: CLI sanity checks
```bash
cargo run -p aadk-cli -- job start-demo
cargo run -p aadk-cli -- toolchain list-providers
cargo run -p aadk-cli -- targets list
```

## Extending from here (recommended order)
1. Implement toolchain install/verify jobs in `aadk-toolchain`
2. Implement real Gradle build execution in `aadk-build`
3. Expand target management (ADB + Cuttlefish discovery/install/launch/logcat are implemented)
4. Implement run history + evidence bundle export in `aadk-observe`
5. Move orchestration logic into `aadk-core` (doctor.run, evidence.run)

## Development notes
- gRPC currently uses TCP loopback for simplicity.
  Switching to **Unix domain sockets** is a straightforward follow-on.
- The UI uses a background tokio runtime thread to avoid blocking the GTK main thread.
- Log display uses a **bounded buffer** and can be replaced with a fully virtualized widget later.
- ToolchainService downloads real SDK/NDK archives, verifies sha256, caches under `~/.local/share/aadk/downloads`,
  and installs under `~/.local/share/aadk/toolchains`.
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

## Third artifact: Reference Implementation Plan (Milestones, Module Skeletons, Performance Harness, CI Matrix)

This artifact is a concrete, implementable plan for shipping AADK v1 (GUI-first) on ARM64 Linux with a service-oriented architecture, Rust implementation, and strong performance discipline. It is written to enable parallel development by multiple contributors.

---

# 1) Project Objectives and Success Definition

## 1.1 Primary objective (v1)

Deliver a native GUI application that enables, on an ARM64 Linux host:

1. Toolchain installation + verification (SDK + NDK)
2. Compose+Kotlin project scaffolding
3. Build + deploy + run on a target
4. Streaming logs and diagnostics
5. Evidence mode + bundle export

## 1.2 "Definition of Done" (DoD)

AADK v1 is "done" when:

* A fresh Debian aarch64 user can install AADK (deb or tarball), run setup wizard, and reach "Doctor: green".
* A new Compose template project can be created and run with one click on a target.
* Console displays build output and logcat without freezing.
* Evidence mode produces a durable export bundle.

---

# 2) Implementation Strategy (Top-level)

## 2.1 Development model

* Monorepo with multiple Rust crates/binaries.
* Service contracts defined by `.proto` files and generated code.
* Strict API versioning.
* Local-first: all services run on the same host with Unix domain socket gRPC.

## 2.2 Two-phase build approach

* **Phase A (MVP)**: implement services with minimal correctness and stability; keep UI thin and functioning.
* **Phase B (Hardening)**: performance tuning, log virtualization, reliability improvements, packaging, and CI.

---

# 3) Repository Structure (Monorepo Blueprint)

```
aadk/
  Cargo.toml                 # workspace root
  README.md
  LICENSE
  CONTRIBUTING.md
  CODE_OF_CONDUCT.md         # optional
  SECURITY.md                # optional
  proto/
    aadk/v1/*.proto
  crates/
    aadk-common/             # shared types, config, utilities
    aadk-proto/              # generated code (or build.rs generated)
    aadk-core/               # control plane (service registry, job scheduler)
    aadk-toolchain/          # toolchain service
    aadk-build/              # build service
    aadk-targets/            # target service
    aadk-observe/            # observe/history/evidence bundles
    aadk-cli/                # CLI thin client
    aadk-ui/                 # GTK app (thin client)
    aadk-testkit/            # integration test harness
  assets/
    templates/
      compose-counter/
      compose-form/
  packaging/
    debian/
      control
      rules
      postinst
      prerm
    systemd/
      aadk-core.service
      aadk-toolchain.service
      ...
  scripts/
    dev/
      gen-proto.sh
      run-services.sh
      smoke-test.sh
  docs/
    architecture.md
    api.md
    gui-wireframes.md
    performance.md
    release.md
```

---

# 4) Technology Choices (Concrete)

## 4.1 Rust stack

* gRPC: `tonic`
* Protobuf compilation: `prost` + `tonic-build`
* Async runtime: `tokio`
* Logging: `tracing`, `tracing-subscriber`
* Structured serialization: `serde`, `serde_json`, `toml`
* CLI: `clap` (thin client)
* Packaging: Debian tooling + systemd units

## 4.2 GUI stack

* GTK4 via `gtk4` crate / gtk-rs
* libadwaita optional for modern shell layout
* UI threading: GTK main thread only
* gRPC client: tonic in a background tokio runtime; forward messages via glib channels

---

# 5) Architecture Implementation Plan (Service Details)

## 5.1 Core patterns

### 5.1.1 Job model

All work is a Job:

* has state transitions: queued -> running -> success/failed/cancelled
* emits events:

  * progress updates
  * log output
  * result outputs (artifact paths)
* stored in an in-memory cache and persisted (optional in v1, recommended in v1.1)

### 5.1.2 Event bus

* Internal event bus in each service:

  * jobs stream events to gRPC stream for GUI
* `aadk-observe` stores selected job events for run history and evidence bundles

### 5.1.3 Config

Config precedence:

1. Project override
2. User global config
3. Defaults

Store in `~/.config/aadk/config.toml` and `~/.local/share/aadk/` for state.

---

## 5.2 Service skeletons and responsibilities

### 5.2.1 `aadk-core` (Control Plane)

**Responsibilities**

* Service discovery and health monitoring
* Job scheduling and orchestration for composite jobs:

  * `doctor.run`
  * `evidence.run` (pipeline job)
* Persistent registry of:

  * installed toolchains
  * known projects
  * run history references (delegated to observe service)

**Key modules**

* `registry.rs` (services, providers)
* `orchestrator.rs` (multi-step jobs)
* `health.rs` (service health polling)
* `config.rs` (global config)
* `ipc.rs` (UDS gRPC server)
* `job_store.rs` (job metadata; maybe in-memory + file persistence)

**MVP note**

* It is acceptable for services to run independently without core orchestration at first; core can be introduced early or after initial service MVPs.

---

### 5.2.2 `aadk-toolchain`

**Responsibilities**

* Provide toolchain provider framework:

  * list providers
  * list available versions
  * install toolchain
  * verify toolchain
  * activate toolchain set (via core or direct RPC)
* Manage install directory layout and provenance.

**Provider MVP implementations**

* Provider: `android-ndk-custom`

  * fetch releases metadata (or pinned version list for v1)
  * download archive
  * verify sha256
  * extract into toolchain dir
* Provider: `android-sdk-custom`

  * same pattern

**Key modules**

* `providers/mod.rs`
* `providers/ndk_custom.rs`
* `providers/sdk_custom.rs`
* `download.rs` (resume support; checksums)
* `verify.rs`
* `install.rs` (extract; atomic move)
* `provenance.rs` (write provenance file)
* `envmap.rs` (export env vars for build service)

**Transactional install requirement**

* Extract to temp dir
* Verify
* Move into final dir atomically
* Mark installed only after success

---

### 5.2.3 `aadk-build`

**Responsibilities**

* Run Gradle wrapper builds
* Stream build output logs to job stream
* Parse diagnostics from output
* Provide artifacts list

**Key modules**

* `gradle.rs` (spawn process, capture stdout/stderr)
* `diagnostics.rs` (regex/state machine parser for errors)
* `artifacts.rs` (locate APK paths in Gradle output or known directories)
* `cache.rs` (optional; can be minimal)
* `env.rs` (apply toolchain environment mapping)

**Gradle execution model**

* Use `tokio::process::Command`
* Set env:

  * `ANDROID_SDK_ROOT`, `ANDROID_HOME` (if needed)
  * `ANDROID_NDK_ROOT`
  * `JAVA_HOME`
  * `PATH` with toolchain platform tools
* Pass standard flags:

  * `--stacktrace` in debug mode
  * `--no-daemon` configurable (daemon can help performance but increases memory; choose default explicitly)

---

### 5.2.4 `aadk-targets`

**Responsibilities**

* List ADB targets (devices) and emulator-like targets
* Install APK via adb
* Launch activity
* Stream logcat

**Key modules**

* `adb.rs` (wrap `adb` command; parse outputs)
* `device_discovery.rs`
* `install.rs`
* `launch.rs`
* `logcat.rs` (stream; filters)
* `providers/mod.rs` (emulator-like providers, e.g., cuttlefish provider)

**Cuttlefish provider (v1)**

* Minimal requirement:

  * treat Cuttlefish as "adb target reachable"
  * optionally assist with connecting (adb connect 127.0.0.1:6520)
* If Cuttlefish is not running, the provider can show "Not running" state (start/stop via UI/CLI).

---

### 5.2.5 `aadk-observe`

**Responsibilities**

* Run history
* Evidence bundles
* Support bundles
* Export compressed artifacts

**Key modules**

* `history.rs` (run records)
* `bundle.rs` (zip/tar builder)
* `redact.rs` (strip sensitive paths if needed)
* `storage.rs` (persist run summaries)

**Evidence bundle contents (v1)**

* `summary.json`
* `toolchain.json`
* `target.json`
* `jobs/...` (logs)
* `diagnostics.json`
* optionally: `project/gradle_versions.json`

---

### 5.2.6 `aadk-ui`

**Responsibilities**

* Render screens described in the wireframe spec
* Provide a stable UX with high responsiveness
* Subscribe to job streams and log streams

**Key modules**

* `app.rs` (main)
* `state.rs` (AppState model)
* `rpc.rs` (gRPC client wrapper)
* `screens/` (home, setup, projects, toolchains, targets, console, evidence)
* `widgets/log_view.rs` (virtualized log viewer)
* `jobs.rs` (job start, cancellation, stream binding)
* `errors.rs` (render error model and remediation actions)

**Log virtualization implementation requirement**

* Use a model of:

  * `VecDeque<LineRef>` + backing store
  * optional on-disk append file for very large logs
  * render only visible subset based on scroll position
* Apply throttling:

  * batch log append events to UI at fixed cadence (timer-driven)

---

### 5.2.7 `aadk-cli`

**Responsibilities**

* Provide parity with GUI actions:

  * toolchain install
  * init project
  * build
  * run
  * logs
  * evidence export
* Useful for CI, automation, and debugging.

---

# 6) Milestone Plan (Detailed)

## Milestone 0 - Contracts and scaffolding (1-2 weeks equivalent work)

**Deliverables**

* Finalize `proto/` schema (v1)
* Generate Rust bindings (`aadk-proto`)
* Build workspace compiles
* Dummy services implement health endpoints and minimal RPC stubs
* GUI shell renders navigation with placeholder screens

**Acceptance**

* `aadk-core`, `aadk-toolchain`, `aadk-build`, `aadk-targets`, `aadk-observe` start and respond to "list providers / list targets" with stub results.
* `aadk-ui` can connect to services via Unix socket (or TCP loopback in dev).

## Milestone 1 - Toolchain MVP (2-4 weeks)

**Deliverables**

* Toolchain providers implemented:

  * android-ndk-custom (pinned versions acceptable for v1)
  * android-sdk-custom
* Transactional install + sha256 verification
* Setup wizard integrates install + doctor checks
* Provenance recorded

**Acceptance**

* A user can install SDK+NDK via GUI; Doctor reports green for toolchain checks.

## Milestone 2 - Project templates + create/open (2-3 weeks)

**Deliverables**

* Template system with at least:

  * Compose Counter (state update)
* New Project wizard
* Project registry (recent projects)

**Acceptance**

* A user can create a project; file tree exists; build service recognizes it.

## Milestone 3 - Build service MVP (2-4 weeks)

**Deliverables**

* Gradle wrapper execution, logs streamed to GUI
* Artifact discovery for debug APK
* Diagnostics parsing basic rules
* Console UI with build output virtualized viewer (initial)

**Acceptance**

* Build debug completes on Pi host for a template project.
* GUI displays build output without freezing.

## Milestone 4 - Targets MVP (2-4 weeks)

**Deliverables**

* ADB target discovery
* Install APK
* Launch app
* Logcat streaming
* Targets screen functional

**Acceptance**

* A user can plug in a physical device, run app, and view logcat.

## Milestone 5 - Evidence mode + bundles (2-3 weeks)

**Deliverables**

* Evidence job orchestration (core sequences build/install/launch/open logcat)
* Evidence mode UI checklist
* Evidence bundle export
* Support bundle export

**Acceptance**

* A user can run one-click pipeline and export summary/logs as a bundle.

## Milestone 6 - Hardening + packaging (2-6 weeks)

**Deliverables**

* Debian packaging
* systemd services
* crash recovery
* performance tuning (log viewer, caching, streaming)
* documentation and contributor guide
* integration tests

**Acceptance**

* `apt install aadk` (or dpkg) provides functional application; services auto-start; GUI stable.

---

# 7) Module Skeletons (Rust crate manifests)

## 7.1 Workspace `Cargo.toml` (conceptual)

* `members = ["crates/*"]`
* shared dependency versions pinned at workspace level
* feature flags for optional providers (cuttlefish, remote targets)

## 7.2 Each service binary structure

Each service crate contains:

* `src/main.rs` (gRPC server start, UDS bind)
* `src/lib.rs` (core logic)
* `src/api.rs` (tonic service implementations)
* `src/config.rs`
* `src/health.rs`

---

# 8) Performance Test Harness (Mandatory)

Performance must be engineered, not hoped for. Add a `aadk-testkit` crate that can run benchmarks and stress tests.

## 8.1 Key performance tests

### 8.1.1 GUI cold start test

* Measure time from process start to first frame + responsive navigation.
* Record: P50/P95 on Pi hardware.

### 8.1.2 Log viewer throughput test

* Simulate 5,000 lines/min and 50,000 lines burst.
* Verify:

  * memory stays bounded
  * UI remains responsive
  * scroll remains smooth

### 8.1.3 Build streaming test

* Run a known Gradle build and stream output.
* Verify no dropped events, or controlled truncation with on-disk spill.

### 8.1.4 End-to-end pipeline test

* Create project -> build -> install -> launch (mock target for CI; real target optional for local)

## 8.2 Harness mechanics

* Provide a `aadk perf` command (CLI) that runs these tests and outputs a JSON report.

---

# 9) CI Matrix (Complete)

## 9.1 CI platforms

* Primary: Linux ARM64 (aarch64) CI runners (self-hosted or cloud ARM)

## 9.2 CI jobs

1. **lint**

   * `cargo fmt --check`
   * `cargo clippy -- -D warnings`
2. **unit tests**

   * `cargo test --workspace`
3. **proto check**

   * ensure generated code matches protos (or regenerate in CI and diff)
4. **integration tests**

   * mock target provider tests
   * toolchain provider tests with local fixture archives (no network)
5. **build artifacts**

   * build release binaries
   * build `.deb` packages
6. **security**

   * `cargo audit` (dependency advisory scanning)
7. **docs**

   * ensure docs compile/validate if using mdbook or similar

## 9.3 Release pipeline

* Tag-based releases:

  * build binaries
  * attach deb/tarball
  * generate checksums
  * publish release notes with upgrade notes

---

# 10) Developer Onboarding Plan

## 10.1 One-command dev setup

Provide `scripts/dev/bootstrap.sh` that:

* installs Rust toolchain
* installs protobuf compiler or uses `prost-build` with vendored protoc (choose one)
* sets up a dev config
* runs services in dev mode on TCP loopback (optional)
* launches GUI

## 10.2 Local run scripts

* `scripts/dev/run-services.sh`: start all services with logs
* `scripts/dev/run-ui.sh`: start GUI
* `scripts/dev/smoke-test.sh`: create project, build, list targets

---

# 11) Risk Register and Mitigations (Engineering reality)

## 11.1 Toolchain volatility

**Risk:** upstream toolchain formats change.
**Mitigation:** provider abstraction, pinned versions, provenance tracking, hash verification, offline fixtures.

## 11.2 Target/emulator ambiguity

**Risk:** "emulator-like" provider behavior varies.
**Mitigation:** uniform target interface; declare provider capabilities; degrade gracefully.

## 11.3 Performance regression

**Risk:** logs or builds cause UI freezes.
**Mitigation:** strict log virtualization; throttling; perf harness in CI; budgets.

## 11.4 Contributor fragmentation (Rust vs C)

**Risk:** integrating C toolchains becomes maintenance burden.
**Mitigation:** treat C as upstream artifacts; Rust owns orchestration; isolate integration points.

---

# 12) Concrete v1 Deliverable List (Checkable)

1. GTK GUI with all screens:

   * Setup/Doctor, Home, Projects, Toolchains, Targets, Console, Evidence, History, Settings
2. Services implemented:

   * JobService, ToolchainService, ProjectService, BuildService, TargetService, ObserveService
3. Two toolchain providers:

   * android-ndk-custom
   * android-sdk-custom
4. One template:

   * Compose Counter (state update)
5. Build debug + artifact detection
6. ADB device install/launch/logcat
7. Evidence bundle export
8. Debian packaging + systemd units
9. CI pipeline with lint/test/build/package
10. Performance harness with at least log throughput and cold start tests

---

# Part A - GUI Wireframe Specification (AADK GUI v1)

## A.1 Design goals and constraints (performance-driven)

### A.1.1 Goals

* **Fast, native** GUI suitable for ARM64 SBC hardware.
* Minimal "Android Studio outcome" loop: **install toolchain -> create project -> build -> deploy -> run -> logcat -> evidence export**.
* Everything essential is accessible from GUI without terminal.
* All background work is handled by services; GUI is a thin client.

### A.1.2 Constraints

* UI must remain responsive during Gradle builds and log streaming.
* Log viewer must be **virtualized**; no unbounded text widget growth.
* UI must handle intermittent service restarts.
* UI must make "evidence mode" easy for screen recording and grading.

### A.1.3 UI toolkit assumption

* GTK4 (gtk-rs) and libadwaita-style layout patterns (header bar + sidebar + content).

---

## A.2 Application shell: global layout and navigation

### A.2.1 Global layout

* **Header Bar (top)**

  * App name: "AADK"
  * Active project selector (dropdown)
  * Active target indicator (chip)
  * Active toolchain indicator (chip)
  * Global search (optional)
  * Overflow menu: Settings, Export Support Bundle, About

* **Sidebar (left)**

  * Home (Dashboard)
  * Setup / Doctor
  * Projects
  * Toolchains
  * Targets
  * Console (Build/Run/Logs)
  * Evidence Mode
  * History (Runs)
  * Settings

* **Content area (main)**

  * Screen content with cards, tables, and panels.
  * Right-side optional "Details drawer" on large screens for selected items.

* **Status bar (bottom, optional)**

  * Service health icons (core/toolchain/build/targets/observe)
  * Current job status (if any)
  * Disk/RAM quick stats (optional)

### A.2.2 Global "always visible" state

* Active project path (or "No project selected")
* Default target (device/emulator-like)
* Active toolchain set (SDK + NDK versions)
* Service health state: green/yellow/red with tooltips and "Restart services" action

---

## A.3 Screen-by-screen wireframes

### A.3.1 Setup / Doctor

#### A.3.1.1 Purpose

First-run setup and ongoing health validation of host, toolchain, and targets.

#### A.3.1.2 Wireframe (ASCII)

```
+----------------------------------------------------------------------+
| Setup / Doctor                                                       |
+----------------------------------------------------------------------+
| Host Summary                                                         |
|  OS: Debian GNU/Linux 13 (trixie)  Arch: aarch64  CPU: BCM2712       |
|  RAM: 8GB   Disk Free: 218GB   Kernel: 6.12.x                        |
|                                                                      |
| Toolchain Status                                                     |
|  SDK:   [Not installed]   NDK: [Not installed]                       |
|  JDK:   [Detected: Temurin/OpenJDK 17]                                |
|                                                                      |
| Actions:   [Install Toolchains...]  [Run Doctor]  [View Details]       |
|                                                                      |
| Doctor Results (last run: never)                                     |
|  [ ] JDK available                                                     |
|  [ ] ANDROID_SDK_ROOT configured                                       |
|  [ ] Build tools executable (aapt2/zipalign)                           |
|  [ ] Platform tools executable (adb)                                   |
|  [ ] Targets reachable (device or emulator-like)                       |
|                                                                      |
| Issues / Remediation                                                 |
|  - No SDK installed. [Install]                                       |
|  - No NDK installed. [Install]                                       |
+----------------------------------------------------------------------+
```

#### A.3.1.3 "Install Toolchains..." dialog

* Provider selection:

  * SDK provider: `android-sdk-custom` (default)
  * NDK provider: `android-ndk-custom` (default)
* Version selection dropdowns (provider-driven)
* Install location (default `~/.aadk/toolchains`)
* "Verify hashes" checkbox (forced ON when hashes exist)
* Buttons: Install / Cancel

#### A.3.1.4 Doctor details (expanded)

Each check has:

* status (OK/Warn/Fail)
* measured data (e.g., `adb version`, `aapt2 --version`)
* remediation suggestions
* "Copy diagnostic snippet"

---

### A.3.2 Home (Dashboard)

```
+----------------------------------------------------------------------+
| Home                                                                 |
+----------------------------------------------------------------------+
| Quick Actions: [New Project] [Open Project] [Run] [Evidence Mode]    |
|                                                                      |
| Status Cards                                                         |
|  +---------------+  +---------------+  +---------------+            |
|  | Toolchain     |  | Targets       |  | Last Build    |            |
|  | SDK: rX       |  | Pixel 9 (USB) |  | SUCCESS       |            |
|  | NDK: r29      |  | Cuttlefish    |  | 28s           |            |
|  +---------------+  +---------------+  +---------------+            |
|                                                                      |
| Recent Projects                                                      |
|  - PiComposeDemo   (last run: 2m ago) [Open] [Run]                   |
|  - ...                                                                 |
+----------------------------------------------------------------------+
```

---

### A.3.3 Projects

#### A.3.3.1 Purpose

Create/open/manage projects and per-project config (toolchain/target pinning).

```
+----------------------------------------------------------------------+
| Projects                                  [New] [Open...]              |
+----------------------------------------------------------------------+
| Recent                                                               |
|  > PiComposeDemo      /home/user/dev/PiComposeDemo   Build: OK       |
|    ComposeCounter     /home/user/dev/ComposeCounter  Build: Fail     |
|                                                                      |
| Selected Project Details                                             |
|  Path: /home/user/dev/PiComposeDemo                                  |
|  Toolchain: [Use global]  (SDK v..., NDK v...)                            |
|  Target:    [Use global]  (Pixel 9)                                  |
|  Actions: [Build] [Run] [Open Folder] [Remove from list]            |
+----------------------------------------------------------------------+
```

#### A.3.3.2 New Project Wizard (Compose)

Step-based wizard:

1. Name + Location
2. App ID / Namespace
3. Min/Target SDK
4. Template selection (Compose Counter, Compose Form, Compose List)
5. Summary + "Create" + "Create and Run"

---

### A.3.4 Toolchains

#### A.3.4.1 Purpose

Install, verify, activate SDK/NDK; show provenance; manage versions.

```
+----------------------------------------------------------------------+
| Toolchains                             [Install...] [Verify] [Remove] |
+----------------------------------------------------------------------+
| Installed Toolchains                                                 |
|  SDK Provider        Version     Path                 Status         |
|  android-sdk-custom   35.0.x      ~/.aadk/...          Verified [x]     |
|                                                                      |
|  NDK Provider        Version      Path                Status         |
|  android-ndk-custom   r29         ~/.aadk/...          Verified [x]     |
|                                                                      |
| Active Toolchain Set:  SDK 35.0.x + NDK r29   [Set Active...]          |
|                                                                      |
| Provenance (selected)                                                |
|  Source URL: ...                                                      |
|  SHA-256: ... (match [x])                                               |
|  Installed: 2026-01-07 00:12                                        |
+----------------------------------------------------------------------+
```

#### A.3.4.2 Advanced: "Toolchain mapping" view

Shows what the build service will export:

* `ANDROID_SDK_ROOT`
* `ANDROID_NDK_ROOT`
* `JAVA_HOME`
* `PATH` additions
* optional `aapt2` override path

---

### A.3.5 Targets

#### A.3.5.1 Purpose

Unified device/emulator-like target management.

```
+----------------------------------------------------------------------+
| Targets                                           [Refresh] [Connect]|
+----------------------------------------------------------------------+
| Physical Devices (ADB)                                                |
|  * Pixel 9   serial: ABC123   API: 35   USB   [Set Default] [Details]|
|                                                                      |
| Emulator-like Targets                                                 |
|  o Cuttlefish (Local)   Status: Running   ADB: Connected            |
|     [Set Default] [Start/Stop] [Details]                             |
|                                                                      |
| Default Target: Pixel 9                                              |
+----------------------------------------------------------------------+
```

#### A.3.5.2 Wi-Fi ADB connect dialog

* Input: IP:port
* "Pairing code" option (if supported)
* Connect / Cancel
* Logs of connection attempts

---

### A.3.6 Console (Build / Run / Logs)

#### A.3.6.1 Purpose

Central workflow screen for building, running, and diagnostics.

```
+----------------------------------------------------------------------+
| Console - PiComposeDemo                         Target: Pixel 9      |
+----------------------------------------------------------------------+
| Actions: [Build] [Clean] [Run] [Install] [Launch] [Stop]             |
|                                                                      |
| Job Timeline:                                                        |
|  * BuildDebug     00:00 -> 00:28    SUCCESS                            |
|  * InstallDebug   00:28 -> 00:31    SUCCESS                            |
|  * Launch         00:31 -> 00:32    SUCCESS                            |
|                                                                      |
| Tabs: [Build Output] [Logcat] [Diagnostics] [Artifacts]              |
|                                                                      |
| Build Output (virtualized viewer)                                    |
|  ... log lines ...                                                       |
|                                                                      |
| Footer:  [Filter...] [Search...] [Copy Error Summary] [Export Logs...]     |
+----------------------------------------------------------------------+
```

#### A.3.6.2 Log viewer requirements (hard)

* Virtualized scrolling with line indexing.
* Ring buffer defaults:

  * Build output: 200k lines (configurable)
  * Logcat: 50k lines (configurable)
* Throttled UI updates:

  * append batches at up to 20-60Hz
* Export full stream to file without requiring it all in memory.

#### A.3.6.3 Diagnostics tab

Parsed issues grouped:

* Kotlin compile errors
* Missing SDK/NDK
* Resource packaging errors (aapt2)
* Dex/Desugaring
  Each issue includes:
* file/line if available
* raw excerpt
* suggested remediation action (e.g., "Install SDK platform 35")

---

### A.3.7 Evidence Mode (Screencast-friendly)

#### A.3.7.1 Purpose

Deterministic pipeline for recording.

```
+----------------------------------------------------------------------+
| Evidence Mode - PiComposeDemo                                        |
+----------------------------------------------------------------------+
| Target: [Default: Pixel 9 v]   Template Evidence: Compose Counter     |
|                                                                      |
| [One-click Evidence Run]                                             |
|  Performs: Clean -> Build -> Install -> Launch -> Open Logcat            |
|                                                                      |
| Checklist (tick as you record)                                       |
|  [x] Project created (Compose + Kotlin)                                |
|  [x] Build succeeded                                                   |
|  [x] App launched                                                     |
|  [ ] User interaction changes state (press Increment)                  |
|  [ ] Show logcat filter "PiComposeDemo"                                |
|                                                                      |
| Run Summary                                                          |
|  Start: 00:00  End: 00:32  Result: SUCCESS                           |
|  Export: [Save Evidence Bundle...]                                     |
+----------------------------------------------------------------------+
```

#### A.3.7.2 Evidence bundle contents

* JSON summary (jobs + durations + toolchain/target IDs)
* Build logs (compressed)
* Logcat snippet (filtered)
* Project metadata (Gradle/AGP/Kotlin versions)
* Optional screenshot triggers (future)

---

## A.4 Common dialogs and states

### A.4.1 Service health / recovery dialog

If a service disconnects:

* Show banner: "Build service disconnected. [Restart] [Details]"
* Allow continue browsing (read-only) and queue jobs until recovery (optional).

### A.4.2 Permission prompts

* USB device authorization is handled on the phone; GUI should detect "unauthorized" and show instructions, not attempt to escalate privileges.

### A.4.3 Error presentation policy

* Always show:

  * What failed (job name)
  * Why (top 3 likely causes with confidence)
  * What to do next (action buttons)

---

## A.5 GUI state model (canonical)

The GUI is a projection of service state; it stores minimal local state.

* `AppState`

  * `ServiceHealth[]`
  * `HostStatus`
  * `ActiveProjectId?`
  * `ActiveToolchainSetId?`
  * `DefaultTargetId?`
  * `Jobs` (active + recent)
  * `Projects` (recent list)
  * `Toolchains` (installed + available)
  * `Targets` (current enumeration)
  * `RunHistory` (last N runs + evidence bundles)

---

# Part B - Protobuf API Specification (AADK gRPC v1)

## B.1 Repository layout for protos (suggested)

```
proto/
  aadk/v1/
    common.proto
    errors.proto
    job.proto
    toolchain.proto
    project.proto
    build.proto
    target.proto
    observe.proto
```

All services share:

* common identifiers
* a unified job model and event stream
* a unified error model

Below is a complete baseline set.

---

## B.2 `common.proto`

```proto
syntax = "proto3";

package aadk.v1;

option go_package = "aadk/v1;aadkv1";

message Id {
  string value = 1; // opaque stable identifier
}

message Timestamp {
  int64 unix_millis = 1;
}

message KeyValue {
  string key = 1;
  string value = 2;
}

message HostStatus {
  string os_name = 1;         // e.g. Debian GNU/Linux
  string os_version = 2;      // e.g. 13 (trixie)
  string arch = 3;            // e.g. aarch64
  string kernel_version = 4;  // e.g. 6.12.x
  uint64 ram_total_bytes = 5;
  uint64 ram_free_bytes = 6;
  uint64 disk_total_bytes = 7;
  uint64 disk_free_bytes = 8;
  string cpu_model = 9;
  uint32 cpu_cores = 10;
}

message Pagination {
  uint32 page_size = 1;
  string page_token = 2;
}

message PageInfo {
  string next_page_token = 1;
}
```

---

## B.3 `errors.proto` (uniform error model)

```proto
syntax = "proto3";

package aadk.v1;

enum ErrorCode {
  ERROR_CODE_UNSPECIFIED = 0;

  // Generic
  ERROR_CODE_INTERNAL = 1;
  ERROR_CODE_INVALID_ARGUMENT = 2;
  ERROR_CODE_NOT_FOUND = 3;
  ERROR_CODE_ALREADY_EXISTS = 4;
  ERROR_CODE_PERMISSION_DENIED = 5;
  ERROR_CODE_UNAVAILABLE = 6;
  ERROR_CODE_TIMEOUT = 7;
  ERROR_CODE_CANCELLED = 8;

  // Toolchain
  ERROR_CODE_TOOLCHAIN_VERIFY_FAILED = 100;
  ERROR_CODE_TOOLCHAIN_INSTALL_FAILED = 101;
  ERROR_CODE_TOOLCHAIN_INCOMPATIBLE_HOST = 102;

  // Build
  ERROR_CODE_BUILD_FAILED = 200;
  ERROR_CODE_GRADLE_NOT_FOUND = 201;
  ERROR_CODE_SDK_MISSING = 202;
  ERROR_CODE_NDK_MISSING = 203;

  // Targets
  ERROR_CODE_ADB_NOT_AVAILABLE = 300;
  ERROR_CODE_TARGET_NOT_REACHABLE = 301;
  ERROR_CODE_INSTALL_FAILED = 302;
  ERROR_CODE_LAUNCH_FAILED = 303;
}

message Remediation {
  string title = 1;       // e.g. "Install SDK Platform 35"
  string description = 2; // e.g. "SDK platform 35 is required for compileSdk=35"
  string action_id = 3;   // opaque id that UI can map to a button action
  repeated KeyValue params = 4;
}

message ErrorDetail {
  ErrorCode code = 1;
  string message = 2;              // human readable
  string technical_details = 3;    // raw snippet, stack traces, etc.
  repeated Remediation remedies = 4;

  // Correlation
  string correlation_id = 5;
}
```

---

## B.4 `job.proto` (job model + event streaming)

```proto
syntax = "proto3";

package aadk.v1;

import "aadk/v1/common.proto";
import "aadk/v1/errors.proto";

enum JobState {
  JOB_STATE_UNSPECIFIED = 0;
  JOB_STATE_QUEUED = 1;
  JOB_STATE_RUNNING = 2;
  JOB_STATE_SUCCESS = 3;
  JOB_STATE_FAILED = 4;
  JOB_STATE_CANCELLED = 5;
}

message JobRef {
  Id job_id = 1;
}

message Job {
  Id job_id = 1;
  string job_type = 2;        // e.g. "toolchain.install", "build.debug", "target.install"
  JobState state = 3;
  Timestamp created_at = 4;
  Timestamp started_at = 5;
  Timestamp finished_at = 6;

  string display_name = 7;    // UI-friendly name
  string correlation_id = 8;

  // Optional linkage
  Id project_id = 9;
  Id target_id = 10;
  Id toolchain_set_id = 11;
}

message JobProgress {
  uint32 percent = 1; // 0..100
  string phase = 2;   // e.g. "Downloading", "Extracting", "Verifying"
  repeated KeyValue metrics = 3; // bytes_downloaded, total_bytes, etc.
}

message LogChunk {
  string stream = 1;          // "stdout", "stderr", "logcat", etc.
  bytes data = 2;             // raw bytes (utf-8 expected but not required)
  bool truncated = 3;
}

message JobEvent {
  Timestamp at = 1;
  Id job_id = 2;

  oneof payload {
    JobStateChanged state_changed = 10;
    JobProgressUpdated progress = 11;
    JobLogAppended log = 12;
    JobCompleted completed = 13;
    JobFailed failed = 14;
  }
}

message JobStateChanged {
  JobState new_state = 1;
}

message JobProgressUpdated {
  JobProgress progress = 1;
}

message JobLogAppended {
  LogChunk chunk = 1;
}

message JobCompleted {
  string summary = 1;
  repeated KeyValue outputs = 2; // e.g. artifact paths
}

message JobFailed {
  ErrorDetail error = 1;
}

message StartJobRequest {
  string job_type = 1;
  repeated KeyValue params = 2;
  Id project_id = 3;
  Id target_id = 4;
  Id toolchain_set_id = 5;
}

message StartJobResponse {
  JobRef job = 1;
}

message GetJobRequest {
  Id job_id = 1;
}

message GetJobResponse {
  Job job = 1;
}

message CancelJobRequest {
  Id job_id = 1;
}

message CancelJobResponse {
  bool accepted = 1;
}

message StreamJobEventsRequest {
  Id job_id = 1;
  bool include_history = 2; // if true, replay buffered events then stream live
}

service JobService {
  rpc StartJob(StartJobRequest) returns (StartJobResponse);
  rpc GetJob(GetJobRequest) returns (GetJobResponse);
  rpc CancelJob(CancelJobRequest) returns (CancelJobResponse);
  rpc StreamJobEvents(StreamJobEventsRequest) returns (stream JobEvent);
}
```

---

## B.5 `toolchain.proto` (providers, installs, activation)

```proto
syntax = "proto3";

package aadk.v1;

import "aadk/v1/common.proto";
import "aadk/v1/errors.proto";

enum ToolchainKind {
  TOOLCHAIN_KIND_UNSPECIFIED = 0;
  TOOLCHAIN_KIND_SDK = 1;
  TOOLCHAIN_KIND_NDK = 2;
}

message ToolchainProvider {
  Id provider_id = 1;
  string name = 2;          // e.g. "android-ndk-custom"
  ToolchainKind kind = 3;   // SDK or NDK
  string description = 4;
}

message ToolchainVersion {
  string version = 1;       // e.g. "r29" or "35.0.2"
  string channel = 2;       // "stable", "beta", etc. provider-defined
  string notes = 3;
}

message ToolchainArtifact {
  string url = 1;
  string sha256 = 2;        // optional but required if provider can supply
  uint64 size_bytes = 3;
}

message AvailableToolchain {
  ToolchainProvider provider = 1;
  ToolchainVersion version = 2;
  ToolchainArtifact artifact = 3;
}

message InstalledToolchain {
  Id toolchain_id = 1;
  ToolchainProvider provider = 2;
  ToolchainVersion version = 3;
  string install_path = 4;
  bool verified = 5;
  Timestamp installed_at = 6;

  // Provenance
  string source_url = 7;
  string sha256 = 8;
}

message ToolchainSet {
  Id toolchain_set_id = 1;
  Id sdk_toolchain_id = 2;
  Id ndk_toolchain_id = 3;
  string display_name = 4; // "SDK 35.0.2 + NDK r29"
}

message ListProvidersRequest {}
message ListProvidersResponse {
  repeated ToolchainProvider providers = 1;
}

message ListAvailableRequest {
  Id provider_id = 1;
  Pagination page = 2;
}
message ListAvailableResponse {
  repeated AvailableToolchain items = 1;
  PageInfo page_info = 2;
}

message ListInstalledRequest {
  ToolchainKind kind = 1; // optional filter
}
message ListInstalledResponse {
  repeated InstalledToolchain items = 1;
}

message InstallToolchainRequest {
  Id provider_id = 1;
  string version = 2;
  string install_root = 3; // e.g. "~/.aadk/toolchains"
  bool verify_hash = 4;
}
message InstallToolchainResponse {
  Id job_id = 1; // job: toolchain.install
}

message VerifyToolchainRequest {
  Id toolchain_id = 1;
}
message VerifyToolchainResponse {
  bool verified = 1;
  ErrorDetail error = 2; // optional
}

message CreateToolchainSetRequest {
  Id sdk_toolchain_id = 1;
  Id ndk_toolchain_id = 2;
  string display_name = 3;
}
message CreateToolchainSetResponse {
  ToolchainSet set = 1;
}

message SetActiveToolchainSetRequest {
  Id toolchain_set_id = 1;
}
message SetActiveToolchainSetResponse {
  bool ok = 1;
}

message GetActiveToolchainSetRequest {}
message GetActiveToolchainSetResponse {
  ToolchainSet set = 1;
}

service ToolchainService {
  rpc ListProviders(ListProvidersRequest) returns (ListProvidersResponse);
  rpc ListAvailable(ListAvailableRequest) returns (ListAvailableResponse);
  rpc ListInstalled(ListInstalledRequest) returns (ListInstalledResponse);

  rpc InstallToolchain(InstallToolchainRequest) returns (InstallToolchainResponse);
  rpc VerifyToolchain(VerifyToolchainRequest) returns (VerifyToolchainResponse);

  rpc CreateToolchainSet(CreateToolchainSetRequest) returns (CreateToolchainSetResponse);
  rpc SetActiveToolchainSet(SetActiveToolchainSetRequest) returns (SetActiveToolchainSetResponse);
  rpc GetActiveToolchainSet(GetActiveToolchainSetRequest) returns (GetActiveToolchainSetResponse);
}
```

---

## B.6 `project.proto` (projects and templates)

```proto
syntax = "proto3";

package aadk.v1;

import "aadk/v1/common.proto";
import "aadk/v1/errors.proto";

message Project {
  Id project_id = 1;
  string name = 2;
  string path = 3;           // root directory
  Timestamp created_at = 4;
  Timestamp last_opened_at = 5;

  // Optional per-project overrides
  Id toolchain_set_id = 6;
  Id default_target_id = 7;
}

message Template {
  Id template_id = 1;
  string name = 2;           // "Compose Counter"
  string description = 3;
  repeated KeyValue defaults = 4; // minSdk, compileSdk, etc.
}

message ListTemplatesRequest {}
message ListTemplatesResponse {
  repeated Template templates = 1;
}

message CreateProjectRequest {
  string name = 1;
  string path = 2;            // directory to create into
  Id template_id = 3;

  repeated KeyValue params = 4; // applicationId, namespace, minSdk, targetSdk...
  Id toolchain_set_id = 5;      // optional override at creation time
}

message CreateProjectResponse {
  Id job_id = 1; // job: project.create
  Id project_id = 2;
}

message OpenProjectRequest {
  string path = 1;
}
message OpenProjectResponse {
  Project project = 1;
}

message ListRecentProjectsRequest {
  Pagination page = 1;
}
message ListRecentProjectsResponse {
  repeated Project projects = 1;
  PageInfo page_info = 2;
}

message SetProjectConfigRequest {
  Id project_id = 1;
  Id toolchain_set_id = 2;
  Id default_target_id = 3;
}
message SetProjectConfigResponse {
  bool ok = 1;
}

service ProjectService {
  rpc ListTemplates(ListTemplatesRequest) returns (ListTemplatesResponse);
  rpc CreateProject(CreateProjectRequest) returns (CreateProjectResponse);
  rpc OpenProject(OpenProjectRequest) returns (OpenProjectResponse);
  rpc ListRecentProjects(ListRecentProjectsRequest) returns (ListRecentProjectsResponse);
  rpc SetProjectConfig(SetProjectConfigRequest) returns (SetProjectConfigResponse);
}
```

---

## B.7 `build.proto` (build orchestration + artifacts)

```proto
syntax = "proto3";

package aadk.v1;

import "aadk/v1/common.proto";
import "aadk/v1/errors.proto";

enum BuildVariant {
  BUILD_VARIANT_UNSPECIFIED = 0;
  BUILD_VARIANT_DEBUG = 1;
  BUILD_VARIANT_RELEASE = 2;
}

message BuildRequest {
  Id project_id = 1;
  BuildVariant variant = 2;
  bool clean_first = 3;
  repeated KeyValue gradle_args = 4; // e.g. -P, --stacktrace
}

message BuildResponse {
  Id job_id = 1; // job: build.run
}

message Artifact {
  string name = 1;       // app-debug.apk
  string path = 2;
  uint64 size_bytes = 3;
  string sha256 = 4;     // optional if computed
  repeated KeyValue metadata = 5; // variant, module, etc.
}

message ListArtifactsRequest {
  Id project_id = 1;
  BuildVariant variant = 2;
}
message ListArtifactsResponse {
  repeated Artifact artifacts = 1;
}

service BuildService {
  rpc Build(BuildRequest) returns (BuildResponse);
  rpc ListArtifacts(ListArtifactsRequest) returns (ListArtifactsResponse);
}
```

---

## B.8 `target.proto` (targets + deploy + logcat)

```proto
syntax = "proto3";

package aadk.v1;

import "aadk/v1/common.proto";
import "aadk/v1/errors.proto";

enum TargetKind {
  TARGET_KIND_UNSPECIFIED = 0;
  TARGET_KIND_DEVICE = 1;        // physical device via adb
  TARGET_KIND_EMULATORLIKE = 2;  // e.g. Cuttlefish target
  TARGET_KIND_REMOTE = 3;        // future
}

message Target {
  Id target_id = 1;
  TargetKind kind = 2;

  string display_name = 3;   // "Pixel 9"
  string provider = 4;       // "adb", "cuttlefish"
  string address = 5;        // e.g. "ABC123" or "192.168.1.50:5555"

  string api_level = 6;      // "35"
  string state = 7;          // "connected", "unauthorized", "offline", provider-defined
  repeated KeyValue details = 8; // model, brand, transport, etc.
}

message ListTargetsRequest {
  bool include_offline = 1;
}
message ListTargetsResponse {
  repeated Target targets = 1;
}

message SetDefaultTargetRequest {
  Id target_id = 1;
}
message SetDefaultTargetResponse {
  bool ok = 1;
}

message GetDefaultTargetRequest {}
message GetDefaultTargetResponse {
  Target target = 1;
}

message InstallApkRequest {
  Id target_id = 1;
  Id project_id = 2;
  string apk_path = 3; // from BuildService artifacts
}
message InstallApkResponse {
  Id job_id = 1; // job: target.install
}

message LaunchRequest {
  Id target_id = 1;
  string application_id = 2; // e.g. com.example.app
  string activity = 3;       // e.g. ".MainActivity"
}
message LaunchResponse {
  Id job_id = 1; // job: target.launch
}

message StopAppRequest {
  Id target_id = 1;
  string application_id = 2;
}
message StopAppResponse {
  Id job_id = 1; // job: target.stop
}

message StreamLogcatRequest {
  Id target_id = 1;
  string filter = 2;         // optional, e.g. application id/tag
  bool include_history = 3;
}

message LogcatEvent {
  Timestamp at = 1;
  Id target_id = 2;
  bytes line = 3;            // raw line bytes
}

service TargetService {
  rpc ListTargets(ListTargetsRequest) returns (ListTargetsResponse);
  rpc SetDefaultTarget(SetDefaultTargetRequest) returns (SetDefaultTargetResponse);
  rpc GetDefaultTarget(GetDefaultTargetRequest) returns (GetDefaultTargetResponse);

  rpc InstallApk(InstallApkRequest) returns (InstallApkResponse);
  rpc Launch(LaunchRequest) returns (LaunchResponse);
  rpc StopApp(StopAppRequest) returns (StopAppResponse);

  rpc StreamLogcat(StreamLogcatRequest) returns (stream LogcatEvent);
}
```

---

## B.9 `observe.proto` (history + evidence bundles + support export)

```proto
syntax = "proto3";

package aadk.v1;

import "aadk/v1/common.proto";
import "aadk/v1/errors.proto";

message RunRecord {
  Id run_id = 1;
  Id project_id = 2;
  Id target_id = 3;
  Id toolchain_set_id = 4;

  Timestamp started_at = 5;
  Timestamp finished_at = 6;
  string result = 7; // "SUCCESS", "FAILED", "CANCELLED"

  repeated Id job_ids = 8;
  repeated KeyValue summary = 9; // durations, artifact paths, etc.
}

message ListRunsRequest {
  Pagination page = 1;
}
message ListRunsResponse {
  repeated RunRecord runs = 1;
  PageInfo page_info = 2;
}

message ExportSupportBundleRequest {
  bool include_logs = 1;
  bool include_config = 2;
  bool include_toolchain_provenance = 3;
  bool include_recent_runs = 4;
  uint32 recent_runs_limit = 5;
}
message ExportSupportBundleResponse {
  Id job_id = 1; // job: observe.export_support_bundle
  string output_path = 2; // path to zip/tar
}

message ExportEvidenceBundleRequest {
  Id run_id = 1;
}
message ExportEvidenceBundleResponse {
  Id job_id = 1; // job: observe.export_evidence_bundle
  string output_path = 2;
}

service ObserveService {
  rpc ListRuns(ListRunsRequest) returns (ListRunsResponse);
  rpc ExportSupportBundle(ExportSupportBundleRequest) returns (ExportSupportBundleResponse);
  rpc ExportEvidenceBundle(ExportEvidenceBundleRequest) returns (ExportEvidenceBundleResponse);
}
```

---

# Part C - Canonical user flows mapped to API calls (GUI <-> services)

This provides an implementable "interaction contract" for the GUI.

## C.1 First-run setup flow

1. GUI calls `ToolchainService.ListProviders`
2. GUI calls `ToolchainService.ListAvailable` for SDK + NDK providers
3. User selects versions, clicks Install
4. GUI calls `ToolchainService.InstallToolchain` twice (SDK then NDK)
5. GUI subscribes to `JobService.StreamJobEvents(job_id)` for each install
6. After installs succeed:

   * GUI calls `ToolchainService.CreateToolchainSet`
   * GUI calls `ToolchainService.SetActiveToolchainSet`
7. GUI triggers `JobService.StartJob(job_type="doctor.run")` (implementation detail: doctor could be a job type on JobService)
8. GUI shows results

## C.2 Create project + run flow

1. GUI calls `ProjectService.ListTemplates`
2. GUI calls `ProjectService.CreateProject`
3. Stream job events until success
4. GUI calls `BuildService.Build(project_id, DEBUG, clean_first=false)`
5. Stream build events; on success:
6. GUI calls `BuildService.ListArtifacts` and selects debug APK
7. GUI calls `TargetService.GetDefaultTarget` (or user selects)
8. GUI calls `TargetService.InstallApk`
9. Stream events; then `TargetService.Launch`
10. GUI opens `TargetService.StreamLogcat`

## C.3 Evidence mode flow

1. GUI triggers a single orchestrated job via `JobService.StartJob(job_type="evidence.run")`
2. Control plane internally sequences build/install/launch and records `RunRecord`
3. GUI streams job events and shows the checklist UI
4. User clicks "Export Evidence Bundle"
5. GUI calls `ObserveService.ExportEvidenceBundle(run_id)`

---

# Part D - Notes that affect implementation quality (important)

## D.1 Versioning rules for proto stability

* Never reuse field numbers.
* Prefer additive changes.
* Use `oneof` for expanding event payloads.
* Maintain backward compatibility for the GUI of version N consuming services of version N-1 (within major version).

## D.2 Backpressure and log streaming

* `JobService.StreamJobEvents` and `TargetService.StreamLogcat` are server streams.
* Implement log batching in services:

  * Combine small writes into `LogChunk` up to a target size (e.g., 4-32KB).
  * Include `truncated=true` if dropping data, but prefer spilling to disk and exposing export.

## D.3 Security and provenance in GUI

* Toolchain install jobs must record:

  * URL
  * expected hash (if published)
  * computed hash
  * verification status
* GUI must surface "Verified [x]" explicitly.

---
## Software Requirements Specification (SRS) v2.0

### Project: **Arm64 Android DevKit (AADK)** - GUI-first, lightweight Android "Studio-minimum" for ARM64 Linux

**Document type:** Software Requirements Specification (GUI-first baseline)
**Version:** 2.0 (supersedes SRS v1.0)
**Date:** 2026-01-07
**Primary goal:** Provide an **ARM64-native** development environment that achieves the practical outcomes of Android Studio's "create/build/run/log" loop with **minimal overhead**, a **first-class GUI**, and an **extensible service-oriented architecture**.

---

# 0. Executive Summary

AADK is a GUI-first application and local service suite that enables Android app development on ARM64 Linux hosts (e.g., Raspberry Pi 5) by bundling:

* A high-performance **native GUI** (planned now as a first-class deliverable)
* A modular **local service layer** for toolchain management, building, target management, and logging
* ARM64-host-compatible SDK/NDK "drop-in replacement" toolchains (e.g., `android-ndk-custom`, `android-sdk-custom`) to avoid the official host-architecture limitations

This project is motivated by two hard realities:

1. **Android Studio is not supported on Linux ARM64** (official install page explicitly states Linux ARM-based CPUs are not supported). ([Android Developers][1])
2. Official Android NDK downloadable packages for Linux are labeled **"Linux 64-bit (x86)"**, not ARM64, so ARM64 hosts cannot rely on official host distributions for NDK. ([Android Developers][2])

Therefore, AADK's design centers on: **native performance**, **repeatable builds**, **extensibility**, and **explicit integration of ARM64-host toolchain distributions** such as:

* `android-ndk-custom`: a custom NDK intended as a **drop-in replacement**; supports `aarch64` as a host architecture. ([GitHub][3])
* `android-sdk-custom`: a custom SDK intended as a **drop-in replacement**; replaces binaries with **musl-based** ones built using Zig and supports `aarch64`. ([GitHub][4])
* `android-ndk-custom` releases publish `aarch64-linux-android` and `aarch64-linux-musl` archives with SHA-256 hashes (enabling strict verification). ([GitHub][5])

---

# 1. Introduction

## 1.1 Purpose

This SRS defines complete requirements for a **GUI-first** ARM64 Android development kit that provides:

* Project creation (Kotlin + Jetpack Compose templates)
* Toolchain installation and activation (SDK/NDK)
* Build orchestration (Gradle/AGP)
* Target management (physical devices via ADB, emulator-like targets)
* Run/deploy and log streaming
* "Evidence mode" for coursework/screencast flows

The document is intended for:

* Core maintainers (architecture, language/toolkit decisions)
* Contributors (service boundaries, APIs, extension points)
* Downstream packagers (Debian packages, tarballs)
* Evaluators (functional acceptance criteria)

## 1.2 Scope

AADK is **not** an IDE. It does not aim to replace:

* code editing features,
* indexing/refactoring,
* Compose Preview,
* AVD Manager parity.

Instead, it targets the **minimum loop** required to ship working apps and satisfy common assignments: "create -> build -> run -> interact -> show logs".

## 1.3 Definitions

* **Host:** machine running AADK (ARM64 Linux).
* **Target:** environment running the Android app (physical device; emulator-like provider).
* **Toolchain:** SDK + NDK + build tools + platform tools used by builds.
* **Provider:** pluggable implementation of a toolchain source or target source.
* **Service:** local background process exposing APIs to the GUI and CLI.

## 1.4 References

Key constraint references already enumerated in the Executive Summary:

* Android Studio Linux ARM not supported. ([Android Developers][1])
* Official NDK Linux downloads are x86. ([Android Developers][2])
* `android-ndk-custom` drop-in and `aarch64` host support. ([GitHub][3])
* `android-ndk-custom` `aarch64` archives + SHA-256 in releases. ([GitHub][5])
* `android-sdk-custom` drop-in and musl/Zig + `aarch64` support. ([GitHub][4])

---

# 2. Product Overview (GUI-first)

## 2.1 Product perspective

AADK is a **native desktop application** consisting of:

* **GUI Application** (primary entry point)
* **Local service suite** (toolchain/build/targets/logs)
* **Optional CLI** (power-user and automation interface)
* **Plugin/provider ecosystem** (toolchains, targets, templates)

The GUI must be functional without the CLI. The CLI must be able to drive all actions exposed by the GUI (API parity), but the GUI is the "design center."

## 2.2 User classes

1. **Student / assignment-driven user**

   * Needs deterministic "project created + runs + state changes" evidence quickly.
2. **ARM64 hobbyist**

   * Needs local builds and device deployment on SBC hardware.
3. **Contributor / maintainer**

   * Extends providers, templates, packaging.
4. **Instructor / evaluator**

   * Wants verifiable run evidence, repeatability, and clear outputs.

## 2.3 Operating environment

* Host OS: Debian-family ARM64 Linux (primary), other ARM64 distros (secondary).
* Minimum hardware target: ARM64 quad-core, 8GB RAM, SSD/NVMe recommended.
* Targets:

  * ADB-connected physical Android devices (USB or Wi-Fi debugging)
  * Emulator-like provider(s) suitable for ARM64 (container-based Android is a primary candidate)
  * Optional remote targets (future)

## 2.4 Constraints and rationale

* **No reliance on Android Studio on host**, because Linux ARM is explicitly unsupported. ([Android Developers][1])
* **No reliance on official Linux NDK downloads**, because the published Linux packages are x86. ([Android Developers][2])
* Toolchain sources must include ARM64-compatible distributions such as `android-ndk-custom` and `android-sdk-custom`, each described as drop-in replacements with ARM64 host support. ([GitHub][3])

---

# 3. Language and GUI Toolkit Requirements (Performance-Optimized)

This section answers your explicit request: "determine the proper language to use... highly optimize performance" and treats it as a **hard requirement** rather than a preference.

## 3.1 Primary implementation language (MANDATORY)

**REQ-LANG-001**: All first-party AADK components (GUI, services, CLI) **shall be implemented in Rust**, except where third-party toolchains are integrated as external artifacts or subprocesses (e.g., NDK/SDK build systems implemented in C/C++).
**REQ-LANG-002**: Rust edition and compiler baseline shall be pinned and documented to ensure reproducible builds.

**Rationale:** Rust enables predictable performance, memory safety, and strong concurrency for streaming logs/progress while keeping the UI responsive on ARM64 SBCs.

## 3.2 GUI toolkit (MANDATORY)

**REQ-GUI-STACK-001**: The GUI shall be implemented using a native Linux toolkit with minimal runtime overhead; the baseline stack shall be **GTK 4 via gtk-rs**.
**REQ-GUI-STACK-002**: The GUI shall not embed a full browser runtime as its primary rendering engine in MVP (i.e., no Electron-class runtime).

**Rationale:** The project prioritizes performance and low memory overhead on ARM64; a native toolkit avoids the fixed costs of bundling a full web engine.

## 3.3 Service IPC (MANDATORY)

**REQ-IPC-001**: GUI <-> services communication shall use **gRPC** over **Unix domain sockets** by default.
**REQ-IPC-002**: All APIs shall be versioned and backward-compatible within a major version.

---

# 4. System Architecture (Service-Oriented, GUI-driven)

## 4.1 High-level component model

AADK consists of:

1. **GUI (aadk-ui)**

   * Presents wizards, dashboards, build console, logs, target manager.
2. **Control Plane (aadk-core)**

   * Central coordinator, configuration manager, provider registry, job scheduler.
3. **Toolchain Service (aadk-toolchain)**

   * Downloads, verifies, installs, activates SDK/NDK distributions.
4. **Build Service (aadk-build)**

   * Orchestrates Gradle builds, captures structured outputs, caches artifacts.
5. **Target Service (aadk-targets)**

   * Discovers ADB devices; manages emulator-like providers; installs/launches apps.
6. **Observability Service (aadk-observe)**

   * Event bus, structured logs, run history, export bundles.

The GUI is a client; it never "owns" long-running tasks.

## 4.2 Core design principles

* **Single source of truth:** the control plane owns state and config.
* **Jobs are first-class:** every action is a job with progress + logs + result.
* **Streaming-first UI:** logs and progress are streamed to avoid blocking.
* **Provider extensibility:** toolchains/targets/templates are pluggable.

## 4.3 Provider model

### 4.3.1 Toolchain providers

**REQ-PROV-TC-001**: Toolchain providers shall support:

* listing available versions
* downloading archives
* verifying integrity (hash/signature when available)
* installing into an AADK-managed directory
* exposing environment variables and path mappings per project

**REQ-PROV-TC-002**: MVP must include providers for:

* `android-ndk-custom` (ARM64 host supported; drop-in replacement). ([GitHub][3])
* `android-sdk-custom` (drop-in replacement; musl/Zig-based binaries; includes `aarch64`). ([GitHub][4])

**REQ-PROV-TC-003**: Provider shall enforce strict verification when hashes are available (e.g., SHA-256 entries in release assets). ([GitHub][5])

### 4.3.2 Target providers

**REQ-PROV-TGT-001**: Target providers shall expose a uniform interface:

* list targets
* select default target
* install APK
* launch activity
* stream logs (logcat or equivalent)

**REQ-PROV-TGT-002**: MVP must include:

* ADB physical device provider
* at least one emulator-like provider suitable for ARM64

---

# 5. GUI Requirements (Complete, Screen-Level)

## 5.1 Global GUI requirements

**REQ-UI-001**: The GUI shall be usable without a terminal for all MVP workflows.
**REQ-UI-002**: The GUI shall provide a consistent navigation model (left rail or top tabs) with explicit status indicators for Toolchain, Targets, Builds, and Runs.
**REQ-UI-003**: All long-running operations shall be cancellable where safe (downloads, builds, installs).
**REQ-UI-004**: The GUI shall not freeze during builds; UI thread stalls > 100ms shall be treated as defects.
**REQ-UI-005**: The GUI shall provide an "Export Support Bundle" feature (logs, config snapshot, versions) for debugging and community support.

## 5.2 Primary screens and functional requirements

### 5.2.1 First-Run Wizard (Setup)

**Goal:** Get to "Doctor: green" and a working toolchain.

**UI elements**

* Welcome panel with detected host: OS, arch, RAM, disk free
* Toolchain selection (NDK provider, SDK provider)
* Install location chooser (default: user home)
* "Install" button with progress view
* "Run Doctor" button
* "Finish" button

**Requirements**

* **REQ-SETUP-001**: Wizard shall detect host architecture and block configurations that cannot work (e.g., selecting official Linux x86 NDK on ARM64). The rationale is consistent with official NDK Linux packages being x86. ([Android Developers][2])
* **REQ-SETUP-002**: Wizard shall enforce toolchain integrity checks when provider publishes hashes. ([GitHub][5])
* **REQ-SETUP-003**: Wizard shall record installed toolchain versions and set a default active toolchain.

**Flow (happy path)**

1. Launch AADK -> "First run detected"
2. Choose SDK provider (android-sdk-custom) + NDK provider (android-ndk-custom)
3. Install -> progress stream
4. Doctor -> success
5. Finish -> main dashboard

### 5.2.2 Dashboard (Home)

**UI elements**

* Status cards:

  * Toolchain active version
  * Targets connected
  * Last build result
  * Last run result
* Quick actions:

  * New Project
  * Open Project
  * Run (uses last project + default target)
  * Evidence Mode

**Requirements**

* **REQ-DASH-001**: Dashboard shall reflect live service state (if a service is down, show it).
* **REQ-DASH-002**: Dashboard shall show an actionable error if Android Studio is attempted to be installed locally; messaging must reference that Linux ARM is not supported (as a product constraint). ([Android Developers][1])

### 5.2.3 Projects Screen

**UI elements**

* Recent projects list (path, last build, last run)
* Buttons: New, Open, Remove from list
* Project details panel: toolchain selection override, target selection override

**Requirements**

* **REQ-PROJ-001**: A project can override the global active toolchain (pinning per-project).
* **REQ-PROJ-002**: GUI must show the project's Gradle/AGP/Kotlin versions (read-only in MVP) and warn on incompatibilities.

### 5.2.4 New Project Wizard (Compose + Kotlin)

**Goal:** Create a minimal Compose app with demonstrable state updates.

**Inputs**

* Project name
* Location
* applicationId / namespace
* minSdk, targetSdk (defaults)
* Template selection (Compose "Counter", Compose "Form", etc.)

**Requirements**

* **REQ-TPL-001**: MVP must include at least one Compose template that demonstrates state updates via user interaction.
* **REQ-TPL-002**: Wizard must support "Create and Run" at the end of the flow.

**Flow**

1. New Project -> choose template
2. Set IDs -> Create
3. Post-create options: Open folder, Build, Run

### 5.2.5 Toolchains Screen

**UI elements**

* Installed toolchains table (SDK, NDK versions, provider, path, status)
* Available toolchains list (from provider)
* Buttons: Install, Remove, Set Active, Verify
* "Advanced" collapsible: show hashes, archive URLs, provenance

**Requirements**

* **REQ-TC-001**: Toolchains screen shall allow installing `android-ndk-custom` and `android-sdk-custom`, each explicitly described upstream as a "drop-in replacement." ([GitHub][3])
* **REQ-TC-002**: Toolchains screen shall show SHA-256 verification status when available (e.g., hashes published in releases). ([GitHub][5])
* **REQ-TC-003**: Toolchains screen shall allow offline operation if archives are already present.

### 5.2.6 Targets Screen

**UI elements**

* Physical devices list (serial, model, API, connection type)
* Emulator-like targets list (provider, status)
* Buttons: Refresh, Set Default, Connect (for Wi-Fi ADB), Start/Stop emulator-like target (provider-dependent)

**Requirements**

* **REQ-TGT-001**: ADB device discovery must be real-time refreshable.
* **REQ-TGT-002**: Default target selection must be explicit and visible across the GUI.
* **REQ-TGT-003**: The target provider must expose install/launch/logs features uniformly.

### 5.2.7 Build & Run Console (Central Screen)

This is the most performance-sensitive part of the GUI.

**UI elements**

* Build controls: Build, Clean, Rebuild
* Run controls: Install, Launch, Run (build+install+launch)
* Artifact panel: APK path(s), size, variant
* Logs panel with tabs:

  * Build output (Gradle)
  * Logcat (target)
  * Diagnostics (parsed errors)

**Requirements**

* **REQ-CONSOLE-001**: Logs must be **virtualized** (render only visible lines); the system must avoid unbounded in-memory text buffers.
* **REQ-CONSOLE-002**: Provide filtering and search without re-rendering the entire log buffer on each keystroke (debounce).
* **REQ-CONSOLE-003**: Provide "copy error summary" extracted from parsed diagnostics.
* **REQ-CONSOLE-004**: Show job timeline: queued -> running -> completed with durations.

### 5.2.8 Evidence Mode (Screencast / Coursework)

**UI elements**

* "One-click Evidence Run" button
* Checklist pane:

  * toolchain active
  * target selected
  * build succeeded
  * app installed
  * app launched
  * interaction performed (manual checkbox + optional instrumentation)
* Timestamped run summary export

**Requirements**

* **REQ-EVID-001**: Evidence mode shall run a deterministic pipeline:

  * clean -> build debug -> install -> launch -> open logcat
* **REQ-EVID-002**: Evidence mode shall generate an exportable summary report (JSON + human-readable) for grading.

---

# 6. Functional Requirements (System-Level)

## 6.1 Toolchain installation and activation

**FR-001**: Install SDK/NDK toolchains via providers.
**FR-002**: Verify toolchain archives via hashes when available (required for `android-ndk-custom` releases that publish sha256 lines). ([GitHub][5])
**FR-003**: Support multiple installed versions; allow per-project pinning.
**FR-004**: Support `android-ndk-custom` as drop-in replacement; must support `aarch64` host. ([GitHub][3])
**FR-005**: Support `android-sdk-custom` as drop-in replacement; must support `aarch64` host and musl/Zig binary model. ([GitHub][4])

## 6.2 Project scaffolding

**FR-010**: Create Kotlin + Compose Android app templates.
**FR-011**: Template must include at least:

* one Compose screen
* one state variable
* user interaction that changes UI state
  **FR-012**: Projects must be buildable without Android Studio.

## 6.3 Build orchestration

**FR-020**: Run Gradle builds via wrapper; stream logs and task progress.
**FR-021**: Provide structured diagnostics extraction from Gradle output (compile errors, missing SDK, dex errors).
**FR-022**: Cache artifacts; provide "clean" option.

## 6.4 Target management and deployment

**FR-030**: Discover devices via ADB.
**FR-031**: Install APK to target.
**FR-032**: Launch main activity.
**FR-033**: Stream logs from target.

---

# 7. Nonfunctional Requirements (Performance-Optimized)

## 7.1 Performance budgets (explicit)

These requirements are written to align with ARM64 SBC realities and the "highly optimize performance" objective.

**NFR-PERF-001 (Cold start):** GUI must be interactive within **<= 1.5 seconds** on a Raspberry Pi 5-class device (SSD/NVMe).
**NFR-PERF-002 (Idle memory):** GUI + all services combined shall target **<= 600MB RSS** at idle (no project open), excluding filesystem cache.
**NFR-PERF-003 (Build responsiveness):** During a build, the GUI must maintain:

* input latency under typical load (no visible stutter from log streaming)
* no UI thread stalls > 100ms (soft), > 500ms (hard defect)

**NFR-PERF-004 (Log throughput):** The console must handle >= 5,000 log lines/min without unbounded memory growth.

## 7.2 Reliability

**NFR-REL-001:** Toolchain installs must be transactional (no partial "active" toolchain).
**NFR-REL-002:** Interrupted jobs must be resumable or cleanly restartable.
**NFR-REL-003:** Services must auto-restart; GUI must degrade gracefully and offer repair actions.

## 7.3 Security

**NFR-SEC-001:** Verify hashes when published; refuse activation on mismatch. ([GitHub][5])
**NFR-SEC-002:** IPC endpoints must be local-only by default (Unix sockets).
**NFR-SEC-003:** No background network calls without explicit user action (downloads are explicit).

## 7.4 Maintainability

**NFR-MNT-001:** Public APIs versioned; protobuf definitions treated as stable contracts.
**NFR-MNT-002:** Provider interfaces documented and tested with conformance tests.
**NFR-MNT-003:** One-command developer setup for contributors (build/test/run).

## 7.5 Observability

**NFR-OBS-001:** Structured event log with correlation IDs per job.
**NFR-OBS-002:** "Export bundle" includes:

* versions
* configs (redacted)
* last N job logs
* environment diagnostics

---

# 8. External Interface Requirements

## 8.1 User interface conventions

* Consistent terminology across screens: "Toolchain", "Target", "Build", "Run".
* Visible active selections (active toolchain, default target) at all times.
* Errors presented with:

  * what failed
  * why it likely failed
  * one-click remediation actions

## 8.2 API interface (gRPC)

Core service APIs shall include:

* `JobService`

  * `StartJob`, `CancelJob`, `GetJob`, `StreamJobEvents`
* `ToolchainService`

  * `ListProviders`, `ListAvailable`, `Install`, `Verify`, `Activate`, `ListInstalled`
* `ProjectService`

  * `CreateProject`, `OpenProject`, `ListRecent`, `GetProjectConfig`, `SetProjectConfig`
* `BuildService`

  * `Build`, `Clean`, `GetArtifacts`, `StreamBuildOutput`
* `TargetService`

  * `ListTargets`, `SetDefaultTarget`, `InstallApk`, `Launch`, `StreamLogcat`

Error model:

* typed error codes
* structured details
* user-action suggestions (optional field)

---

# 9. Packaging and Distribution Requirements

**PKG-001:** Provide:

* Debian `.deb` packages (preferred for Debian users)
* tarball distribution as fallback

**PKG-002:** Default install mode is per-user (no root required), with optional system-wide install.

**PKG-003:** Toolchains installed under an AADK-managed directory with:

* provenance file (provider, version, hash)
* immutable install directory
* active symlink or config mapping

---

# 10. Acceptance Criteria (GUI-first MVP)

AADK v1.0 (GUI-first) is accepted when:

1. Fresh install -> First-run wizard installs toolchains successfully.
2. "Doctor" screen returns green status (JDK + SDK/NDK + ADB).
3. "New Project" wizard creates a Compose + Kotlin project.
4. "Run" deploys and launches the app on a selected target.
5. The app UI demonstrates state update via user interaction (button/input).
6. Build & Run console streams build output and logcat without freezing.
7. Evidence mode runs the pipeline and produces an exportable run summary.

---

# 11. Traceability (sample mapping)

* GUI setup wizard -> REQ-SETUP-001..003, FR-001..005, NFR-SEC-001
* New project (Compose) -> REQ-TPL-001..002, FR-010..012
* Build/run console -> REQ-CONSOLE-001..004, FR-020..022, NFR-PERF-004
* Targets screen -> REQ-TGT-001..003, FR-030..033
* Evidence mode -> REQ-EVID-001..002, acceptance criteria 5-7

---

# 12. Implementation Constraints and Integration Notes (Toolchain realities)

1. **Official Android Studio is not a viable host dependency on ARM64 Linux**. ([Android Developers][1])
2. **Official NDK host packages for Linux are x86**, so ARM64 host support must come from alternative distributions. ([Android Developers][2])
3. `android-ndk-custom` explicitly describes "drop-in replacement" usage and includes `aarch64` as supported host architecture. ([GitHub][3])
4. `android-ndk-custom` releases list ARM64 host archives and publish sha256 hashes, enabling strict verification policies. ([GitHub][5])
5. `android-sdk-custom` describes itself as a "drop-in replacement", replacing default binaries with musl-based ones built using Zig, and lists `aarch64` among supported architectures. ([GitHub][4])

These are not incidental; they are requirements drivers for AADK's provider model and verification policies.

---

[1]: https://developer.android.com/studio/install "Install Android Studio  |  Android Developers"
[2]: https://developer.android.com/ndk/downloads "NDK Downloads  |  Android NDK  |  Android Developers"
[3]: https://github.com/HomuHomu833/android-ndk-custom "GitHub - HomuHomu833/android-ndk-custom: Android NDK with custom LLVM built using various libc's, supporting multiple architectures and platforms."
[4]: https://github.com/HomuHomu833/android-sdk-custom "GitHub - HomuHomu833/android-sdk-custom: Android SDK built using musl libc, supporting multiple architectures."
[5]: https://github.com/HomuHomu833/android-ndk-custom/releases "Releases - HomuHomu833/android-ndk-custom - GitHub"
