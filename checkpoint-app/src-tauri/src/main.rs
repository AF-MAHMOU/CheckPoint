use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use reqwest::blocking::Client;
use rusqlite::{params, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use sysinfo::System;
use tauri::Manager;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GameCandidate {
    app_id: String,
    detected_title: String,
    store: String,
    source: String,
    trusted_launcher_match: bool,
    library_path: String,
    install_dir: String,
    manifest_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GameReviewRequest {
    candidate: GameCandidate,
    accepted: bool,
    save_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewedGame {
    candidate_app_id: String,
    detected_title: String,
    store: String,
    source: String,
    install_dir: String,
    install_dir_exists: bool,
    status: String,
    save_path: Option<String>,
    reviewed_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MatchResult {
    candidate_app_id: String,
    candidate_title: String,
    matched_title: Option<String>,
    confidence: f64,
    decision: String,
    provider: String,
    reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanGamesResponse {
    sources: Vec<ScanSourceGroup>,
    total_candidates: usize,
    total_auto_accepted: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanSourceGroup {
    source_key: String,
    source_label: String,
    candidates: Vec<GameCandidate>,
    matches: Vec<MatchResult>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SavePathSuggestion {
    path: String,
    source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunningGameStatus {
    candidate_app_id: String,
    running: bool,
    process_name: Option<String>,
    process_path: Option<String>,
}

#[derive(Debug, Clone)]
struct AppStorageDirs {
    metadata: PathBuf,
    cache: PathBuf,
    backups: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupEntry {
    id: i64,
    candidate_app_id: String,
    created_at: i64,
    backup_path: String,
    trigger_reason: String,
    file_count: i64,
    total_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupCycleResponse {
    created: Vec<BackupEntry>,
}

#[derive(Debug, Clone)]
struct SessionState {
    was_running: bool,
}

static SESSION_STATE: OnceLock<Mutex<HashMap<String, SessionState>>> = OnceLock::new();

fn session_state() -> &'static Mutex<HashMap<String, SessionState>> {
    SESSION_STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone, Deserialize)]
struct ApiGameHit {
    id: i64,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SteamStoreSearchItem {
    id: i64,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SteamStoreSearchResponse {
    items: Option<Vec<SteamStoreSearchItem>>,
}

#[derive(Debug, Clone)]
struct SteamMetadata {
    name: String,
    short_description: String,
    genres: Vec<String>,
}

#[tauri::command]
fn health_check() -> String {
    "Rust backend is connected".to_string()
}

#[tauri::command]
fn review_game_candidate(
    app: tauri::AppHandle,
    review: GameReviewRequest,
) -> Result<ReviewedGame, String> {
    let mut conn = open_db(&app)?;
    ensure_schema(&conn)?;

    let status = if review.accepted { "accepted" } else { "rejected" };
    let reviewed_at = unix_ts();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("Failed to start review transaction: {e}"))?;

    tx.execute(
        "INSERT INTO reviewed_games (
            candidate_app_id, detected_title, store, source, install_dir, library_path,
            manifest_path, trusted_launcher_match, status, reviewed_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        ON CONFLICT(candidate_app_id) DO UPDATE SET
            detected_title=excluded.detected_title,
            store=excluded.store,
            source=excluded.source,
            install_dir=excluded.install_dir,
            library_path=excluded.library_path,
            manifest_path=excluded.manifest_path,
            trusted_launcher_match=excluded.trusted_launcher_match,
            status=excluded.status,
            reviewed_at=excluded.reviewed_at",
        params![
            review.candidate.app_id,
            review.candidate.detected_title,
            review.candidate.store,
            review.candidate.source,
            review.candidate.install_dir,
            review.candidate.library_path,
            review.candidate.manifest_path,
            if review.candidate.trusted_launcher_match {
                1
            } else {
                0
            },
            status,
            reviewed_at
        ],
    )
    .map_err(|e| format!("Failed to save review: {e}"))?;

    // Rejected candidates should not keep active backup targets.
    if !review.accepted {
        tx.execute(
            "UPDATE save_locations SET is_active=0 WHERE candidate_app_id=?1",
            params![review.candidate.app_id],
        )
        .map_err(|e| format!("Failed to deactivate save paths for rejected candidate: {e}"))?;
    } else if let Some(path) = review.save_path.as_deref() {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            let validated = validate_save_path(trimmed)?;
            tx.execute(
                "UPDATE save_locations SET is_active=0 WHERE candidate_app_id=?1",
                params![review.candidate.app_id],
            )
            .map_err(|e| format!("Failed to update old save paths: {e}"))?;

            tx.execute(
                "INSERT INTO save_locations (candidate_app_id, path, kind, is_active, created_at)
                VALUES (?1, ?2, 'primary', 1, ?3)",
                params![review.candidate.app_id, validated, reviewed_at],
            )
            .map_err(|e| format!("Failed to save save-location path: {e}"))?;
        }
    }

    tx.commit()
        .map_err(|e| format!("Failed to commit review transaction: {e}"))?;

    load_reviewed_by_app_id(&conn, &review.candidate.app_id)
}

#[tauri::command]
fn list_reviewed_games(app: tauri::AppHandle) -> Result<Vec<ReviewedGame>, String> {
    let conn = open_db(&app)?;
    ensure_schema(&conn)?;
    load_reviewed_games(&conn)
}

#[tauri::command]
fn get_running_game_statuses(app: tauri::AppHandle) -> Result<Vec<RunningGameStatus>, String> {
    let conn = open_db(&app)?;
    ensure_schema(&conn)?;
    let reviewed = load_reviewed_games(&conn)?;
    let mut system = System::new_all();
    system.refresh_all();

    let statuses = reviewed
        .into_iter()
        .filter(|game| game.status == "accepted")
        .map(|game| {
            let (running, process_name, process_path) = detect_running_process_for_game(&game, &system);
            RunningGameStatus {
                candidate_app_id: game.candidate_app_id,
                running,
                process_name,
                process_path,
            }
        })
        .collect();

    Ok(statuses)
}

#[tauri::command]
fn run_backup_cycle(app: tauri::AppHandle) -> Result<BackupCycleResponse, String> {
    let conn = open_db(&app)?;
    ensure_schema(&conn)?;
    let reviewed = load_reviewed_games(&conn)?;
    let mut system = System::new_all();
    system.refresh_all();

    let mut created = Vec::new();
    let mut state = session_state()
        .lock()
        .map_err(|_| "Failed to lock session state".to_string())?;

    for game in reviewed
        .into_iter()
        .filter(|item| item.status == "accepted" && item.save_path.as_deref().unwrap_or("").trim().len() > 0)
    {
        let running_now = detect_running_process_for_game(&game, &system).0;
        let key = game.candidate_app_id.clone();
        let previous = state.get(&key).map(|s| s.was_running).unwrap_or(false);

        // Game transitioned from running -> not running, create checkpoint backup.
        if previous && !running_now {
            if let Some(save_path) = game.save_path.as_deref() {
                if let Some(entry) = create_backup_for_game(&conn, &app, &game, save_path, "process_exit")? {
                    created.push(entry);
                }
            }
        }

        state.insert(
            key,
            SessionState {
                was_running: running_now,
            },
        );
    }

    Ok(BackupCycleResponse { created })
}

#[tauri::command]
fn create_backup_now(app: tauri::AppHandle, candidate_app_id: String) -> Result<BackupEntry, String> {
    let conn = open_db(&app)?;
    ensure_schema(&conn)?;
    let game = load_reviewed_by_app_id(&conn, &candidate_app_id)?;
    let Some(save_path) = game.save_path.as_deref() else {
        return Err("Cannot create backup: save path is not set.".to_string());
    };
    let Some(entry) = create_backup_for_game(&conn, &app, &game, save_path, "manual")? else {
        return Err("No files found in save directory to back up.".to_string());
    };
    Ok(entry)
}

#[tauri::command]
fn list_backups(app: tauri::AppHandle, candidate_app_id: String) -> Result<Vec<BackupEntry>, String> {
    let conn = open_db(&app)?;
    ensure_schema(&conn)?;
    load_backups_for_game(&conn, &candidate_app_id)
}

#[tauri::command]
fn restore_backup(app: tauri::AppHandle, backup_id: i64) -> Result<(), String> {
    let conn = open_db(&app)?;
    ensure_schema(&conn)?;
    let backup = load_backup_by_id(&conn, backup_id)?;
    let game = load_reviewed_by_app_id(&conn, &backup.candidate_app_id)?;
    let save_path = game
        .save_path
        .clone()
        .ok_or_else(|| "Cannot restore: game has no save path.".to_string())?;

    // Safety backup of current save before restore.
    if Path::new(&save_path).exists() {
        let _ = create_backup_for_game(&conn, &app, &game, &save_path, "pre_restore_safety")?;
    }

    let source = PathBuf::from(&backup.backup_path);
    if !source.exists() {
        return Err("Backup source folder no longer exists.".to_string());
    }
    let destination = PathBuf::from(save_path);
    restore_directory(&source, &destination)?;
    Ok(())
}

#[tauri::command]
fn suggest_save_paths(
    candidate: GameCandidate,
    matched_title: Option<String>,
) -> Result<Vec<SavePathSuggestion>, String> {
    Ok(build_save_path_suggestions(&candidate, matched_title.as_deref()))
}

#[tauri::command]
fn set_save_location(
    app: tauri::AppHandle,
    candidate_app_id: String,
    save_path: String,
) -> Result<ReviewedGame, String> {
    let mut conn = open_db(&app)?;
    ensure_schema(&conn)?;
    let validated = validate_save_path(save_path.trim())?;
    let now = unix_ts();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("Failed to start save-location transaction: {e}"))?;

    tx.execute(
        "UPDATE save_locations SET is_active=0 WHERE candidate_app_id=?1",
        params![candidate_app_id],
    )
    .map_err(|e| format!("Failed to deactivate old save paths: {e}"))?;

    tx.execute(
        "INSERT INTO save_locations (candidate_app_id, path, kind, is_active, created_at)
         VALUES (?1, ?2, 'primary', 1, ?3)",
        params![candidate_app_id, validated, now],
    )
    .map_err(|e| format!("Failed to save save-location path: {e}"))?;

    tx.commit()
        .map_err(|e| format!("Failed to commit save-location transaction: {e}"))?;
    load_reviewed_by_app_id(&conn, &candidate_app_id)
}

#[tauri::command]
fn open_in_file_explorer(path: String) -> Result<(), String> {
    let target = PathBuf::from(path.trim());
    if target.as_os_str().is_empty() {
        return Err("Path is empty.".to_string());
    }
    if !target.exists() {
        return Err("Path does not exist.".to_string());
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .arg(target.to_string_lossy().to_string())
            .spawn()
            .map_err(|e| format!("Failed to open path in explorer: {e}"))?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&target)
            .spawn()
            .map_err(|e| format!("Failed to open path in Finder: {e}"))?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(&target)
            .spawn()
            .map_err(|e| format!("Failed to open path in file manager: {e}"))?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("Unsupported platform for opening file explorer.".to_string())
}

#[tauri::command]
fn get_game_icon(
    app: tauri::AppHandle,
    app_id: String,
    store: String,
    install_dir: String,
    detected_title: Option<String>,
    matched_title: Option<String>,
    matched_provider: Option<String>,
) -> Result<String, String> {
    let dirs = resolve_app_storage_dirs(&app)?;
    let icon_dir = dirs.cache.join("icons");
    fs::create_dir_all(&icon_dir).map_err(|e| format!("Failed to create icon cache dir: {e}"))?;

    let cache_key = format!(
        "v2|{store}|{app_id}|{install_dir}|{}|{}",
        matched_title.as_deref().unwrap_or_default(),
        matched_provider.as_deref().unwrap_or_default()
    );
    let cached_file = icon_dir.join(format!("{:016x}.img", hash_str(&cache_key)));
    let miss_file = icon_dir.join(format!("{:016x}.empty", hash_str(&cache_key)));
    let should_force_retry = matched_provider.as_deref() == Some("steam_public");

    if let Ok(bytes) = fs::read(&cached_file) {
        return Ok(bytes_to_data_url(&bytes));
    }
    if !should_force_retry && is_recent_marker(&miss_file, 30 * 60) {
        return Ok(default_icon_data_url());
    }

    if store == "steam" {
        if let Some(bytes) = fetch_steam_public_cover_bytes(&app_id)? {
            let _ = fs::write(&cached_file, &bytes);
            let _ = fs::remove_file(&miss_file);
            return Ok(bytes_to_data_url(&bytes));
        }
    }

    let steam_query = matched_title
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .or_else(|| {
            detected_title
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| v.to_string())
        });
    if store != "steam" {
        if let Some(query) = steam_query {
            if let Some(bytes) = fetch_steam_cover_by_search_title(&query)? {
                let _ = fs::write(&cached_file, &bytes);
                let _ = fs::remove_file(&miss_file);
                return Ok(bytes_to_data_url(&bytes));
            }
        }
    }

    if let Some(bytes) = extract_local_icon_candidate(Path::new(&install_dir)) {
        let _ = fs::write(&cached_file, &bytes);
        let _ = fs::remove_file(&miss_file);
        return Ok(bytes_to_data_url(&bytes));
    }

    let _ = fs::write(&miss_file, b"missing");
    Ok(default_icon_data_url())
}

#[tauri::command]
fn scan_steam_games(app: tauri::AppHandle) -> Result<Vec<GameCandidate>, String> {
    let mut steam = scan_steam_candidates();
    let _ = enrich_steam_candidates_with_metadata(&app, &mut steam);
    Ok(steam)
}

#[tauri::command]
fn scan_epic_games() -> Result<Vec<GameCandidate>, String> {
    Ok(scan_epic_candidates())
}

#[tauri::command]
fn scan_battlenet_games() -> Result<Vec<GameCandidate>, String> {
    Ok(scan_battlenet_candidates())
}

#[tauri::command]
fn scan_gog_games() -> Result<Vec<GameCandidate>, String> {
    Ok(scan_gog_candidates())
}

#[tauri::command]
fn scan_games(app: tauri::AppHandle) -> Result<ScanGamesResponse, String> {
    let mut steam = dedupe_candidates(scan_steam_candidates());
    let _ = enrich_steam_candidates_with_metadata(&app, &mut steam);
    let epic = dedupe_candidates(scan_epic_candidates());
    let battlenet = dedupe_candidates(scan_battlenet_candidates());
    let gog = dedupe_candidates(scan_gog_candidates());

    let mut excluded = launcher_install_paths(&steam);
    excluded.extend(launcher_install_paths(&epic));
    excluded.extend(launcher_install_paths(&battlenet));
    excluded.extend(launcher_install_paths(&gog));

    let local = dedupe_candidates(scan_local_candidates(&excluded));

    let groups = vec![
        make_source_group("steam", "Steam", steam),
        make_source_group("epic", "Epic Games", epic),
        make_source_group("battlenet", "Battle.net", battlenet),
        make_source_group("gog", "GOG", gog),
        make_source_group("local", "Manual / Local Scan", local),
    ];

    let total_candidates = groups.iter().map(|g| g.candidates.len()).sum();
    let total_auto_accepted = groups
        .iter()
        .flat_map(|g| g.matches.iter())
        .filter(|m| m.decision == "auto_accept")
        .count();

    Ok(ScanGamesResponse {
        sources: groups,
        total_candidates,
        total_auto_accepted,
    })
}

fn scan_steam_candidates() -> Vec<GameCandidate> {
    let mut games: Vec<GameCandidate> = Vec::new();
    let mut seen = HashSet::new();

    for steam_root in steam_roots() {
        let steamapps = steam_root.join("steamapps");
        if !steamapps.exists() {
            continue;
        }

        let mut libraries = read_libraries_from_vdf(&steamapps.join("libraryfolders.vdf"));
        libraries.push(steam_root.clone());

        for library in libraries {
            let library_key = normalize_path_for_key(&library);
            if !seen.insert(format!("library:{library_key}")) {
                continue;
            }

            let library_steamapps = library.join("steamapps");
            if !library_steamapps.exists() {
                continue;
            }

            let entries = match fs::read_dir(&library_steamapps) {
                Ok(v) => v,
                Err(_) => continue,
            };

            for entry in entries.flatten() {
                let path = entry.path();
                let file_name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(v) => v.to_string(),
                    None => continue,
                };

                if !file_name.starts_with("appmanifest_") || !file_name.ends_with(".acf") {
                    continue;
                }

                if let Some(game) = parse_appmanifest(&path, &library) {
                    let dedupe_key = format!("{}:{}", game.app_id, normalize_path_for_key(&library));
                    if seen.insert(dedupe_key) {
                        games.push(game);
                    }
                }
            }
        }
    }

    games.sort_by(|a, b| a.detected_title.cmp(&b.detected_title));
    games
}

fn scan_epic_candidates() -> Vec<GameCandidate> {
    let mut games = Vec::new();
    for dir in epic_manifest_dirs() {
        let entries = match fs::read_dir(&dir) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("item"))
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(game) = parse_epic_item_file(&path) {
                games.push(game);
            }
        }
    }
    games
}

fn scan_battlenet_candidates() -> Vec<GameCandidate> {
    let mut games = Vec::new();
    let known_dirs = [
        "World of Warcraft",
        "Overwatch",
        "Overwatch 2",
        "Diablo III",
        "Diablo IV",
        "Hearthstone",
        "StarCraft II",
        "Heroes of the Storm",
        "Warcraft III",
    ];
    for base in common_program_dirs() {
        for game_dir in known_dirs {
            let install_dir = base.join(game_dir);
            if install_dir.exists() {
                games.push(GameCandidate {
                    app_id: format!("bnet:{}", normalize_path_for_key(&install_dir)),
                    detected_title: game_dir.to_string(),
                    store: "battlenet".to_string(),
                    source: "launcher_heuristic".to_string(),
                    trusted_launcher_match: false,
                    library_path: base.to_string_lossy().to_string(),
                    install_dir: install_dir.to_string_lossy().to_string(),
                    manifest_path: String::new(),
                });
            }
        }
    }
    games
}

fn scan_gog_candidates() -> Vec<GameCandidate> {
    let mut games = Vec::new();
    for root in gog_roots() {
        let entries = match fs::read_dir(&root) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || !contains_root_exe(&path) {
                continue;
            }
            let title = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("GOG Game")
                .to_string();
            games.push(GameCandidate {
                app_id: format!("gog:{}", normalize_path_for_key(&path)),
                detected_title: title,
                store: "gog".to_string(),
                source: "launcher_heuristic".to_string(),
                trusted_launcher_match: false,
                library_path: root.to_string_lossy().to_string(),
                install_dir: path.to_string_lossy().to_string(),
                manifest_path: String::new(),
            });
        }
    }
    games
}

fn scan_local_candidates(excluded_install_paths: &HashSet<String>) -> Vec<GameCandidate> {
    let mut games = Vec::new();
    for scan_root in local_scan_roots() {
        let entries = match fs::read_dir(&scan_root) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let normalized = normalize_path_for_key(&path);
            if should_exclude_local_candidate(&normalized, excluded_install_paths) {
                continue;
            }
            if !contains_root_exe(&path) {
                continue;
            }
            let folder_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Unknown Game".to_string());
            games.push(GameCandidate {
                app_id: format!("local:{}", normalized),
                detected_title: folder_name,
                store: "local".to_string(),
                source: "filesystem_heuristic".to_string(),
                trusted_launcher_match: false,
                library_path: scan_root.to_string_lossy().to_string(),
                install_dir: path.to_string_lossy().to_string(),
                manifest_path: String::new(),
            });
        }
    }
    games
}

fn launcher_install_paths(candidates: &[GameCandidate]) -> HashSet<String> {
    candidates
        .iter()
        .filter(|c| c.store != "local")
        .map(|c| normalize_path_for_key(Path::new(&c.install_dir)))
        .collect()
}

fn should_exclude_local_candidate(path: &str, excluded_install_paths: &HashSet<String>) -> bool {
    excluded_install_paths
        .iter()
        .any(|excluded| path.starts_with(excluded))
}

fn contains_root_exe(path: &Path) -> bool {
    let entries = match fs::read_dir(path) {
        Ok(v) => v,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let candidate = entry.path();
        if candidate
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("exe"))
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

fn epic_manifest_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(program_data) = std::env::var("ProgramData") {
        dirs.push(
            PathBuf::from(program_data)
                .join("Epic")
                .join("EpicGamesLauncher")
                .join("Data")
                .join("Manifests"),
        );
    }
    if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
        dirs.push(
            PathBuf::from(local_appdata)
                .join("EpicGamesLauncher")
                .join("Saved")
                .join("Data")
                .join("Manifests"),
        );
    }
    dirs
}

fn parse_epic_item_file(path: &Path) -> Option<GameCandidate> {
    let content = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let display_name = value
        .get("DisplayName")
        .and_then(|v| v.as_str())
        .unwrap_or("Epic Game")
        .to_string();
    let install_location = value
        .get("InstallLocation")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if install_location.is_empty() {
        return None;
    }
    let app_name = value
        .get("AppName")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("CatalogItemId").and_then(|v| v.as_str()))
        .unwrap_or(&display_name)
        .to_string();

    Some(GameCandidate {
        app_id: format!("epic:{app_name}"),
        detected_title: display_name,
        store: "epic".to_string(),
        source: "launcher_manifest".to_string(),
        trusted_launcher_match: true,
        library_path: Path::new(&install_location)
            .parent()
            .unwrap_or(Path::new(""))
            .to_string_lossy()
            .to_string(),
        install_dir: install_location,
        manifest_path: path.to_string_lossy().to_string(),
    })
}

fn local_scan_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(custom) = std::env::var("CHECKPOINT_SCAN_DIRS") {
        for part in custom.split(';') {
            let trimmed = part.trim();
            if !trimmed.is_empty() {
                roots.push(PathBuf::from(trimmed));
            }
        }
    }
    for default in ["C:\\Games", "D:\\Games", "E:\\Games"] {
        roots.push(PathBuf::from(default));
    }
    roots
}

fn gog_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    roots.push(PathBuf::from("C:\\GOG Games"));
    roots.push(PathBuf::from("D:\\GOG Games"));
    if let Ok(program_files) = std::env::var("ProgramFiles") {
        roots.push(PathBuf::from(program_files).join("GOG Galaxy").join("Games"));
    }
    roots
}

fn common_program_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(program_files_x86) = std::env::var("ProgramFiles(x86)") {
        dirs.push(PathBuf::from(program_files_x86));
    }
    if let Ok(program_files) = std::env::var("ProgramFiles") {
        dirs.push(PathBuf::from(program_files));
    }
    dirs
}

fn dedupe_candidates(candidates: Vec<GameCandidate>) -> Vec<GameCandidate> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for candidate in candidates {
        let key = format!("{}|{}", candidate.store, normalize_path_for_key(Path::new(&candidate.install_dir)));
        if seen.insert(key) {
            deduped.push(candidate);
        }
    }
    deduped
}

fn make_source_group(
    source_key: &str,
    source_label: &str,
    mut candidates: Vec<GameCandidate>,
) -> ScanSourceGroup {
    candidates.sort_by(|a, b| a.detected_title.cmp(&b.detected_title));
    let matches = candidates
        .iter()
        .map(|candidate| {
            if candidate.trusted_launcher_match {
                MatchResult {
                    candidate_app_id: candidate.app_id.clone(),
                    candidate_title: candidate.detected_title.clone(),
                    matched_title: Some(candidate.detected_title.clone()),
                    confidence: 0.97,
                    decision: "auto_accept".to_string(),
                    provider: "local_launcher".to_string(),
                    reason:
                        "Detected from trusted launcher manifest, API matching skipped.".to_string(),
                }
            } else {
                match_candidate(candidate)
            }
        })
        .collect();

    ScanSourceGroup {
        source_key: source_key.to_string(),
        source_label: source_label.to_string(),
        candidates,
        matches,
    }
}

fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(program_files_x86) = std::env::var("ProgramFiles(x86)") {
        roots.push(PathBuf::from(program_files_x86).join("Steam"));
    }
    if let Ok(program_files) = std::env::var("ProgramFiles") {
        roots.push(PathBuf::from(program_files).join("Steam"));
    }
    if let Ok(custom_steam) = std::env::var("STEAM_DIR") {
        roots.push(PathBuf::from(custom_steam));
    }

    roots
}

fn read_libraries_from_vdf(library_vdf_path: &Path) -> Vec<PathBuf> {
    let mut libraries = Vec::new();
    let content = match fs::read_to_string(library_vdf_path) {
        Ok(v) => v,
        Err(_) => return libraries,
    };

    for line in content.lines() {
        let quoted = extract_quoted_tokens(line);
        if quoted.len() >= 2 && quoted[0] == "path" {
            libraries.push(PathBuf::from(unescape_vdf_path(&quoted[1])));
            continue;
        }
        if quoted.len() >= 2 && quoted[0].chars().all(|c| c.is_ascii_digit()) {
            libraries.push(PathBuf::from(unescape_vdf_path(&quoted[1])));
        }
    }

    libraries
}

fn parse_appmanifest(manifest_path: &Path, library_root: &Path) -> Option<GameCandidate> {
    let content = fs::read_to_string(manifest_path).ok()?;
    let mut app_id: Option<String> = None;
    let mut name: Option<String> = None;
    let mut install_dir_name: Option<String> = None;

    for line in content.lines() {
        let quoted = extract_quoted_tokens(line);
        if quoted.len() < 2 {
            continue;
        }

        match quoted[0].as_str() {
            "appid" => app_id = Some(quoted[1].clone()),
            "name" => name = Some(quoted[1].clone()),
            "installdir" => install_dir_name = Some(quoted[1].clone()),
            _ => {}
        }
    }

    let app_id = app_id?;
    let detected_title = name.unwrap_or_else(|| format!("Steam App {app_id}"));
    let install_dir_name = install_dir_name.unwrap_or_default();
    let install_dir = library_root.join("steamapps").join("common").join(install_dir_name);

    Some(GameCandidate {
        app_id,
        detected_title,
        store: "steam".to_string(),
        source: "launcher_manifest".to_string(),
        trusted_launcher_match: true,
        library_path: library_root.to_string_lossy().to_string(),
        install_dir: install_dir.to_string_lossy().to_string(),
        manifest_path: manifest_path.to_string_lossy().to_string(),
    })
}

fn enrich_steam_candidates_with_metadata(
    app: &tauri::AppHandle,
    candidates: &mut [GameCandidate],
) -> Result<Vec<String>, String> {
    let conn = open_db(app)?;
    ensure_schema(&conn)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
        .map_err(|e| format!("Failed to create Steam metadata client: {e}"))?;

    let mut warnings = Vec::new();
    for candidate in candidates.iter_mut() {
        if candidate.store != "steam" || !candidate.app_id.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        match get_steam_metadata_cached(&conn, &client, &candidate.app_id) {
            Ok(Some(meta)) => {
                if !meta.name.trim().is_empty() {
                    candidate.detected_title = meta.name;
                }
            }
            Ok(None) => {}
            Err(err) => {
                if warnings.len() < 5 {
                    warnings.push(format!(
                        "Steam metadata unavailable for app {}: {}",
                        candidate.app_id, err
                    ));
                }
            }
        }
    }
    Ok(warnings)
}

fn get_steam_metadata_cached(
    conn: &Connection,
    client: &Client,
    app_id: &str,
) -> Result<Option<SteamMetadata>, String> {
    let now = unix_ts();
    let mut cached: Option<(SteamMetadata, i64)> = None;

    let mut stmt = conn
        .prepare(
            "SELECT name, short_description, genres_json, updated_at
             FROM steam_metadata
             WHERE app_id=?1
             LIMIT 1",
        )
        .map_err(|e| format!("Failed to prepare steam_metadata query: {e}"))?;
    let mut rows = stmt
        .query(params![app_id])
        .map_err(|e| format!("Failed to query steam_metadata: {e}"))?;
    if let Some(row) = rows
        .next()
        .map_err(|e| format!("Failed to iterate steam_metadata row: {e}"))?
    {
        let name: String = row
            .get(0)
            .map_err(|e| format!("Failed to read steam metadata name: {e}"))?;
        let short_description: String = row
            .get(1)
            .map_err(|e| format!("Failed to read steam metadata description: {e}"))?;
        let genres_json: String = row
            .get(2)
            .map_err(|e| format!("Failed to read steam metadata genres: {e}"))?;
        let updated_at: i64 = row
            .get(3)
            .map_err(|e| format!("Failed to read steam metadata timestamp: {e}"))?;
        let genres = serde_json::from_str::<Vec<String>>(&genres_json).unwrap_or_default();
        cached = Some((
            SteamMetadata {
                name,
                short_description,
                genres,
            },
            updated_at,
        ));
    }

    if let Some((meta, updated_at)) = &cached {
        if now - updated_at < 30 * 24 * 60 * 60 {
            return Ok(Some(meta.clone()));
        }
    }

    match fetch_steam_app_details(client, app_id)? {
        Some(fresh) => {
            let genres_json = serde_json::to_string(&fresh.genres).unwrap_or_else(|_| "[]".to_string());
            conn.execute(
                "INSERT INTO steam_metadata (app_id, name, short_description, genres_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(app_id) DO UPDATE SET
                   name=excluded.name,
                   short_description=excluded.short_description,
                   genres_json=excluded.genres_json,
                   updated_at=excluded.updated_at",
                params![app_id, &fresh.name, &fresh.short_description, &genres_json, now],
            )
            .map_err(|e| format!("Failed to upsert steam metadata: {e}"))?;
            Ok(Some(fresh))
        }
        None => Ok(cached.map(|(meta, _)| meta)),
    }
}

fn fetch_steam_app_details(client: &Client, app_id: &str) -> Result<Option<SteamMetadata>, String> {
    if !app_id.chars().all(|c| c.is_ascii_digit()) {
        return Ok(None);
    }
    let url = format!(
        "https://store.steampowered.com/api/appdetails?appids={}",
        app_id
    );
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("Steam appdetails request failed: {e}"))?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let payload: serde_json::Value = response
        .json()
        .map_err(|e| format!("Steam appdetails JSON parse failed: {e}"))?;
    let Some(entry) = payload.get(app_id) else {
        return Ok(None);
    };
    let success = entry
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !success {
        return Ok(None);
    }
    let Some(data) = entry.get("data") else {
        return Ok(None);
    };
    let name = data
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if name.trim().is_empty() {
        return Ok(None);
    }
    let short_description = data
        .get("short_description")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let genres = data
        .get("genres")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|genre| {
                    genre
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(Some(SteamMetadata {
        name,
        short_description,
        genres,
    }))
}

fn match_candidate(candidate: &GameCandidate) -> MatchResult {
    let variants = query_variants(candidate);
    let mut best_title: Option<String> = None;
    let mut best_confidence = local_evidence_confidence(candidate);
    let mut best_reason = "No API match; using local evidence only.".to_string();
    let mut best_provider = "local_evidence".to_string();
    let mut seen_hits = HashSet::new();

    // For manual/non-Steam candidates, try Steam public search.
    if candidate.store != "steam" {
        for variant in variants {
            if let Ok(steam_hits) = fetch_steam_storesearch_hits(&variant) {
                for hit in steam_hits {
                    let hit_key = format!("steam_public:{}", hit.id);
                    if !seen_hits.insert(hit_key) {
                        continue;
                    }
                    let score = score_match(candidate, &hit.name);
                    if score > best_confidence {
                        best_confidence = score;
                        best_title = Some(hit.name.clone());
                        best_provider = "steam_public".to_string();
                        best_reason = "Title similarity matched via Steam public store search."
                            .to_string();
                    }
                }
            }
        }
    }

    MatchResult {
        candidate_app_id: candidate.app_id.clone(),
        candidate_title: candidate.detected_title.clone(),
        matched_title: best_title,
        confidence: (best_confidence * 100.0).round() / 100.0,
        decision: decide(best_confidence),
        provider: best_provider,
        reason: best_reason,
    }
}

fn query_variants(candidate: &GameCandidate) -> Vec<String> {
    let mut variants = Vec::new();
    variants.push(candidate.detected_title.clone());

    if let Some(folder_name) = Path::new(&candidate.install_dir)
        .file_name()
        .and_then(|v| v.to_str())
        .map(clean_title)
    {
        if !folder_name.is_empty() {
            variants.push(folder_name);
        }
    }

    variants.push(clean_title(&candidate.detected_title));
    variants.sort();
    variants.dedup();
    variants
}

fn clean_title(input: &str) -> String {
    let mut cleaned = input.replace(['_', '-'], " ");
    for noise in ["game of the year", "goty", "ultimate edition", "deluxe edition", "steam"] {
        cleaned = cleaned.replace(noise, "");
        cleaned = cleaned.replace(&noise.to_uppercase(), "");
    }
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn score_match(candidate: &GameCandidate, matched_title: &str) -> f64 {
    let title_sim = title_similarity(&candidate.detected_title, matched_title);
    let folder_name = Path::new(&candidate.install_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let folder_sim = title_similarity(folder_name, matched_title);
    let store_bonus = if candidate.store == "steam" { 0.08 } else { 0.0 };
    (0.68 * title_sim + 0.24 * folder_sim + store_bonus).clamp(0.0, 0.99)
}

fn local_evidence_confidence(candidate: &GameCandidate) -> f64 {
    let has_manifest = !candidate.manifest_path.is_empty();
    let has_title = !candidate.detected_title.trim().is_empty();
    let has_install = !candidate.install_dir.trim().is_empty();
    match (has_manifest, has_title, has_install) {
        (true, true, true) => 0.72,
        (true, true, false) => 0.62,
        (true, false, true) => 0.55,
        _ => 0.45,
    }
}

fn decide(confidence: f64) -> String {
    if confidence >= 0.90 {
        "auto_accept".to_string()
    } else if confidence >= 0.70 {
        "needs_confirmation".to_string()
    } else {
        "unknown".to_string()
    }
}

fn title_similarity(a: &str, b: &str) -> f64 {
    let a_norm = normalize_title(a);
    let b_norm = normalize_title(b);
    if a_norm.is_empty() || b_norm.is_empty() {
        return 0.0;
    }
    if a_norm == b_norm {
        return 1.0;
    }
    if a_norm.contains(&b_norm) || b_norm.contains(&a_norm) {
        return 0.9;
    }

    let a_tokens = tokenize(&a_norm);
    let b_tokens = tokenize(&b_norm);
    jaccard_similarity(&a_tokens, &b_tokens)
}

fn normalize_title(input: &str) -> String {
    input
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == ' ' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn tokenize(input: &str) -> Vec<String> {
    input.split_whitespace().map(|s| s.to_string()).collect()
}

fn jaccard_similarity(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut counts = HashMap::new();
    for item in a {
        counts.insert(item, 1_u8);
    }
    let mut intersection = 0;
    for item in b {
        if counts.contains_key(item) {
            intersection += 1;
        }
    }
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

fn extract_quoted_tokens(line: &str) -> Vec<String> {
    line.split('"')
        .enumerate()
        .filter_map(|(idx, part)| {
            if idx % 2 == 1 {
                Some(part.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn unescape_vdf_path(raw: &str) -> String {
    raw.replace("\\\\", "\\")
}

fn normalize_path_for_key(path: &Path) -> String {
    let joined = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/");
    if cfg!(windows) {
        joined.to_lowercase()
    } else {
        joined
    }
}

fn hash_str(input: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher);
    hasher.finish()
}

fn is_recent_marker(path: &Path, max_age_seconds: u64) -> bool {
    let metadata = match fs::metadata(path) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let modified = match metadata.modified() {
        Ok(v) => v,
        Err(_) => return false,
    };
    let age = match modified.elapsed() {
        Ok(v) => v.as_secs(),
        Err(_) => return false,
    };
    age <= max_age_seconds
}

fn bytes_to_data_url(bytes: &[u8]) -> String {
    let mime = detect_mime(bytes);
    format!("data:{mime};base64,{}", BASE64_STANDARD.encode(bytes))
}

fn detect_mime(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 8
        && bytes[0] == 0x89
        && bytes[1] == 0x50
        && bytes[2] == 0x4E
        && bytes[3] == 0x47
    {
        "image/png"
    } else if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        "image/jpeg"
    } else if bytes.len() >= 4
        && bytes[0] == 0x00
        && bytes[1] == 0x00
        && bytes[2] == 0x01
        && bytes[3] == 0x00
    {
        "image/x-icon"
    } else if bytes.len() >= 12
        && &bytes[0..4] == b"RIFF"
        && &bytes[8..12] == b"WEBP"
    {
        "image/webp"
    } else {
        "application/octet-stream"
    }
}

fn extract_local_icon_candidate(install_dir: &Path) -> Option<Vec<u8>> {
    let entries = fs::read_dir(install_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_icon = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| {
                matches!(
                    e.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "webp" | "ico"
                )
            })
            .unwrap_or(false);
        if !is_icon {
            continue;
        }
        if let Ok(bytes) = fs::read(&path) {
            if !bytes.is_empty() {
                return Some(bytes);
            }
        }
    }
    None
}

fn fetch_steam_public_cover_bytes(app_id: &str) -> Result<Option<Vec<u8>>, String> {
    if !app_id.chars().all(|c| c.is_ascii_digit()) {
        return Ok(None);
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
        .map_err(|e| format!("Failed to create Steam icon client: {e}"))?;

    let candidates = [
        format!(
            "https://cdn.cloudflare.steamstatic.com/steam/apps/{}/header.jpg",
            app_id
        ),
        format!(
            "https://cdn.akamai.steamstatic.com/steam/apps/{}/capsule_231x87.jpg",
            app_id
        ),
    ];

    for url in candidates {
        let response = client
            .get(url)
            .send()
            .map_err(|e| format!("Steam icon fetch failed: {e}"))?;
        if !response.status().is_success() {
            continue;
        }
        let bytes = response
            .bytes()
            .map_err(|e| format!("Steam icon bytes decode failed: {e}"))?;
        if !bytes.is_empty() {
            return Ok(Some(bytes.to_vec()));
        }
    }
    Ok(None)
}

fn fetch_steam_storesearch_hits(query: &str) -> Result<Vec<ApiGameHit>, String> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("Failed to create Steam storesearch client: {e}"))?;
    let response = client
        .get("https://store.steampowered.com/api/storesearch/")
        .query(&[
            ("term", query),
            ("cc", "US"),
            ("l", "english"),
        ])
        .send()
        .map_err(|e| format!("Steam storesearch request failed: {e}"))?;
    if !response.status().is_success() {
        return Ok(Vec::new());
    }
    let payload: SteamStoreSearchResponse = response
        .json()
        .map_err(|e| format!("Steam storesearch JSON parse failed: {e}"))?;
    let hits = payload
        .items
        .unwrap_or_default()
        .into_iter()
        .take(5)
        .map(|item| ApiGameHit {
            id: item.id,
            name: item.name,
        })
        .collect();
    Ok(hits)
}

fn fetch_steam_cover_by_search_title(query: &str) -> Result<Option<Vec<u8>>, String> {
    let hits = fetch_steam_storesearch_hits(query)?;
    if hits.is_empty() {
        return Ok(None);
    }

    let mut best_app_id: Option<String> = None;
    let mut best_score = 0.0_f64;
    for hit in hits {
        let score = title_similarity(query, &hit.name);
        if score > best_score {
            best_score = score;
            best_app_id = Some(hit.id.to_string());
        }
    }

    if best_score < 0.72 {
        return Ok(None);
    }

    if let Some(app_id) = best_app_id {
        return fetch_steam_public_cover_bytes(&app_id);
    }
    Ok(None)
}

fn default_icon_data_url() -> String {
    "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='128' height='128'><rect width='100%' height='100%' rx='16' fill='%230f2c21'/><text x='50%' y='56%' dominant-baseline='middle' text-anchor='middle' font-size='58' fill='%2320bf6b'>%F0%9F%8E%AE</text></svg>".to_string()
}

fn build_save_path_suggestions(
    candidate: &GameCandidate,
    matched_title: Option<&str>,
) -> Vec<SavePathSuggestion> {
    let mut candidates = Vec::new();
    let title = matched_title
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(candidate.detected_title.as_str());
    let safe_title = sanitize_for_folder(title);
    let install_dir = PathBuf::from(&candidate.install_dir);
    let volume = get_volume(Path::new(&candidate.install_dir));

    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        push_unique_suggestion_candidate(
            &mut candidates,
            PathBuf::from(&user_profile)
                .join("Documents")
                .join("My Games")
                .join(&safe_title),
            "Documents\\My Games",
        );
        push_unique_suggestion_candidate(
            &mut candidates,
            PathBuf::from(&user_profile).join("Saved Games").join(&safe_title),
            "Saved Games",
        );
    }
    if let Ok(app_data) = std::env::var("APPDATA") {
        push_unique_suggestion_candidate(
            &mut candidates,
            PathBuf::from(app_data).join(&safe_title),
            "%APPDATA%",
        );
    }
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        push_unique_suggestion_candidate(
            &mut candidates,
            PathBuf::from(&local_app_data).join(&safe_title).join("Saved").join("SaveGames"),
            "%LOCALAPPDATA% Saved/SaveGames",
        );
        push_unique_suggestion_candidate(
            &mut candidates,
            PathBuf::from(local_app_data).join(&safe_title).join("Saved"),
            "%LOCALAPPDATA% Saved",
        );
    }

    if candidate.store == "steam" && candidate.app_id.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(program_files_x86) = std::env::var("ProgramFiles(x86)") {
            push_unique_suggestion_candidate(
                &mut candidates,
                PathBuf::from(program_files_x86)
                    .join("Steam")
                    .join("userdata")
                    .join("<SteamID>")
                    .join(&candidate.app_id)
                    .join("remote"),
                "Steam userdata remote",
            );
        }
    }

    if let Some(subfolder) = find_common_save_subfolder(&install_dir) {
        push_unique_suggestion_candidate(&mut candidates, subfolder, "install subfolder");
    }
    if let Some(existing) = find_existing_save_folder(&install_dir) {
        push_unique_suggestion_candidate(&mut candidates, existing, "existing save folder");
    }

    push_unique_suggestion_candidate(
        &mut candidates,
        install_dir.join("Save"),
        "install_dir_heuristic",
    );
    push_unique_suggestion_candidate(
        &mut candidates,
        install_dir.join("Saved").join("SaveGames"),
        "install_dir_heuristic",
    );

    if !volume.eq_ignore_ascii_case("C:") {
        push_unique_suggestion_candidate(
            &mut candidates,
            PathBuf::from(format!("{}\\GameSaves\\{}", volume, safe_title)),
            "volume_root_suggestion",
        );
    }

    let mut scored: Vec<(SavePathSuggestion, usize)> = candidates
        .into_iter()
        .map(|suggestion| {
            let mut score = 0usize;
            if fs::metadata(&suggestion.path).is_ok() {
                score += 10;
            }
            let lowered = suggestion.path.to_lowercase();
            if lowered.contains("saved games") || lowered.contains("my games") {
                score += 4;
            }
            if lowered.contains("save") {
                score += 2;
            }
            if suggestion.source.contains("install") {
                score += 1;
            }
            (suggestion, score)
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1));

    let mut unique = Vec::new();
    for (suggestion, _) in scored {
        if unique
            .iter()
            .any(|item: &SavePathSuggestion| item.path.eq_ignore_ascii_case(&suggestion.path))
        {
            continue;
        }
        unique.push(suggestion);
        if unique.len() >= 5 {
            break;
        }
    }

    unique
}

fn push_unique_suggestion_candidate(
    suggestions: &mut Vec<SavePathSuggestion>,
    path: PathBuf,
    source: &str,
) {
    let path_string = path.to_string_lossy().to_string();
    if suggestions.iter().any(|item| item.path == path_string) {
        return;
    }
    suggestions.push(SavePathSuggestion {
        path: path_string,
        source: source.to_string(),
    });
}

fn get_volume(path: &Path) -> String {
    let as_text = path.to_string_lossy();
    let chars: Vec<char> = as_text.chars().collect();
    if chars.len() >= 2 && chars[1] == ':' {
        return as_text[..2].to_string();
    }
    "C:".to_string()
}

fn find_common_save_subfolder(install_dir: &Path) -> Option<PathBuf> {
    let names = ["Saves", "Saved", "SaveGames", "save", "savedata"];
    for name in names {
        let path = install_dir.join(name);
        if path.is_dir() {
            return Some(path);
        }
    }
    None
}

fn find_existing_save_folder(install_dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(install_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.contains("save") {
            return Some(path);
        }
    }
    None
}

fn sanitize_for_folder(input: &str) -> String {
    let cleaned = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == ' ' || ch == '-' || ch == '_' {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>();
    let normalized = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        "Unknown Game".to_string()
    } else {
        normalized
    }
}

fn validate_save_path(path: &str) -> Result<String, String> {
    let candidate = PathBuf::from(path);
    if !candidate.is_absolute() {
        return Err("Save path must be an absolute path.".to_string());
    }
    if candidate
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("Save path cannot contain parent traversal ('..').".to_string());
    }
    Ok(candidate.to_string_lossy().to_string())
}

fn create_backup_for_game(
    conn: &Connection,
    app: &tauri::AppHandle,
    game: &ReviewedGame,
    save_path: &str,
    trigger_reason: &str,
) -> Result<Option<BackupEntry>, String> {
    let source = PathBuf::from(save_path.trim());
    if !source.exists() || !source.is_dir() {
        return Ok(None);
    }

    let snapshot = snapshot_directory(&source)?;
    if snapshot.file_count == 0 {
        return Ok(None);
    }

    let dirs = resolve_app_storage_dirs(app)?;
    let game_dir = dirs
        .backups
        .join(sanitize_for_folder(&game.detected_title))
        .join(format!("{}-{}", sanitize_for_folder(&game.store), sanitize_for_folder(&game.candidate_app_id)));
    let timestamp = unix_ts();
    let backup_dir = game_dir.join(format!("{timestamp}"));
    copy_dir_recursive(&source, &backup_dir)?;

    conn.execute(
        "INSERT INTO backups (
            candidate_app_id, created_at, backup_path, trigger_reason, file_count, total_bytes
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            game.candidate_app_id,
            timestamp,
            backup_dir.to_string_lossy().to_string(),
            trigger_reason,
            snapshot.file_count as i64,
            snapshot.total_bytes as i64
        ],
    )
    .map_err(|e| format!("Failed to insert backup metadata: {e}"))?;

    let id = conn.last_insert_rowid();
    Ok(Some(BackupEntry {
        id,
        candidate_app_id: game.candidate_app_id.clone(),
        created_at: timestamp,
        backup_path: backup_dir.to_string_lossy().to_string(),
        trigger_reason: trigger_reason.to_string(),
        file_count: snapshot.file_count as i64,
        total_bytes: snapshot.total_bytes as i64,
    }))
}

#[derive(Debug, Clone, Default)]
struct DirectorySnapshot {
    file_count: usize,
    total_bytes: u64,
}

fn snapshot_directory(path: &Path) -> Result<DirectorySnapshot, String> {
    let mut snapshot = DirectorySnapshot::default();
    collect_directory_stats(path, &mut snapshot)?;
    Ok(snapshot)
}

fn collect_directory_stats(path: &Path, snapshot: &mut DirectorySnapshot) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(path)
        .map_err(|e| format!("Failed to read directory {}: {e}", path.display()))?;
    for entry in entries.flatten() {
        let item_path = entry.path();
        let metadata = match entry.metadata() {
            Ok(v) => v,
            Err(_) => continue,
        };
        if metadata.is_dir() {
            collect_directory_stats(&item_path, snapshot)?;
            continue;
        }
        if metadata.is_file() {
            snapshot.file_count += 1;
            snapshot.total_bytes += metadata.len();
        }
    }
    Ok(())
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|e| format!("Failed to create destination {}: {e}", destination.display()))?;
    let entries = fs::read_dir(source)
        .map_err(|e| format!("Failed to read source directory {}: {e}", source.display()))?;
    for entry in entries.flatten() {
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = match entry.metadata() {
            Ok(v) => v,
            Err(_) => continue,
        };
        if metadata.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if metadata.is_file() {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent directory {}: {e}", parent.display()))?;
            }
            fs::copy(&from, &to)
                .map_err(|e| format!("Failed to copy file {} to {}: {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn restore_directory(source: &Path, destination: &Path) -> Result<(), String> {
    if destination.exists() {
        fs::remove_dir_all(destination).map_err(|e| {
            format!(
                "Failed to clear destination {} before restore: {e}",
                destination.display()
            )
        })?;
    }
    copy_dir_recursive(source, destination)
}

fn load_backups_for_game(conn: &Connection, candidate_app_id: &str) -> Result<Vec<BackupEntry>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, candidate_app_id, created_at, backup_path, trigger_reason, file_count, total_bytes
             FROM backups
             WHERE candidate_app_id = ?1
             ORDER BY created_at DESC",
        )
        .map_err(|e| format!("Failed to prepare backups query: {e}"))?;
    let rows = stmt
        .query_map(params![candidate_app_id], |row| {
            Ok(BackupEntry {
                id: row.get(0)?,
                candidate_app_id: row.get(1)?,
                created_at: row.get(2)?,
                backup_path: row.get(3)?,
                trigger_reason: row.get(4)?,
                file_count: row.get(5)?,
                total_bytes: row.get(6)?,
            })
        })
        .map_err(|e| format!("Failed to map backups query: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Failed to read backup row: {e}"))?);
    }
    Ok(out)
}

fn load_backup_by_id(conn: &Connection, backup_id: i64) -> Result<BackupEntry, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, candidate_app_id, created_at, backup_path, trigger_reason, file_count, total_bytes
             FROM backups
             WHERE id = ?1
             LIMIT 1",
        )
        .map_err(|e| format!("Failed to prepare backup-by-id query: {e}"))?;
    let mut rows = stmt
        .query(params![backup_id])
        .map_err(|e| format!("Failed to query backup by id: {e}"))?;
    if let Some(row) = rows
        .next()
        .map_err(|e| format!("Failed to iterate backup-by-id row: {e}"))?
    {
        Ok(BackupEntry {
            id: row.get(0).map_err(|e| format!("Failed to read backup id: {e}"))?,
            candidate_app_id: row
                .get(1)
                .map_err(|e| format!("Failed to read backup candidate app id: {e}"))?,
            created_at: row
                .get(2)
                .map_err(|e| format!("Failed to read backup created_at: {e}"))?,
            backup_path: row
                .get(3)
                .map_err(|e| format!("Failed to read backup path: {e}"))?,
            trigger_reason: row
                .get(4)
                .map_err(|e| format!("Failed to read backup trigger reason: {e}"))?,
            file_count: row
                .get(5)
                .map_err(|e| format!("Failed to read backup file_count: {e}"))?,
            total_bytes: row
                .get(6)
                .map_err(|e| format!("Failed to read backup total_bytes: {e}"))?,
        })
    } else {
        Err("Backup not found.".to_string())
    }
}

fn open_db(app: &tauri::AppHandle) -> Result<Connection, String> {
    let dirs = resolve_app_storage_dirs(app)?;
    let db_path = dirs.metadata.join("checkpoint.db");
    let conn = Connection::open(db_path).map_err(|e| format!("Failed to open SQLite DB: {e}"))?;
    conn.execute("PRAGMA foreign_keys = ON", [])
        .map_err(|e| format!("Failed to enable sqlite foreign keys: {e}"))?;
    Ok(conn)
}

fn resolve_app_storage_dirs(app: &tauri::AppHandle) -> Result<AppStorageDirs, String> {
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("Failed to resolve local app data dir: {e}"))?;
    let metadata = root.join("metadata");
    let cache = root.join("cache");
    let backups = root.join("backups");
    let logs = root.join("logs");

    for dir in [&root, &metadata, &cache, &backups, &logs] {
        fs::create_dir_all(dir)
            .map_err(|e| format!("Failed to create app storage directory {}: {e}", dir.display()))?;
    }

    Ok(AppStorageDirs {
        metadata,
        cache,
        backups,
    })
}

fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS reviewed_games (
            candidate_app_id TEXT PRIMARY KEY,
            detected_title TEXT NOT NULL,
            store TEXT NOT NULL,
            source TEXT NOT NULL,
            install_dir TEXT NOT NULL,
            library_path TEXT NOT NULL,
            manifest_path TEXT NOT NULL,
            trusted_launcher_match INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL CHECK (status IN ('accepted', 'rejected')),
            reviewed_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS save_locations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            candidate_app_id TEXT NOT NULL,
            path TEXT NOT NULL,
            kind TEXT NOT NULL DEFAULT 'primary',
            is_active INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL,
            FOREIGN KEY(candidate_app_id) REFERENCES reviewed_games(candidate_app_id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS steam_metadata (
            app_id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            short_description TEXT NOT NULL,
            genres_json TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS backups (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            candidate_app_id TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            backup_path TEXT NOT NULL,
            trigger_reason TEXT NOT NULL,
            file_count INTEGER NOT NULL DEFAULT 0,
            total_bytes INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY(candidate_app_id) REFERENCES reviewed_games(candidate_app_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_reviewed_games_status ON reviewed_games(status);
        CREATE INDEX IF NOT EXISTS idx_save_locations_candidate_active ON save_locations(candidate_app_id, is_active);
        CREATE INDEX IF NOT EXISTS idx_steam_metadata_updated_at ON steam_metadata(updated_at);
        CREATE INDEX IF NOT EXISTS idx_backups_candidate_created ON backups(candidate_app_id, created_at DESC);",
    )
    .map_err(|e| format!("Failed to initialize schema: {e}"))
}

fn load_reviewed_games(conn: &Connection) -> Result<Vec<ReviewedGame>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT
                rg.candidate_app_id,
                rg.detected_title,
                rg.store,
                rg.source,
                rg.install_dir,
                rg.status,
                sl.path,
                rg.reviewed_at
            FROM reviewed_games rg
            LEFT JOIN save_locations sl
              ON sl.candidate_app_id = rg.candidate_app_id
             AND sl.is_active = 1
            ORDER BY rg.reviewed_at DESC",
        )
        .map_err(|e| format!("Failed to query reviewed games: {e}"))?;

    let rows = stmt
        .query_map([], |row| {
            Ok(ReviewedGame {
                candidate_app_id: row.get(0)?,
                detected_title: row.get(1)?,
                store: row.get(2)?,
                source: row.get(3)?,
                install_dir: row.get(4)?,
                install_dir_exists: install_dir_exists(row.get::<_, String>(4)?.as_str()),
                status: row.get(5)?,
                save_path: row.get(6)?,
                reviewed_at: row.get(7)?,
            })
        })
        .map_err(|e| format!("Failed to map reviewed games: {e}"))?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Failed to read reviewed game row: {e}"))?);
    }
    Ok(out)
}

fn load_reviewed_by_app_id(conn: &Connection, app_id: &str) -> Result<ReviewedGame, String> {
    let mut stmt = conn
        .prepare(
            "SELECT
                rg.candidate_app_id,
                rg.detected_title,
                rg.store,
                rg.source,
                rg.install_dir,
                rg.status,
                sl.path,
                rg.reviewed_at
            FROM reviewed_games rg
            LEFT JOIN save_locations sl
              ON sl.candidate_app_id = rg.candidate_app_id
             AND sl.is_active = 1
            WHERE rg.candidate_app_id = ?1
            LIMIT 1",
        )
        .map_err(|e| format!("Failed to prepare reviewed game query: {e}"))?;

    let mut rows = stmt
        .query(params![app_id])
        .map_err(|e| format!("Failed to query reviewed game: {e}"))?;

    if let Some(row) = rows
        .next()
        .map_err(|e| format!("Failed to iterate reviewed game row: {e}"))?
    {
        let install_dir: String = row
            .get(4)
            .map_err(|e| format!("Failed to read install_dir: {e}"))?;
        Ok(ReviewedGame {
            candidate_app_id: row
                .get(0)
                .map_err(|e| format!("Failed to read candidate_app_id: {e}"))?,
            detected_title: row
                .get(1)
                .map_err(|e| format!("Failed to read detected_title: {e}"))?,
            store: row
                .get(2)
                .map_err(|e| format!("Failed to read store: {e}"))?,
            source: row
                .get(3)
                .map_err(|e| format!("Failed to read source: {e}"))?,
            install_dir_exists: install_dir_exists(install_dir.as_str()),
            install_dir,
            status: row
                .get(5)
                .map_err(|e| format!("Failed to read status: {e}"))?,
            save_path: row
                .get(6)
                .map_err(|e| format!("Failed to read save path: {e}"))?,
            reviewed_at: row
                .get(7)
                .map_err(|e| format!("Failed to read reviewed_at: {e}"))?,
        })
    } else {
        Err("Reviewed game record not found after save.".to_string())
    }
}

fn unix_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn install_dir_exists(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return false;
    }
    Path::new(trimmed).exists()
}

fn detect_running_process_for_game(
    game: &ReviewedGame,
    system: &System,
) -> (bool, Option<String>, Option<String>) {
    let install_dir_norm = normalize_path_for_key(Path::new(&game.install_dir));
    let game_title_norm = normalize_title(&game.detected_title);

    for process in system.processes().values() {
        let process_name = process.name().to_string_lossy().to_string();
        let process_name_norm = normalize_title(&process_name);
        let process_path = process
            .exe()
            .map(|value| value.to_string_lossy().to_string());
        let process_path_norm = process.exe().map(normalize_path_for_key);

        if let Some(path_norm) = &process_path_norm {
            if !install_dir_norm.is_empty() && path_norm.starts_with(&install_dir_norm) {
                return (true, Some(process_name), process_path);
            }
        }

        if !game_title_norm.is_empty()
            && (process_name_norm.contains(&game_title_norm)
                || title_similarity(&process_name_norm, &game_title_norm) >= 0.92)
        {
            return (true, Some(process_name), process_path);
        }
    }

    (false, None, None)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            health_check,
            review_game_candidate,
            list_reviewed_games,
            get_running_game_statuses,
            run_backup_cycle,
            create_backup_now,
            list_backups,
            restore_backup,
            suggest_save_paths,
            set_save_location,
            open_in_file_explorer,
            get_game_icon,
            scan_steam_games,
            scan_epic_games,
            scan_battlenet_games,
            scan_gog_games,
            scan_games
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
