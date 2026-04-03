# CheckPoint App

Application package for the CheckPoint desktop client (React + Tauri + Rust).

## Stack

- React + TypeScript + Vite
- Tauri v2 (`@tauri-apps/api`, `@tauri-apps/cli`)
- Rust backend (`src-tauri`)
- SQLite metadata database

## Run Modes

### Frontend only (browser)

```bash
npm install
npm run dev
```

### Desktop app (recommended)

```bash
npm install
npm run tauri dev
```

## Current Core Features

- Launcher-first game detection (Steam/Epic/Battle.net/GOG/local)
- Add/remove tracked games
- Save path suggestion + folder picker confirmation
- Running status polling for tracked games
- Backup timeline with:
  - automatic process-exit backups
  - manual backup now
  - restore action
  - pre-restore safety backup

## Storage

The app writes runtime data to OS-local app data directories:

- metadata: SQLite and metadata files
- cache: icons and transient files
- backups: per-game snapshot folders
- logs: runtime diagnostics

## Repo Layout

- `src/`: React UI
- `src-tauri/src/main.rs`: Tauri commands and core backend logic
- `src-tauri/capabilities/`: Tauri capability permissions
