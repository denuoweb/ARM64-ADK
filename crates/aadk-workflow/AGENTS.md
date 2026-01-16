# WorkflowService Agent Notes (aadk-workflow)

## Role and scope
WorkflowService orchestrates multi-step pipelines across Project, Toolchain, Build, Targets, and
Observe services. It owns the workflow.pipeline job type and emits progress/log events via JobService.

## Maintenance
Update this file whenever WorkflowService behavior changes or when commits touching this crate are made.

## gRPC contract
- proto/aadk/v1/workflow.proto
- RPCs: RunPipeline

## Current implementation details
- Implementation lives in crates/aadk-workflow/src/main.rs with a tonic server; shared bootstrap,
  address defaults, and time helpers come from `aadk-util`.
- RunPipeline creates (or reuses) a JobService job of type workflow.pipeline and returns run_id/job_id.
- If WorkflowPipelineOptions is omitted, the pipeline infers which steps to run from inputs.
- Steps include: project.create, project.open, toolchain.verify, build.run, targets.install,
  targets.launch, observe.support_bundle, observe.evidence_bundle.
- Progress metrics include pipeline_step, step_index/total_steps, run_id/correlation_id, and
  step-specific fields (project/toolchain/target identifiers, artifact path).
- The service waits on each step job by streaming JobService events and validates success.
- Run records are upserted in ObserveService on start, failure (best-effort), and success.
- After build steps, workflow upserts artifact outputs to ObserveService so run dashboards can list outputs.

## Data flow and dependencies
- Uses JobService for pipeline job creation and event publishing.
- Calls ProjectService/ToolchainService/BuildService/TargetService/ObserveService to execute steps.
- ObserveService UpsertRun is used for best-effort run tracking.

## Environment / config
- AADK_WORKFLOW_ADDR sets the bind address (default 127.0.0.1:50057).
- AADK_JOB_ADDR, AADK_TOOLCHAIN_ADDR, AADK_PROJECT_ADDR, AADK_BUILD_ADDR,
  AADK_TARGETS_ADDR, AADK_OBSERVE_ADDR configure upstream services.
- AADK_TELEMETRY and AADK_TELEMETRY_CRASH enable opt-in usage/crash reporting (local spool).

## Telemetry
- Emits service.start (service=workflow) when opt-in telemetry is enabled.

## Prioritized TODO checklist by service
- None (run outputs are recorded via ObserveService inventory).
