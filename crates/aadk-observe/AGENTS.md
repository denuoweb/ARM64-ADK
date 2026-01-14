# ObserveService Agent Notes (aadk-observe)

## Role and scope
ObserveService is meant to track run history and produce evidence/support bundles for diagnostics.
It persists run history and exports bundles via JobService-driven jobs.

## Maintenance
Update this file whenever observe behavior changes or when commits touching this crate are made.

## gRPC contract
- proto/aadk/v1/observe.proto
- RPCs: ListRuns, ExportSupportBundle, ExportEvidenceBundle, UpsertRun

## Current implementation details
- Implementation lives in crates/aadk-observe/src/main.rs with a tonic server.
- Run history is persisted in ~/.local/share/aadk/state/observe.json and paged in list_runs.
- ListRuns accepts RunFilter (run_id, correlation_id, project/target/toolchain ids, result).
- UpsertRun merges partial updates (job ids, summary entries, timestamps, result) for best-effort run tracking.
- export_support_bundle creates a JobService job, writes a zip bundle under
  ~/.local/share/aadk/bundles, and streams progress/log events.
- export_evidence_bundle does the same for a single run id.
- Bundle export progress metrics include run_id/output_path plus include flags and item counts.
- ExportSupportBundle/ExportEvidenceBundle accept optional job_id to attach to existing jobs plus
  correlation_id and run_id to group multi-service workflows.
- Runs capture project/target/toolchain ids and correlation_id when provided for filtering.
- Bundle retention prunes old zip files and tmp-* directories by age/count.
- Bundle contents include a manifest, config snapshots (env + state files), optional toolchain
  state, optional recent run snapshots, and job log capture from JobService history
  (`~/.local/share/aadk/state/jobs.json`).

## Data flow and dependencies
- Uses JobService for bundle jobs and event streaming.
- UI and CLI can list runs and export support/evidence bundles with job streaming.

## Environment / config
- AADK_OBSERVE_ADDR sets the bind address (default 127.0.0.1:50056).
- AADK_OBSERVE_BUNDLE_RETENTION_DAYS controls bundle age retention (default 30, 0 disables).
- AADK_OBSERVE_BUNDLE_MAX controls max bundle count (default 50, 0 disables).
- AADK_OBSERVE_TMP_RETENTION_HOURS controls tmp directory retention (default 24, 0 disables).

## Prioritized TODO checklist by service
- Persist bundle export paths in run summaries (or add a list bundles RPC) to support dashboards.
