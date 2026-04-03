# v1 System Design

## Implementation Note (Current Codebase)

This document is the target architecture. The current implementation is intentionally more consolidated in `src-tauri/src/main.rs` and is being iteratively extracted toward the module layout described below.

## 1) Architecture

### High-level

- **UI (React/TS)**: dashboard, game list, backup history, restore flow, settings
- **Tauri bridge**: command/event boundary between UI and Rust services
- **Core (Rust)**: detection, save resolution, backup/restore engine, process monitor, scheduler
- **Storage**:
  - SQLite for metadata and indexing
  - filesystem snapshots for backup payloads and captures

### Key rule

Restore is always a two-phase safety operation:

1. Create a pre-restore safety backup of current save files
2. Restore selected snapshot

Never directly overwrite without safety backup.

## 2) Rust Modules

Suggested Rust module layout under `src-tauri/src/`:

- `main.rs`: app bootstrap, command registration, service startup
- `commands/`
  - `games.rs`: list games, trigger scans, get game details
  - `backups.rs`: create/list/restore backups
  - `settings.rs`: read/update settings
  - `captures.rs`: trigger/list captures
- `core/`
  - `detectors/`
    - `steam.rs`: parse steam libraries + `appmanifest_*.acf`
    - `epic.rs`, `gog.rs`, `heroic.rs`, `lutris.rs`: launcher manifest readers
  - `process_monitor.rs`: polls running processes and emits game start/stop events
  - `save_locator.rs`: resolve save paths via Ludusavi + custom overrides
  - `backup_engine.rs`: snapshot creation, hashing, retention policy
  - `restore_engine.rs`: safety backup + transactional restore
  - `scheduler.rs`: periodic tasks and backup-after-exit automation
  - `capture_worker.rs`: FFmpeg invocations and artifact indexing
- `db/`
  - `mod.rs`: connection and migration wiring
  - `schema.sql`: migrations
  - `repositories/`: per-entity CRUD/query logic
- `models/`: game, install, save location, backup, restore event, capture, setting
- `errors.rs`: typed domain errors and command-safe error mapping

## 3) Tauri Command Surface (v1)

Commands exposed to UI:

- `scan_games() -> ScanResult`
- `list_games() -> Vec<GameSummary>`
- `get_game_details(game_id) -> GameDetails`
- `resolve_save_locations(game_id) -> Vec<ResolvedSaveLocation>`
- `backup_now(game_id) -> BackupResult`
- `list_backups(game_id) -> Vec<Backup>`
- `restore_backup(backup_id) -> RestoreResult`
- `get_restore_preview(backup_id) -> RestorePreview`
- `set_custom_save_path(game_id, path) -> SaveLocation`
- `get_settings() -> Settings`
- `update_settings(patch) -> Settings`
- `start_capture(game_id, mode) -> CaptureTask`
- `list_captures(game_id) -> Vec<Capture>`

Events emitted to UI:

- `scan-progress`
- `game-session-started`
- `game-session-ended`
- `backup-started`
- `backup-completed`
- `restore-started`
- `restore-completed`
- `capture-progress`

## 4) SQLite Schema (v1)

### `games`

- `id` TEXT PK (uuid)
- `canonical_name` TEXT NOT NULL
- `display_name` TEXT NOT NULL
- `store` TEXT NULL
- `metadata_source` TEXT NULL
- `metadata_last_sync_at` INTEGER NULL
- `created_at` INTEGER NOT NULL
- `updated_at` INTEGER NOT NULL

### `installs`

- `id` TEXT PK
- `game_id` TEXT NOT NULL FK -> games.id
- `store` TEXT NOT NULL
- `install_dir` TEXT NOT NULL
- `exe_path` TEXT NOT NULL
- `store_app_id` TEXT NULL
- `detected_at` INTEGER NOT NULL
- `last_seen_at` INTEGER NOT NULL

### `save_locations`

- `id` TEXT PK
- `game_id` TEXT NOT NULL FK -> games.id
- `source` TEXT NOT NULL (`ludusavi`, `custom`, `heuristic`)
- `path_pattern` TEXT NOT NULL
- `resolved_path` TEXT NULL
- `platform` TEXT NOT NULL
- `confidence` REAL NOT NULL DEFAULT 0.0
- `is_active` INTEGER NOT NULL DEFAULT 1
- `created_at` INTEGER NOT NULL
- `updated_at` INTEGER NOT NULL

### Candidate review workflow (v1.1)

- Every scanned candidate can be marked as `accepted` or `rejected`.
- Accepted candidates can store an active `save_location` path immediately.
- Rejected candidates are excluded from backup scheduling.
- Review decisions are persisted locally so app restarts keep state.

### `backups`

- `id` TEXT PK
- `game_id` TEXT NOT NULL FK -> games.id
- `snapshot_path` TEXT NOT NULL
- `snapshot_hash` TEXT NULL
- `snapshot_size_bytes` INTEGER NOT NULL
- `file_count` INTEGER NOT NULL
- `created_by` TEXT NOT NULL (`manual`, `after_exit`, `scheduled`, `pre_restore_safety`)
- `created_at` INTEGER NOT NULL

### `restore_points`

- `id` TEXT PK
- `backup_id` TEXT NOT NULL FK -> backups.id
- `game_id` TEXT NOT NULL FK -> games.id
- `pre_restore_backup_id` TEXT NULL FK -> backups.id
- `status` TEXT NOT NULL (`started`, `completed`, `failed`, `rolled_back`)
- `notes` TEXT NULL
- `created_at` INTEGER NOT NULL

### `captures`

- `id` TEXT PK
- `game_id` TEXT NOT NULL FK -> games.id
- `backup_id` TEXT NULL FK -> backups.id
- `type` TEXT NOT NULL (`gif`, `clip`)
- `file_path` TEXT NOT NULL
- `duration_sec` INTEGER NULL
- `size_bytes` INTEGER NULL
- `created_at` INTEGER NOT NULL

### `settings`

- `key` TEXT PK
- `value_json` TEXT NOT NULL
- `updated_at` INTEGER NOT NULL

### Useful indexes

- `idx_installs_game_id`
- `idx_installs_exe_path`
- `idx_save_locations_game_id`
- `idx_backups_game_id_created_at`
- `idx_restore_points_game_id_created_at`
- `idx_captures_game_id_created_at`

## 5) Data and Filesystem Layout

Recommended app data directory:

- `data/db.sqlite`
- `data/manifests/ludusavi-manifest.yaml`
- `backups/<game-id>/<backup-id>/...files`
- `captures/<game-id>/<capture-id>.gif|.mp4`
- `logs/app.log`

Backups should be immutable after creation. Metadata updates happen in SQLite only.

## 6) Detection Strategy

### Installed games

- Parse launcher-native local manifests (Steam first)
- Normalize paths and canonical names
- Track provenance (`store`, `store_app_id`, source manifest path)

### Running games

- Poll process list every N seconds (2-5)
- Match executable path to `installs.exe_path` (normalized)
- Emit start/end events with debounce to avoid flapping

### Save location resolution

- Query Ludusavi entries by canonical name + known aliases
- Resolve variables (home/profile/appdata/etc.)
- Validate path existence when possible
- Fall back to user custom save path

## 7) Safe Restore Algorithm

1. Resolve active save locations for target game
2. Validate selected backup snapshot integrity (if hash exists)
3. Create pre-restore safety backup from current live saves
4. Stage restore files to temporary location
5. Replace target save files atomically where possible
6. Record restore result and link safety backup in DB
7. On failure, attempt rollback from safety backup and mark status

## 8) MVP Non-Goals

- Cloud sync
- Multiplayer conflict resolution
- Deep mod-manager integration
- Full auto-detection for every launcher on day one

## 9) Quality Gates

- Unit tests for manifest parsing and path resolution
- Integration tests for backup/restore on temp directories
- Crash-safe writes and error telemetry in restore flow
- Basic migration tests for SQLite schema evolution
