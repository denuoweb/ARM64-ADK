# ObserveService Agent Notes (aadk-observe)

## Role and scope
ObserveService is meant to track run history and produce evidence/support bundles for diagnostics.
In this scaffold it is a placeholder service with minimal responses.

## gRPC contract
- proto/aadk/v1/observe.proto
- RPCs: ListRuns, ExportSupportBundle, ExportEvidenceBundle

## Current implementation details
- Implementation lives in crates/aadk-observe/src/main.rs with a tonic server.
- list_runs returns an empty list with an empty page token.
- export_support_bundle and export_evidence_bundle return a synthetic job_id and a fixed /tmp
  output path, without generating any files or job events.

## Data flow and dependencies
- Eventually this service should create jobs in JobService and stream progress/log output while
  building bundles, then persist run metadata for the UI to query.
- Current UI/CLI support is limited to a placeholder "Evidence" action in the GTK UI.

## Environment / config
- AADK_OBSERVE_ADDR sets the bind address (default 127.0.0.1:50056).

## Prioritized TODO checklist by service
- P0: Implement real run storage + pagination for list_runs. main.rs (line 17)
- P0: Generate real support/evidence bundles (zip actual files). main.rs (line 24) main.rs (line 35)
- P0: Execute bundle work through JobService with progress/log streaming. main.rs (line 24)
- P1: Capture run metadata (project/target/timestamps) for filtering. main.rs (line 17)
- P2: Add retention/cleanup for bundles and temp dirs. main.rs (line 24)
