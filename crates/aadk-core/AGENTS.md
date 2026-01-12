# JobService Agent Notes (aadk-core)

## Role and scope
JobService is the central event bus for long-running work. Other services create jobs, publish
progress/log events, and clients stream these events. This crate defines the job registry, event
history, and cancellation mechanism.

## Maintenance
Update this file whenever JobService behavior changes or when commits touching this crate are made.

## gRPC contract
- proto/aadk/v1/job.proto
- RPCs: StartJob, GetJob, CancelJob, StreamJobEvents, PublishJobEvent, ListJobs, ListJobHistory
- Core messages: Job, JobEvent, JobProgress, JobLogAppended, JobCompleted, JobFailed, ErrorDetail

## Current implementation details
- Implementation lives in crates/aadk-core/src/main.rs with a tonic server.
- JobStore loads and persists job history/state to `~/.local/share/aadk/state/jobs.json`.
- Each job has:
  - broadcast::Sender<JobEvent> for live streaming
  - VecDeque<JobEvent> history (bounded, persisted)
  - watch::Sender<bool> for cancellation
- StreamJobEvents replays history (optional) and then forwards live events.
- ListJobs supports pagination with filters by job_type, state, and created/finished timestamps.
- ListJobHistory returns paginated job events with optional kind/time filters.
- publish_job_event updates job state on StateChanged/Completed/Failed payloads before broadcasting.
- start_job requires a non-empty job_type, rejects unknown types, and runs the demo runner only
  for demo.job (other services own their worker execution).
- demo.job progress emits step/total_steps metrics for UI/CLI validation.
- Retention trims completed jobs by age/count; active jobs are preserved.

## Data flow and dependencies
- ToolchainService, BuildService, TargetService, and ObserveService are expected to create jobs
  and publish progress/log events to this service.
- The GTK UI and CLI stream job events from JobService (both demo and real jobs).

## Environment / config
- AADK_JOB_ADDR sets the bind address (default 127.0.0.1:50051).
- AADK_JOB_HISTORY_RETENTION_DAYS controls age-based cleanup (0 disables).
- AADK_JOB_HISTORY_MAX controls max completed jobs to keep (0 disables).

## Prioritized TODO checklist by service
