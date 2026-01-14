# TargetService Agent Notes (aadk-targets)

## Role and scope
TargetService enumerates devices/emulators via adb, exposes optional Cuttlefish integration,
installs APKs, launches/stops apps, and streams logcat. It publishes progress/logs to JobService
for long-running actions (install/start/stop/launch/logcat/cuttlefish actions).

## Maintenance
Update this file whenever TargetService behavior changes or when commits touching this crate are made.

## gRPC contract
- proto/aadk/v1/target.proto
- RPCs: ListTargets, SetDefaultTarget, GetDefaultTarget, InstallApk, Launch, StopApp,
  StreamLogcat, InstallCuttlefish, ResolveCuttlefishBuild, StartCuttlefish, StopCuttlefish, GetCuttlefishStatus

## Current implementation details
- Implementation lives in crates/aadk-targets/src/main.rs with a tonic server.
- list_targets uses a provider pipeline (ADB listing plus Cuttlefish augmentation), normalizes
  target IDs/addresses, enriches metadata/health, and merges persisted inventory for offline targets.
- default target and target inventory are persisted under ~/.local/share/aadk/state/targets.json
  and reconciled against live discovery when possible.
- APK install/launch/stop and logcat are implemented via adb commands.
- Cuttlefish install can accept per-request branch/target/build_id overrides; build resolution is
  exposed via ResolveCuttlefishBuild.
- Cuttlefish operations run external commands and report state via JobService events.
- Job progress metrics include target/app identifiers, adb serials, install/launch inputs, and
  target health/ABI/SDK details plus Cuttlefish env/artifact details.
- InstallApk, Launch, StopApp, and Cuttlefish job RPCs accept optional job_id for existing jobs
  plus correlation_id and run_id for grouping related workflows.
- Cuttlefish start preflight checks host tools, images, and KVM availability/access (configurable).
- Defaults align with aosp-android-latest-release and aosp_cf_*_only_phone-userdebug targets; 16K hosts use main-16k-with-phones with aosp_cf_arm64/aosp_cf_x86_64.
- GPU mode can be set via AADK_CUTTLEFISH_GPU_MODE and is appended to launch arguments when starting Cuttlefish.
- Start adds --start_webrtc based on show_full_ui unless already provided in AADK_CUTTLEFISH_START_ARGS.
- Cuttlefish details and job outputs include WebRTC and environment control URLs.

## Data flow and dependencies
- Requires JobService to publish job state/log/progress for install/launch/stop/cuttlefish jobs.
- UI/CLI typically call list_targets with include_offline=true.

## Environment / config
- AADK_TARGETS_ADDR sets the bind address (default 127.0.0.1:50055).
- AADK_JOB_ADDR sets the JobService address.
- AADK_ADB_PATH or ANDROID_SDK_ROOT/ANDROID_HOME can override adb lookup.

### Cuttlefish configuration (env vars)
- AADK_CUTTLEFISH_ENABLE=0 to disable detection
- AADK_CVD_BIN=/path/to/cvd
- AADK_LAUNCH_CVD_BIN=/path/to/launch_cvd
- AADK_STOP_CVD_BIN=/path/to/stop_cvd
- AADK_CUTTLEFISH_ADB_SERIAL=127.0.0.1:6520
- AADK_CUTTLEFISH_CONNECT=0 to skip adb connect
- AADK_CUTTLEFISH_WEBRTC_URL=https://localhost:8443
- AADK_CUTTLEFISH_ENV_URL=https://localhost:1443
- AADK_CUTTLEFISH_PAGE_SIZE_CHECK=0 to skip kernel page-size checks
- AADK_CUTTLEFISH_KVM_CHECK=0 to skip KVM availability/access checks
- AADK_CUTTLEFISH_GPU_MODE=gfxstream|drm_virgl to configure GPU acceleration mode
- AADK_CUTTLEFISH_HOME=/path (or _16K/_4K variants)
- AADK_CUTTLEFISH_IMAGES_DIR=/path (or _16K/_4K variants)
- AADK_CUTTLEFISH_HOST_DIR=/path (or _16K/_4K variants)
- AADK_CUTTLEFISH_START_CMD / AADK_CUTTLEFISH_START_ARGS
- AADK_CUTTLEFISH_STOP_CMD
- AADK_CUTTLEFISH_INSTALL_CMD
- AADK_CUTTLEFISH_INSTALL_HOST=0
- AADK_CUTTLEFISH_INSTALL_IMAGES=0
- AADK_CUTTLEFISH_ADD_GROUPS=0
- AADK_CUTTLEFISH_BRANCH / AADK_CUTTLEFISH_TARGET / AADK_CUTTLEFISH_BUILD_ID

## Prioritized TODO checklist by service
