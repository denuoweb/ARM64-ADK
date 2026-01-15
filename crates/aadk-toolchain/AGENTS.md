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
  VerifyToolchain, UpdateToolchain, UninstallToolchain, CleanupToolchainCache,
  CreateToolchainSet, SetActiveToolchainSet, GetActiveToolchainSet

## Current implementation details
- Implementation is split across crates/aadk-toolchain/src/service.rs (gRPC + orchestration),
  crates/aadk-toolchain/src/catalog.rs (catalog/fixtures), crates/aadk-toolchain/src/artifacts.rs
  (download/extract), crates/aadk-toolchain/src/verify.rs (signature/transparency checks),
  crates/aadk-toolchain/src/state.rs (persisted state/toolchain sets), crates/aadk-toolchain/src/jobs.rs
  (JobService helpers), crates/aadk-toolchain/src/hashing.rs (sha256 helpers),
  crates/aadk-toolchain/src/provenance.rs (provenance I/O), and crates/aadk-toolchain/src/cancel.rs;
  crates/aadk-toolchain/src/main.rs only wires the tonic server.
- Providers and versions come from a JSON catalog (crates/aadk-toolchain/catalog.json or
  AADK_TOOLCHAIN_CATALOG override).
- Catalog pins SDK versions 36.0.0 and 35.0.2 plus NDK versions r29, r28c, r27d, and r26d for the
  android-* custom providers, with linux-aarch64 (musl), aarch64-linux-android, and
  aarch64_be-linux-musl artifacts where available.
- Host selection uses AADK_TOOLCHAIN_HOST when set and falls back to host aliases (for example,
  linux-aarch64 -> aarch64-linux-musl/aarch64-linux-android/aarch64_be-linux-musl) plus
  AADK_TOOLCHAIN_HOST_FALLBACK when the catalog lacks a matching artifact.
- Available versions can also be sourced from fixture archives via AADK_TOOLCHAIN_FIXTURES_DIR.
- Toolchains are installed under ~/.local/share/aadk/toolchains and cached in
  ~/.local/share/aadk/downloads; state is persisted in ~/.local/share/aadk/state/toolchains.json.
- Install/Update/Verify job progress metrics include provider/version/host/verify settings plus artifact URLs/paths and install roots.
- verify_toolchain checks install path, provenance contents, catalog consistency, artifact size,
  optional Ed25519 signatures (over SHA256 digest), transparency log entries when configured, and
  layout; it re-fetches the artifact for hash verification when needed.
- Catalog artifacts can supply signature metadata via `signature`, `signature_url`, and
  `signature_public_key` (hex or base64); signatures are recorded in provenance when available.
- InstallToolchain and VerifyToolchain accept optional job_id to reuse existing JobService jobs,
  plus correlation_id and run_id to group multi-step workflows.
- InstallToolchain and VerifyToolchain trim provider/toolchain identifiers and reject empty values with invalid-argument errors.
- Update/Uninstall/Cleanup cache operations publish JobService events and can reuse job_id while
  honoring correlation_id and run_id for grouped job streams.
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

## Implementation notes
- InstallToolchain clones artifact URL/hash when persisting InstalledToolchain so post-install metrics can reuse artifact metadata.

## Prioritized TODO checklist by service
- None (workflow UI consumes existing toolchain RPCs).
