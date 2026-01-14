# ProjectService Agent Notes (aadk-project)

## Role and scope
ProjectService is responsible for project templates, project creation/opening, and the recent
projects list. It is the source of truth for project metadata that BuildService and UI/CLI
consume.

## Maintenance
Update this file whenever ProjectService behavior changes or when commits touching this crate are made.

## gRPC contract
- proto/aadk/v1/project.proto
- RPCs: ListTemplates, CreateProject, OpenProject, ListRecentProjects, GetProject, SetProjectConfig
- Shared messages: Id, Project, Template, PageInfo, Timestamp, KeyValue

## Current implementation details
- Implementation lives in crates/aadk-project/src/main.rs with a tonic server.
- State is persisted in ~/.local/share/aadk/state/projects.json and cached in-memory.
- list_templates loads a JSON registry from AADK_PROJECT_TEMPLATES (or templates/registry.json),
  skipping invalid entries and validating resolved defaults.
- Template defaults (minSdk/compileSdk) are resolved from registry defaults, request params, and
  Gradle files; schema errors are returned when required defaults are missing/invalid.
- templates/registry.json currently includes the Sample Console template (tmpl-sample-console)
  pointing at SampleConsole/ for the bundled Android sample.
- create_project scaffolds files from the template directory, streams job progress/logs to
  JobService, and persists metadata in .aadk/project.json plus the recent list; requests can
  attach to an existing job_id and supply correlation_id/run_id for grouped workflows.
- create_project progress metrics include project/template metadata plus copied/total files and
  resolved minSdk/compileSdk values.
- open_project reads .aadk/project.json when present, otherwise generates metadata and persists it.
- list_recent_projects pages through the persisted recent list with page tokens.
- get_project resolves a project by id for authoritative build resolution.
- set_project_config updates metadata and persists recent state; missing fields leave existing
  values unchanged.

## Data flow and dependencies
- BuildService resolves project_id to a path by calling ProjectService GetProject.
- The GTK UI and CLI call list_templates/create/open/list_recent and use set_project_config.

## Environment / config
- AADK_PROJECT_ADDR sets the bind address (default 127.0.0.1:50053).

## Prioritized TODO checklist by service
