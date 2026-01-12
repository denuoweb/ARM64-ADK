# BuildService Agent Notes (aadk-build)

## Role and scope
BuildService runs Gradle builds, streams job progress/logs to JobService, and returns build
artifacts. It is designed to be thin orchestration around existing Gradle projects.

## Maintenance
Update this file whenever BuildService behavior changes or when commits touching this crate are made.

## gRPC contract
- proto/aadk/v1/build.proto
- RPCs: Build, ListArtifacts
- Shared messages: Artifact, BuildVariant, KeyValue, Job events (via JobService)

## Current implementation details
- Implementation lives in crates/aadk-build/src/main.rs with a tonic server.
- Build requests:
  - Validate project_id and resolve to a path using ProjectService GetProject unless the value already looks like a path.
  - Spawn a Gradle process with wrapper checks, GRADLE_USER_HOME defaults, and tasks based on BuildVariant/clean_first.
  - Publish job state, progress, and logs to JobService.
- BuildRequest can include job_id to attach work to an existing JobService job.
- Persist build/job artifact records in ~/.local/share/aadk/state/builds.json.
- list_artifacts returns stored artifacts when available and falls back to scanning outputs.
- Artifact sha256 is populated for stored artifacts.

## Data flow and dependencies
- Requires JobService to be up for job creation and event streaming.
- Uses ProjectService GetProject to resolve project_id to a path when not already a path.

## Environment / config
- AADK_BUILD_ADDR sets the bind address (default 127.0.0.1:50054).
- AADK_JOB_ADDR sets the JobService address.
- AADK_PROJECT_ADDR sets the ProjectService address.
- AADK_GRADLE_DAEMON and AADK_GRADLE_STACKTRACE control Gradle flags.
- AADK_GRADLE_REQUIRE_WRAPPER forces the Gradle wrapper to be present.
- AADK_GRADLE_USER_HOME overrides GRADLE_USER_HOME for builds.

## Prioritized TODO checklist by service
- P2: Expand variant/module support and artifact filtering. main.rs (line 293)
