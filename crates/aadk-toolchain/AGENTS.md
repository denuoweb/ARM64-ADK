# ToolchainService Agent Notes (aadk-toolchain)

## Role and scope
ToolchainService exposes SDK/NDK provider metadata, installs toolchains, verifies installs,
tracks installed toolchains, and manages toolchain sets. It publishes job progress to JobService
for long-running actions.

## Maintenance
Update this file whenever ToolchainService behavior changes or when commits touching this crate are made.

## gRPC contract
- proto/aadk/v1/toolchain.proto
- RPCs: ListProviders, ListAvailable, ListInstalled, ListToolchainSets, InstallToolchain,
  VerifyToolchain, CreateToolchainSet, SetActiveToolchainSet, GetActiveToolchainSet

## Current implementation details
- Implementation lives in crates/aadk-toolchain/src/main.rs with a tonic server.
- Providers and versions come from a JSON catalog (crates/aadk-toolchain/catalog.json or
  AADK_TOOLCHAIN_CATALOG override).
- Host selection uses AADK_TOOLCHAIN_HOST when set and falls back to host aliases plus
  AADK_TOOLCHAIN_HOST_FALLBACK when the catalog lacks a matching artifact.
- Available versions can also be sourced from fixture archives via AADK_TOOLCHAIN_FIXTURES_DIR.
- Toolchains are installed under ~/.local/share/aadk/toolchains and cached in
  ~/.local/share/aadk/downloads; state is persisted in ~/.local/share/aadk/state/toolchains.json.
- verify_toolchain checks that the install path exists, a provenance file exists, and
  validates layout; it re-fetches the artifact for verification when needed.
- InstallToolchain and VerifyToolchain accept optional job_id to reuse existing JobService jobs.
- Toolchain sets are persisted in ~/.local/share/aadk/state/toolchains.json along with the active
  toolchain set id.

## Data flow and dependencies
- Uses JobService for install/verify jobs and publishes logs/progress events.
- UI/CLI call ListProviders/ListAvailable/ListInstalled/ListToolchainSets/Install/Verify plus
  toolchain set management RPCs.

## Environment / config
- AADK_TOOLCHAIN_ADDR sets the bind address (default 127.0.0.1:50052).
- AADK_JOB_ADDR sets the JobService address.
- AADK_TOOLCHAIN_FIXTURES_DIR points to local fixture archives for offline dev.
- AADK_TOOLCHAIN_CATALOG overrides the provider catalog path.
- AADK_TOOLCHAIN_HOST overrides detected host (e.g., linux-aarch64, linux-x86_64).
- AADK_TOOLCHAIN_HOST_FALLBACK provides a comma-separated fallback host list.

## Prioritized TODO checklist by service
- P1: Add uninstall/update operations and cached-artifact cleanup. main.rs (line 1161)
- P2: Strengthen verification (signatures/provenance, not just hash). main.rs (line 1263)
