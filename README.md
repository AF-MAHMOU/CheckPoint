# CheckPoint

CheckPoint is a local-first desktop app for game save tracking, backup, and safe restore.

It is designed for players who want confidence that progress is never lost, with a UX that keeps all data on-device by default.

## Why CheckPoint

- Local-first by design: no cloud account required
- Cross-platform foundation: Windows first, Linux-ready architecture
- Safe restore workflow: pre-restore safety snapshot before overwrite
- Launcher-first scanning: Steam/Epic/Battle.net/GOG plus local/manual installs
- Clear health states: missing save-path and missing install-directory indicators

## Current v1 Capabilities

- Detect installed games from known launchers and local scan roots
- Add/remove games into tracked CheckPoint library
- Suggest and confirm save locations with browse support
- Monitor running status (`Running now` / `Not running`)
- Create backups:
  - manually (`Backup Now`)
  - automatically when a tracked game transitions `running -> not running`
- Show backup history per game with timestamp, trigger reason, and backup size
- Restore from backup with automatic pre-restore safety backup

## Tech Stack

- Frontend: React + TypeScript (Vite)
- Desktop shell: Tauri v2
- Backend/core logic: Rust
- Metadata/state storage: SQLite
- Process detection: `sysinfo`
- Local storage: OS local app data directories (`LOCALAPPDATA` / XDG local share)

## Data Storage Policy

CheckPoint stores app-managed data in OS-standard app-local folders, not Documents/Desktop/game folders.

High-level structure:

- `metadata/` (SQLite and metadata)
- `cache/` (icon cache and transient files)
- `backups/` (per-game backup snapshots)
- `logs/` (runtime and diagnostic logs)

## Project Structure

- `checkpoint-app/`: Tauri + React application
- `docs/`: private technical documentation and roadmap
- `assets/`: project assets and branding materials

## Quick Start (Development)

```bash
cd checkpoint-app
npm install
npm run tauri dev
```

Prerequisites:

- Node.js 20+
- Rust toolchain
- Tauri OS prerequisites

## Documentation

- Public/dev app guide: `checkpoint-app/README.md`
- System design and roadmap: `docs/v1-system-design.md`, `docs/v1-roadmap.md`
- Internal implementation notes: `docs/internal-technical.md`

## Status

Active v1 development. Core backup/restore flow is implemented and being expanded with deeper automation, retention policy, and richer restore timeline features.
