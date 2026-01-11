# ToolchainService Agent Notes (aadk-toolchain)

## Role and scope
ToolchainService exposes SDK/NDK provider metadata, installs toolchains, verifies installs,
tracks installed toolchains, and manages toolchain sets. It publishes job progress to JobService
for long-running actions.

## gRPC contract
- proto/aadk/v1/toolchain.proto
- RPCs: ListProviders, ListAvailable, ListInstalled, InstallToolchain, VerifyToolchain,
  CreateToolchainSet, SetActiveToolchainSet, GetActiveToolchainSet

## Current implementation details
- Implementation lives in crates/aadk-toolchain/src/main.rs with a tonic server.
- Providers are hard-coded to two "custom" providers:
  - android-sdk-custom (SDK)
  - android-ndk-custom (NDK)
- Available versions are pinned and come from either:
  - A fixtures directory (AADK_TOOLCHAIN_FIXTURES_DIR)
  - A remote host-specific artifact list (currently only linux/aarch64)
- Toolchains are installed under ~/.local/share/aadk/toolchains and cached in
  ~/.local/share/aadk/downloads; state is persisted in ~/.local/share/aadk/state/toolchains.json.
- verify_toolchain checks that the install path exists, a provenance file exists, and
  validates layout; it re-fetches the artifact for verification when needed.
- Toolchain sets are created and "active" is stored in memory only.

## Data flow and dependencies
- Uses JobService for install/verify jobs and publishes logs/progress events.
- UI/CLI call ListProviders/ListAvailable/Install/Verify.

## Environment / config
- AADK_TOOLCHAIN_ADDR sets the bind address (default 127.0.0.1:50052).
- AADK_JOB_ADDR sets the JobService address.
- AADK_TOOLCHAIN_FIXTURES_DIR points to local fixture archives for offline dev.

## Prioritized TODO checklist by service
- P0: Replace fixed providers/versions with provider discovery + version catalog. main.rs (line 162) main.rs (line 1163)
- P0: Expand host support beyond linux/aarch64; add fallback behavior. main.rs (line 224) main.rs (line 263)
- P1: Persist toolchain sets and reference them in set_active. main.rs (line 1496) main.rs (line 1511)
- P1: Add uninstall/update operations and cached-artifact cleanup. main.rs (line 1161)
- P2: Strengthen verification (signatures/provenance, not just hash). main.rs (line 1263)
