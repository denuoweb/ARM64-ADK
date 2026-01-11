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
- start_job defaults job_type to "demo.job" and runs a scripted demo runner; unknown job types are
  currently left queued with a warning.

## Data flow and dependencies
- ToolchainService, BuildService, TargetService, and ObserveService are expected to create jobs
  and publish progress/log events to this service.
- The GTK UI and CLI stream job events from JobService (both demo and real jobs).

## Environment / config
- AADK_JOB_ADDR sets the bind address (default 127.0.0.1:50051).

## Prioritized TODO checklist by service
- P0: Replace demo-only job flow with real job dispatch (build/toolchain/targets/observe). main.rs (line 193)
- P0: Propagate cancellation to actual workers, not just demo jobs. main.rs (line 118) main.rs (line 257)
- P1: Persist job state/history across restarts. main.rs (line 35)
- P1: Return explicit errors for unknown job types instead of leaving them queued. main.rs (line 236)
- P2: Add retention/cleanup policy for job history. main.rs (line 84)
- P2: Expand progress/metrics payloads per job type. main.rs (line 100)
