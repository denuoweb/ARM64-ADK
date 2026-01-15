# BuildService Agent Notes (aadk-build)

## Role and scope
BuildService runs Gradle builds, streams job progress/logs to JobService, and returns build
artifacts. It is designed to be thin orchestration around existing Gradle projects.

## Maintenance
Update this file whenever BuildService behavior changes or when commits touching this crate are made.

## gRPC contract
- proto/aadk/v1/build.proto
- RPCs: Build, ListArtifacts
- Shared messages: Artifact, ArtifactType, ArtifactFilter, BuildVariant, KeyValue, Job events (via JobService)

## Current implementation details
- Implementation lives in crates/aadk-build/src/main.rs with a tonic server.
- Build requests:
  - Validate project_id and resolve to a path using ProjectService GetProject unless the value already looks like a path.
  - Reject empty/whitespace project_id values before starting the job.
  - Accept module/variant_name/tasks overrides, validate basic formatting, and prefix module tasks as needed.
  - Load a Gradle model snapshot (init script) to validate module/variant selection and surface
    compileSdk/minSdk/buildTypes/flavors metadata.
  - Spawn a Gradle process with wrapper checks, GRADLE_USER_HOME defaults, and computed task lists.
  - Publish job state, progress, and logs to JobService.
- Progress metrics include project/module/variant/tasks/gradle args plus minSdk/compileSdk and
  buildTypes/flavors when available; finalizing reports artifact_count and artifact_types.
- BuildRequest can include job_id to attach work to an existing JobService job plus correlation_id
  and run_id to group multi-service workflows.
- Persist build/job artifact records in ~/.local/share/aadk/state/builds.json.
- When run_id is provided, successful builds upsert artifact outputs to ObserveService for run output inventory.
- list_artifacts returns stored artifacts when available, applies ArtifactFilter, and falls back to scanning outputs.
- Artifact discovery scans build outputs for APK/AAB/AAR/mapping/test results, parses output metadata
  when present, and tags metadata (module/variant/build_type/flavors/abi/density/task/type).
- Job completion outputs include module/variant/tasks plus artifact path/type entries.
- Artifact sha256 is populated for stored artifacts.

## Data flow and dependencies
- Requires JobService to be up for job creation and event streaming.
- Uses ProjectService GetProject to resolve project_id to a path when not already a path.
- Uses ObserveService UpsertRunOutputs to attach artifact outputs when run_id is set.

## Environment / config
- AADK_BUILD_ADDR sets the bind address (default 127.0.0.1:50054).
- AADK_JOB_ADDR sets the JobService address.
- AADK_PROJECT_ADDR sets the ProjectService address.
- AADK_OBSERVE_ADDR sets the ObserveService address for run output upserts.
- AADK_GRADLE_DAEMON and AADK_GRADLE_STACKTRACE control Gradle flags.
- AADK_GRADLE_REQUIRE_WRAPPER forces the Gradle wrapper to be present.
- AADK_GRADLE_USER_HOME overrides GRADLE_USER_HOME for builds.

## Implementation notes
- Gradle arg expansion now borrows BuildRequest args so the request stays available for progress metrics.
- Gradle model init script now runs after `projectsEvaluated` to avoid root project access errors.

## Prioritized TODO checklist by service
- None (workflow UI consumes existing build RPCs).
