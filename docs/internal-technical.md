# Internal Technical Notes (Private)

This document tracks the implementation-level behavior of the current v1 codebase.

## 1) Runtime Architecture (Current)

- Frontend (`checkpoint-app/src/App.tsx`):
  - Source-grouped game discovery and review flow
  - CheckPoint tab with view modes, health states, running states, backup timeline
  - Polling loops for:
    - running process status
    - backup session cycle
- Backend (`checkpoint-app/src-tauri/src/main.rs`):
  - Tauri command surface in a single module
  - SQLite metadata persistence
  - Filesystem backup/restore operations
  - Process detection via `sysinfo`

## 2) Command Surface (Current)

Main commands exposed through Tauri:

- discovery/review:
  - `scan_games`
  - `scan_steam_games`
  - `scan_epic_games`
  - `scan_battlenet_games`
  - `scan_gog_games`
  - `review_game_candidate`
  - `list_reviewed_games`
- save path and icon:
  - `suggest_save_paths`
  - `set_save_location`
  - `get_game_icon`
- process status:
  - `get_running_game_statuses`
- backup/restore:
  - `run_backup_cycle`
  - `create_backup_now`
  - `list_backups`
  - `restore_backup`
- utility:
  - `open_in_file_explorer`
  - `health_check`

## 3) Storage Layout (Current)

Resolved from OS local app data directory (`app_local_data_dir`):

- `metadata/checkpoint.db`
- `cache/icons/`
- `backups/<game>/<store-app-id>/<timestamp>/...`
- `logs/` (reserved; currently created and available)

This avoids user-facing folders such as Documents/Desktop and follows desktop app conventions.

## 4) Database Schema (Current)

Core tables used by current implementation:

- `reviewed_games`
  - accepted/rejected tracking state
- `save_locations`
  - active save path for candidate
- `steam_metadata`
  - steam metadata cache
- `backups`
  - backup entries with timestamp, trigger reason, size, file count, and path

## 5) Session + Auto Backup Logic

`run_backup_cycle` executes a polling-cycle reconciliation:

1. Load accepted tracked games with active save paths.
2. Detect if each game is currently running.
3. Compare to in-memory previous cycle state:
   - transition `running -> not running` triggers backup creation.
4. Persist backup metadata and return created entries.

Trigger reason values currently used:

- `manual`
- `process_exit`
- `pre_restore_safety`

## 6) Restore Safety Model

On restore:

1. Resolve backup entry and tracked game.
2. Validate game save path exists in metadata.
3. Create `pre_restore_safety` backup of current save directory.
4. Replace destination save folder contents with backup snapshot.

This is a full-directory replace strategy in v1.

## 7) Known Constraints / Technical Debt

- Process matching can still false-positive on generic executable names.
- Backup/restore currently assumes save path points to a directory.
- No retention policy enforcement yet (size/count cleanup pending).
- No capture/screenshot timeline integration yet.
- No dedicated background worker service; polling is UI-driven command loop.

## 8) Next Technical Priorities

1. Retention and storage policy execution:
   - max backups per game
   - max global bytes
2. Session manager hardening:
   - stronger process identity
   - reduced false positives
3. Backup robustness:
   - optional delta/compression strategy
   - more granular conflict/error reporting
4. Restore UX:
   - restore preview and clearer rollback messaging
5. Media:
   - checkpoint-linked screenshot or clip metadata
