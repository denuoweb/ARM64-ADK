# Telemetry Agent Notes (aadk-telemetry)

## Role and scope
aadk-telemetry provides opt-in, local-first telemetry for usage events and crash reports.
It is shared by the UI, CLI, and service binaries.

## Maintenance
Update this file whenever telemetry schema or behavior changes.

## Event schema
- Each event is a single JSON line in `events.jsonl`.
- Fields:
  - event_type: string (e.g., app.start, ui.command.start, service.start)
  - at_unix_millis: int64
  - app: string (binary name)
  - version: string
  - session_id: string
  - install_id: optional string
  - properties: map<string,string>
- events are bounded (rotation at ~2MB) and drop on overload.

## Crash reports
- Stored in `~/.local/share/aadk/telemetry/<app>/crashes/crash-<timestamp>-<pid>.json`.
- Includes message, location (file:line), and a backtrace snapshot.

## Environment / config
- AADK_TELEMETRY=1 enables usage events.
- AADK_TELEMETRY_CRASH=1 enables crash reports.
- AADK_TELEMETRY_INSTALL_ID overrides the install id.
