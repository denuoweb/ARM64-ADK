# Proto Contracts Agent Notes (proto/aadk/v1)

## Role and scope
This directory defines the gRPC/Protobuf contracts for all services. These .proto files are the
source of truth for the Rust gRPC clients/servers and any future non-Rust clients.

## Maintenance
Update this file whenever proto contracts change or when commits touching this directory are made.

## Files and ownership
- common.proto: shared primitives (Id, RunId, Timestamp, KeyValue, PageInfo)
- errors.proto: ErrorCode and ErrorDetail
- job.proto: JobService and job event types
- toolchain.proto: ToolchainService (includes ListToolchainSets)
- project.proto: ProjectService
- build.proto: BuildService
- target.proto: TargetService (includes ResolveCuttlefishBuild and install overrides)
- observe.proto: ObserveService
- workflow.proto: WorkflowService (pipeline orchestration)

## Recent changes
- Added ObserveService RunOutput inventory (RunOutputSummary/RunOutput) plus ListRunOutputs and UpsertRunOutputs; RunRecord now carries output_summary for counts/last update.
- Added optional job_id fields to long-running requests (build/toolchain/target/observe) so services
  can accept pre-created jobs from JobService.
- Added JobService list APIs (ListJobs, ListJobHistory) with pagination and filters plus JobEventKind.
- Added GetProject RPC to project.proto for authoritative project resolution.
- Added project/target/toolchain_set metadata to observe ExportSupportBundleRequest.
- Extended build.proto with module/variant/task selection fields, ArtifactType, and ArtifactFilter.
- Extended toolchain.proto with update/uninstall/cache cleanup RPCs and new toolchain error codes.
- Added correlation_id to StartJobRequest, JobFilter, and long-running requests to group workflow jobs.
- Added job_id + correlation_id to CreateProjectRequest for project creation workflows.
- Added RunId as a shared identifier and threaded it through job + service requests.
- Added StreamRunEvents RPC for aggregated run-level job event streaming.
- Added ObserveService RunFilter/UpsertRun and workflow.proto for pipeline orchestration.

## Code generation
- Build script: crates/aadk-proto/build.rs
- The build script uses a fixed list of protos and `tonic_build` to generate Rust types.
- If you add a new .proto file, you must:
  - add it to the `protos` list in crates/aadk-proto/build.rs
  - import it from other protos as needed
  - run `cargo build -p aadk-proto` (or a full workspace build)
- Generated Rust modules live under `aadk_proto::aadk::v1`.

## Compatibility and evolution rules
- Safe, additive changes:
  - add new fields with new field numbers
  - add new RPCs to an existing service
  - add new enum values (never reuse numbers)
- Avoid breaking changes:
  - renaming packages/services or changing RPC signatures
  - changing field numbers or types
  - removing fields or enum values without reserving them
- When deprecating fields:
  - mark numbers and names as `reserved`
  - keep deprecated fields until a major version bump
- Always keep enum zero values as `Unspecified`/`Unknown` for forward compatibility.

## Versioning strategy
- Current API version is `aadk.v1`.
- Breaking changes should go into a new package (e.g., `aadk.v2`) while keeping v1 live until
  client migration is complete.

## Review checklist for proto changes
- Update the relevant .proto file(s) with comments and new field numbers.
- Reserve removed field numbers and names.
- Update crates/aadk-proto/build.rs if adding files.
- Regenerate and build to ensure compile success.
- Update service implementations and clients to handle new fields.
- Add or adjust tests/fixtures as needed.

## Prioritized TODO checklist by service
- None (workflow/run dashboards are covered by existing protos).
