# ProjectService Agent Notes (aadk-project)

## Role and scope
ProjectService is responsible for project templates, project creation/opening, and the recent
projects list. It is the source of truth for project metadata that BuildService and UI/CLI
consume.

## Maintenance
Update this file whenever ProjectService behavior changes or when commits touching this crate are made.

## gRPC contract
- proto/aadk/v1/project.proto
- RPCs: ListTemplates, CreateProject, OpenProject, ListRecentProjects, GetProject, SetProjectConfig, ReloadState
- Shared messages: Id, Project, Template, PageInfo, Timestamp, KeyValue

## Current implementation details
- Implementation lives in crates/aadk-project/src/main.rs with a tonic server; shared bootstrap
  and path/time helpers come from `aadk-util`.
- State is persisted in ~/.local/share/aadk/state/projects.json and cached in-memory.
- list_templates loads a JSON registry from AADK_PROJECT_TEMPLATES (or templates/registry.json),
  skipping invalid entries and validating resolved defaults.
- Template defaults (minSdk/compileSdk) are resolved from registry defaults, request params, and
  Gradle files; schema errors are returned when required defaults are missing/invalid.
- templates/registry.json currently includes the Sample Console template (tmpl-sample-console)
  and the Empty Activity template (tmpl-empty-activity) pointing at SampleConsole/ and
  EmptyActivity/ for bundled Android starters. Empty Activity includes the Material Components
  dependency to provide the Theme.Material3.* XML styles used by its themes, plus a default
  ic_launcher_foreground drawable so sample code referencing it compiles.
- create_project scaffolds files from the template directory, streams job progress/logs to
  JobService, and persists metadata in .aadk/project.json plus the recent list; requests can
  attach to an existing job_id and supply correlation_id/run_id for grouped workflows.
- create_project attempts to write local.properties with sdk.dir from ANDROID_SDK_ROOT/ANDROID_HOME
  or the installed AADK SDK toolchain state, logging when it skips or overwrites nothing.
- create_project progress metrics include project/template metadata plus copied/total files and
  resolved minSdk/compileSdk values.
- open_project reads .aadk/project.json when present, otherwise generates metadata and persists it.
- list_recent_projects pages through the persisted recent list with page tokens.
- get_project resolves a project by id for authoritative build resolution.
- set_project_config updates metadata and persists recent state; missing fields leave existing
  values unchanged.
- ReloadState reloads recent project metadata from disk and replaces the in-memory cache.
- Unused `tracing::info` imports were removed to keep builds warning-free.
- Project sources are kept rustfmt-formatted to align with workspace style.

## Data flow and dependencies
- BuildService resolves project_id to a path by calling ProjectService GetProject.
- The GTK UI and CLI call list_templates/create/open/list_recent and use set_project_config.

## Environment / config
- AADK_PROJECT_ADDR sets the bind address (default 127.0.0.1:50053).
- AADK_TELEMETRY and AADK_TELEMETRY_CRASH enable opt-in usage/crash reporting (local spool).

## Telemetry
- Emits service.start (service=project) when opt-in telemetry is enabled.

## Prioritized TODO checklist by service
- None (workflow UI consumes existing project RPCs).
