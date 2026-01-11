# ObserveService Agent Notes (aadk-observe)

## Role and scope
ObserveService is meant to track run history and produce evidence/support bundles for diagnostics.
It persists run history and exports bundles via JobService-driven jobs.

## Maintenance
Update this file whenever observe behavior changes or when commits touching this crate are made.

## gRPC contract
- proto/aadk/v1/observe.proto
- RPCs: ListRuns, ExportSupportBundle, ExportEvidenceBundle

## Current implementation details
- Implementation lives in crates/aadk-observe/src/main.rs with a tonic server.
- Run history is persisted in ~/.local/share/aadk/state/observe.json and paged in list_runs.
- export_support_bundle creates a JobService job, writes a zip bundle under
  ~/.local/share/aadk/bundles, and streams progress/log events.
- export_evidence_bundle does the same for a single run id.
- Bundle contents include a manifest, optional env/config capture, optional toolchain state, and
  optional recent run snapshots. Log collection is currently a placeholder file.

## Data flow and dependencies
- Uses JobService for bundle jobs and event streaming.
- UI and CLI can list runs and export support/evidence bundles with job streaming.

## Environment / config
- AADK_OBSERVE_ADDR sets the bind address (default 127.0.0.1:50056).

## Prioritized TODO checklist by service
- P1: Capture run metadata (project/target/toolchain ids) for filtering. main.rs (line 17)
- P1: Replace placeholder log/config collection with real files. main.rs (line 24)
- P2: Add retention/cleanup for bundles and temp dirs. main.rs (line 24)
