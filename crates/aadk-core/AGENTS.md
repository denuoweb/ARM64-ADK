# JobService Agent Notes (aadk-core)

## Role and scope
JobService is the central event bus for long-running work. Other services create jobs, publish
progress/log events, and clients stream these events. This crate defines the job registry, event
history, and cancellation mechanism.

## Maintenance
Update this file whenever JobService behavior changes or when commits touching this crate are made.

## gRPC contract
- proto/aadk/v1/job.proto
- RPCs: StartJob, GetJob, CancelJob, StreamJobEvents, PublishJobEvent
- Core messages: Job, JobEvent, JobProgress, JobLogAppended, JobCompleted, JobFailed, ErrorDetail

## Current implementation details
- Implementation lives in crates/aadk-core/src/main.rs with a tonic server.
- JobStore is in-memory (HashMap of job_id -> JobRecordInner); it does not persist across restarts.
- Each job has:
  - broadcast::Sender<JobEvent> for live streaming
  - VecDeque<JobEvent> history (bounded)
  - watch::Sender<bool> for cancellation
- StreamJobEvents replays history (optional) and then forwards live events.
- publish_job_event updates job state on StateChanged/Completed/Failed payloads before broadcasting.
- start_job requires a non-empty job_type, rejects unknown types, and runs the demo runner only
  for demo.job (other services own their worker execution).

## Data flow and dependencies
- ToolchainService, BuildService, TargetService, and ObserveService are expected to create jobs
  and publish progress/log events to this service.
- The GTK UI and CLI stream job events from JobService (both demo and real jobs).

## Environment / config
- AADK_JOB_ADDR sets the bind address (default 127.0.0.1:50051).

## Prioritized TODO checklist by service
- P1: Persist job state/history across restarts. main.rs (line 35)
- P2: Add retention/cleanup policy for job history. main.rs (line 84)
- P2: Expand progress/metrics payloads per job type. main.rs (line 100)
