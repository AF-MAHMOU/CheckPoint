# v1 Roadmap

## Current Status Snapshot

Implemented now:

- Launcher-first detection and local/manual scan fallback
- Candidate review flow (add/remove tracked game)
- Save path suggestion and manual path override
- Running-state detection (`sysinfo` polling)
- Backup engine v1:
  - manual backup now
  - auto backup on process-exit transition
  - restore from timestamped backup
  - mandatory pre-restore safety backup
- Backup timeline UI in CheckPoint tab

## Phase 0: Project bootstrap

- Scaffold Tauri + React + TypeScript app
- Add SQLite access layer and migrations
- Define command/error contracts between UI and Rust

Deliverable: app launches, DB initializes, health command works.

## Phase 1: Detection foundation

- Implement Steam detector (`libraryfolders` + `appmanifest_*.acf`)
- Persist detected games/installs
- Add manual rescan and basic game list UI

Deliverable: detected Steam games visible in UI with install paths.

## Phase 2: Save location resolution

- Integrate Ludusavi manifest ingestion/parsing
- Implement resolver (manifest + env expansion + aliases)
- Add custom save-path override UI and persistence
- Add candidate review queue (accept game / reject non-game)
- Store approved games and active save-location path in local SQLite

Deliverable: each game shows one or more candidate save paths.

## Phase 3: Backup/restore engine

- [x] Implement manual backup creation
- [x] Implement restore with mandatory pre-restore safety backup
- [x] Add backup history + restore action in UI

Deliverable: end-to-end safe restore works on test fixtures.

## Phase 4: Process monitor automation

- [x] Implement process polling and exe-to-game matching
- [x] Add "backup after game exit" automation (polling-based session transition)
- [ ] Add scheduler toggles and global settings

Deliverable: automatic backup triggers after gameplay sessions.

## Phase 5: Metadata and capture polish

- [ ] Optional metadata enrichment expansion
- [ ] FFmpeg worker for GIF/short clip creation tied to restore points
- [ ] Retention policy and cleanup worker

Deliverable: polished timeline with snapshots and media.

## Next Milestones (Practical)

1. Retention controls:
   - max backups per game
   - max total storage cap
   - automatic cleanup policy
2. Session manager hardening:
   - reduce false positives
   - stronger process identity handling
3. Restore UX polish:
   - restore confirmation preview
   - error recovery guidance
4. Backup observability:
   - backup logs/events in UI
   - richer backup metadata

## Suggested Milestone Rules

- Keep every phase releasable
- Add tests before adding another launcher
- Validate restore safety on real-world sample games
- Track every destructive operation in restore logs
