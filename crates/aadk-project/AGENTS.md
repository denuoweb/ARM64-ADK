# ProjectService Agent Notes (aadk-project)

## Role and scope
ProjectService is responsible for project templates, project creation/opening, and the recent
projects list. It should be the source of truth for project metadata that BuildService and UI/CLI
consume. It currently provides minimal placeholder responses and does not scaffold files on disk.

## gRPC contract
- proto/aadk/v1/project.proto
- RPCs: ListTemplates, CreateProject, OpenProject, ListRecentProjects, SetProjectConfig
- Shared messages: Id, Project, Template, PageInfo, Timestamp, KeyValue

## Current implementation details
- Implementation lives in crates/aadk-project/src/main.rs with a tonic server.
- State is an in-memory Vec<Project> guarded by Arc<Mutex<State>>; it resets on restart.
- list_templates returns a single hard-coded template:
  - name: "Compose Counter"
  - description: "Jetpack Compose + Kotlin counter that updates UI state"
  - defaults: minSdk=24, compileSdk=35
- create_project fabricates a project_id and job_id, stores the project in-memory, and does not
  create files on disk.
- open_project is explicitly stubbed: it uses the folder name from the path as project name and
  fabricates an ID.
- list_recent_projects returns the in-memory list with an empty page token.
- set_project_config returns ok=true without persisting or validating.

## Data flow and dependencies
- BuildService resolves project_id to a path by calling ProjectService list_recent_projects if
  AADK_PROJECT_ROOT does not match; any changes here affect build resolution logic.
- The GTK UI currently only calls list_templates (no create/open flows are wired yet).

## Environment / config
- AADK_PROJECT_ADDR sets the bind address (default 127.0.0.1:50053).

## Prioritized TODO checklist by service
- P0: Replace hard-coded templates with a real template registry + validation. main.rs (line 32) main.rs (line 46)
- P0: Implement create_project to scaffold files on disk, handle collisions, and return real job tracking. main.rs (line 55)
- P0: Implement open_project to read actual metadata/config instead of fabricating IDs. main.rs (line 82)
- P0: Persist projects/recent list to durable storage and support paging tokens. main.rs (line 110)
- P1: Persist set_project_config (toolchain/target defaults) with input validation. main.rs (line 121)
- P2: Add template defaults resolution (minSdk/compileSdk) with schema errors. main.rs (line 32)
