# JobService Agent Notes (aadk-core)

## Role and scope
JobService is the central event bus for long-running work. Other services create jobs, publish
progress/log events, and clients stream these events. This crate defines the job registry, event
history, and cancellation mechanism.

## Maintenance
Update this file whenever JobService behavior changes or when commits touching this crate are made.

## gRPC contract
- proto/aadk/v1/job.proto
- RPCs: StartJob, GetJob, CancelJob, StreamJobEvents, StreamRunEvents, PublishJobEvent, ListJobs, ListJobHistory
- Core messages: Job, JobEvent, JobProgress, JobLogAppended, JobCompleted, JobFailed, ErrorDetail, RunId

## Current implementation details
- Implementation lives in crates/aadk-core/src/main.rs with a tonic server.
- JobStore loads and persists job history/state to `~/.local/share/aadk/state/jobs.json`.
- Each job has:
  - broadcast::Sender<JobEvent> for live streaming
  - VecDeque<JobEvent> history (bounded, persisted)
  - watch::Sender<bool> for cancellation
- StreamJobEvents replays history (optional) and then forwards live events.
- StreamRunEvents aggregates jobs by run_id (or correlation_id), with bounded buffering for best-effort
  timestamp ordering and periodic discovery for late-arriving jobs.
- ListJobs supports pagination with filters by job_type, state, run_id, and created/finished timestamps.
- ListJobHistory returns paginated job events with optional kind/time filters.
- publish_job_event updates job state on StateChanged/Completed/Failed payloads before broadcasting.
- start_job requires a non-empty job_type, rejects unknown types, and runs the demo runner only
  for demo.job (other services own their worker execution). workflow.pipeline is reserved for
  orchestrated multi-step flows.
- StartJob accepts optional correlation_id and run_id; ListJobs can filter by either to group related jobs.
- demo.job progress emits step/total_steps metrics for UI/CLI validation.
- Retention trims completed jobs by age/count; active jobs are preserved.

## Data flow and dependencies
- ToolchainService, BuildService, TargetService, and ObserveService are expected to create jobs
  and publish progress/log events to this service.
- The GTK UI and CLI stream job events from JobService (both demo and real jobs).
- CLI can also stream run-level events via StreamRunEvents for multi-job pipelines.

## Environment / config
- AADK_JOB_ADDR sets the bind address (default 127.0.0.1:50051).
- AADK_JOB_HISTORY_RETENTION_DAYS controls age-based cleanup (0 disables).
- AADK_JOB_HISTORY_MAX controls max completed jobs to keep (0 disables).
- AADK_RUN_STREAM_BUFFER_MAX controls run stream buffer size (default 512).
- AADK_RUN_STREAM_MAX_DELAY_MS controls ordering delay window (default 1500ms).
- AADK_RUN_STREAM_DISCOVERY_MS controls late job discovery polling (default 750ms).
- AADK_RUN_STREAM_FLUSH_MS controls buffer flush cadence (default 200ms).

## Prioritized TODO checklist by service
