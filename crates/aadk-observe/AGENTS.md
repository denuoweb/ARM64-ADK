# ObserveService Agent Notes (aadk-observe)

## Role and scope
ObserveService is meant to track run history and produce evidence/support bundles for diagnostics.
It persists run history and exports bundles via JobService-driven jobs.

## Maintenance
Update this file whenever observe behavior changes or when commits touching this crate are made.

## gRPC contract
- proto/aadk/v1/observe.proto
- RPCs: ListRuns, ListRunOutputs, ExportSupportBundle, ExportEvidenceBundle, UpsertRun, UpsertRunOutputs

## Current implementation details
- Implementation lives in crates/aadk-observe/src/main.rs with a tonic server; shared bootstrap
  and path/time helpers come from `aadk-util`.
- Run history and output inventory are persisted in ~/.local/share/aadk/state/observe.json and paged in list_runs/list_run_outputs.
- ListRuns accepts RunFilter (run_id, correlation_id, project/target/toolchain ids, result).
- UpsertRun merges partial updates (job ids, summary entries, timestamps, result) for best-effort run tracking.
- UpsertRunOutputs stores bundle/artifact outputs and updates the run output_summary pointer (counts, last updated, last bundle id).
- ListRunOutputs supports kind/type/path/label filters and returns the output summary for the run.
- export_support_bundle creates a JobService job, writes a zip bundle under
  ~/.local/share/aadk/bundles, and streams progress/log events.
- export_evidence_bundle does the same for a single run id.
- Bundle export progress metrics include run_id/output_path plus include flags and item counts.
- Bundle exports are recorded as RunOutput entries so dashboards can list bundle inventory.
- ExportSupportBundle/ExportEvidenceBundle accept optional job_id to attach to existing jobs plus
  correlation_id and run_id to group multi-service workflows.
- Runs capture project/target/toolchain ids and correlation_id when provided for filtering.
- Bundle retention prunes old zip files and tmp-* directories by age/count.
- Bundle contents include a manifest, config snapshots (env + state files), optional toolchain
  state, optional recent run snapshots, and job log capture from JobService history
  (`~/.local/share/aadk/state/jobs.json`).

## Data flow and dependencies
- Uses JobService for bundle jobs and event streaming.
- Build/Workflow clients can upsert run outputs (artifacts) for run dashboards.
- UI and CLI can list runs, list outputs, and export support/evidence bundles with job streaming.

## Environment / config
- AADK_OBSERVE_ADDR sets the bind address (default 127.0.0.1:50056).
- AADK_OBSERVE_BUNDLE_RETENTION_DAYS controls bundle age retention (default 30, 0 disables).
- AADK_OBSERVE_BUNDLE_MAX controls max bundle count (default 50, 0 disables).
- AADK_OBSERVE_TMP_RETENTION_HOURS controls tmp directory retention (default 24, 0 disables).
- AADK_TELEMETRY and AADK_TELEMETRY_CRASH enable opt-in usage/crash reporting (local spool).

## Telemetry
- Emits service.start (service=observe) when opt-in telemetry is enabled.

## Prioritized TODO checklist by service
- None (run output inventory is stored and exposed via ListRunOutputs).
