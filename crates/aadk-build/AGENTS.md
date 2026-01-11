# BuildService Agent Notes (aadk-build)

## Role and scope
BuildService runs Gradle builds, streams job progress/logs to JobService, and returns build
artifacts. It is designed to be thin orchestration around existing Gradle projects.

## gRPC contract
- proto/aadk/v1/build.proto
- RPCs: Build, ListArtifacts
- Shared messages: Artifact, BuildVariant, KeyValue, Job events (via JobService)

## Current implementation details
- Implementation lives in crates/aadk-build/src/main.rs with a tonic server.
- Build requests:
  - Validate project_id and resolve to a path using ProjectService or AADK_PROJECT_ROOT.
  - Spawn a Gradle process with tasks based on BuildVariant and clean_first.
  - Publish job state, progress, and logs to JobService.
- list_artifacts scans build outputs for APKs and returns Artifact entries.
- Artifact sha256 is currently left empty.

## Data flow and dependencies
- Requires JobService to be up for job creation and event streaming.
- Uses ProjectService to resolve project_id to a path when not provided as a direct path.
- AADK_PROJECT_ROOT can be used as a fallback root for project ids.

## Environment / config
- AADK_BUILD_ADDR sets the bind address (default 127.0.0.1:50054).
- AADK_JOB_ADDR sets the JobService address.
- AADK_PROJECT_ADDR sets the ProjectService address.
- AADK_PROJECT_ROOT provides a project root path for resolving ids.
- AADK_GRADLE_DAEMON and AADK_GRADLE_STACKTRACE control Gradle flags.

## Prioritized TODO checklist by service
- P0: Make project resolution authoritative (real ProjectService IDs) with robust error mapping. main.rs (line 242)
- P0: Populate artifact hashes instead of empty sha256. main.rs (line 516)
- P1: Persist build records/artifacts per job so list_artifacts is backed by stored results. main.rs (line 800)
- P1: Harden Gradle invocation (wrapper detection, env setup, error surfacing). main.rs (line 529)
- P2: Expand variant/module support and artifact filtering. main.rs (line 293)
