use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use walkdir::WalkDir;

const CONFIG_FILE_NAME: &str = "repo-config.json";

static BASE_DIR_OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
/// Test-only redirection of the home directory, so a test can exercise paths
/// that are deliberately *not* relocatable by the user (see `cli_bridge`).
static HOME_DIR_OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
/// Test-only redirection of the config file. The save-then-restart round trip
/// has to write a real config file to be meaningful, and that must never be the
/// developer's own (~/.config/skills-manager/repo-config.json).
#[cfg(test)]
static CONFIG_PATH_OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
static SKILLS_DIR_OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
static STARTUP_WARNINGS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static STARTUP_ERROR_LOG: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

/// The relocation this startup performed, if any, as `(from, to)`.
///
/// Recorded here because repairing the agent-side links that point into the
/// library needs the adapters and projects that live behind the store — and this
/// runs before the store is open. `initialize_store_inner` drains it once the
/// store exists.
static LAST_MIGRATION: OnceLock<Mutex<Option<(PathBuf, PathBuf)>>> = OnceLock::new();

fn record_migration(from: &Path, to: &Path) {
    *LAST_MIGRATION
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some((from.to_path_buf(), to.to_path_buf()));
}

/// Take the relocation this startup performed, if any.
pub fn take_last_migration() -> Option<(PathBuf, PathBuf)> {
    LAST_MIGRATION
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
}

fn push_startup_warning(code: &str) {
    let mut warnings = STARTUP_WARNINGS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !warnings.iter().any(|w| w == code) {
        warnings.push(code.to_string());
    }
}

/// Warning codes recorded while resolving the central repository at startup.
/// The frontend maps them to localized banner text (`settings.repoWarning_*`).
pub fn startup_warnings() -> Vec<String> {
    STARTUP_WARNINGS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Record a detailed startup error for later logging. `ensure_central_repo`
/// runs before `tauri_plugin_log` is installed (see `run()` in lib.rs), so a
/// `log::error!` here is swallowed by the default no-op logger. Stash the
/// detail and let `setup` flush it once the real logger exists.
fn record_startup_error(message: String) {
    STARTUP_ERROR_LOG
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(message);
}

/// Drain the startup errors stashed by [`record_startup_error`]. Called from
/// `tauri::Builder::setup` once the logger is up so the detail lands in the log
/// file that a support bundle collects.
pub fn take_startup_errors() -> Vec<String> {
    let mut guard = STARTUP_ERROR_LOG
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut guard)
}

/// Global mutex shared by every test that mutates the base-dir override via
/// [`set_test_base_dir_override`]. The override is process-wide static state,
/// so any two tests holding their own per-module locks can still race. Tests
/// must take this guard before calling `set_test_base_dir_override` and keep
/// it alive until they restore the previous value.
#[cfg(test)]
static TEST_BASE_DIR_GUARD: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(test)]
pub(crate) fn test_base_dir_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_BASE_DIR_GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RepoPathConfig {
    repo_path: Option<String>,
    pending_migration_from: Option<String>,
}

fn default_base_dir() -> PathBuf {
    home_base_dir()
}

/// The location the app falls back to when no Central Repo Path is configured.
///
/// Exposed so the UI can inspect it before "reset to default": the default may
/// already hold a library, and the same safety rule that refuses to migrate over
/// user data would then leave the setting stuck on a permanent pending notice.
pub fn default_repo_path() -> PathBuf {
    default_base_dir()
}

/// `~/.skills-manager`, ignoring any configured relocation.
///
/// The library can be moved anywhere the user likes, but a few things must
/// stay where another program can find them without being told — the CLI
/// bridge an agent runs, above all. Those use this rather than [`base_dir`].
pub fn home_base_dir() -> PathBuf {
    if let Some(path) = HOME_DIR_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .clone()
    {
        return path.join(".skills-manager");
    }
    dirs::home_dir()
        .expect("Cannot determine home directory")
        .join(".skills-manager")
}

#[cfg(test)]
pub(crate) fn set_test_home_dir_override(path: Option<PathBuf>) {
    *HOME_DIR_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = path;
}

fn config_file_path() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = CONFIG_PATH_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return path;
    }
    dirs::config_dir()
        .unwrap_or_else(default_base_dir)
        .join("skills-manager")
        .join(CONFIG_FILE_NAME)
}

#[cfg(test)]
pub(crate) fn set_test_config_path_override(path: Option<PathBuf>) {
    *CONFIG_PATH_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = path;
}

/// Distinguishes "no config file" (normal fresh install) from "config file
/// exists but cannot be used" (must never be silently treated as a fresh
/// install — that is how a configured library turns into an empty default
/// one and users report "all my skills are gone", issue #228 review).
#[derive(Debug)]
enum ConfigState {
    Missing,
    Valid(RepoPathConfig),
    Invalid(String),
}

fn load_config_state_from(path: &Path) -> ConfigState {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return ConfigState::Missing,
        Err(err) => {
            return ConfigState::Invalid(format!("cannot read {}: {err}", path.display()));
        }
    };
    match serde_json::from_str(&raw) {
        Ok(config) => ConfigState::Valid(config),
        Err(err) => ConfigState::Invalid(format!("corrupt JSON in {}: {err}", path.display())),
    }
}

fn load_config_state() -> ConfigState {
    load_config_state_from(&config_file_path())
}

fn load_config() -> RepoPathConfig {
    match load_config_state() {
        ConfigState::Valid(config) => config,
        ConfigState::Missing | ConfigState::Invalid(_) => RepoPathConfig::default(),
    }
}

fn save_config(config: &RepoPathConfig) -> Result<()> {
    let path = config_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(config)?)?;
    Ok(())
}

fn normalize_path(raw: &str) -> Result<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("Path cannot be empty"));
    }

    let expanded = if trimmed == "~" {
        dirs::home_dir().ok_or_else(|| anyhow!("Cannot determine home directory"))?
    } else if trimmed.starts_with("~/") || trimmed.starts_with("~\\") {
        dirs::home_dir()
            .ok_or_else(|| anyhow!("Cannot determine home directory"))?
            .join(&trimmed[2..])
    } else {
        PathBuf::from(trimmed)
    };

    if !expanded.is_absolute() {
        return Err(anyhow!("Central repository path must be absolute"));
    }

    let mut normalized = PathBuf::new();
    for component in expanded.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

pub fn configured_base_dir() -> Option<PathBuf> {
    load_config()
        .repo_path
        .and_then(|path| normalize_path(&path).ok())
}

/// Where the library's data actually lives right now.
///
/// This is the **Live Location** (see ADR 0001), not the location the user asked
/// for. The two differ while a migration is pending: `repo_path` already names
/// the requested destination, but the data has not moved yet, so it is still at
/// the source. Resolving that here — rather than at the call site — keeps every
/// path derived from `base_dir()` (skills, cache, logs, the database, and the
/// write lock) on the directory that actually holds the library.
///
/// Getting this wrong is the bug behind #449/#469: following `repo_path`
/// immediately made the running session split across two locations, and the
/// first `RepoLock` created the destination and wrote its lock file into it, so
/// the next launch's migration saw a non-empty target and refused it forever.
pub fn base_dir() -> PathBuf {
    if let Some(path) = BASE_DIR_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .clone()
    {
        return path;
    }

    live_base_dir(&load_config())
}

/// The location a session should run against, given the stored config.
fn live_base_dir(config: &RepoPathConfig) -> PathBuf {
    if let Some(source) = &config.pending_migration_from {
        if let Ok(path) = normalize_path(source) {
            if path.is_dir() {
                return path;
            }
        }
    }
    requested_base_dir(config)
}

/// The location the user asked for, whether or not the data has moved there yet
/// (the **Requested Path**).
fn requested_base_dir(config: &RepoPathConfig) -> PathBuf {
    config
        .repo_path
        .as_deref()
        .and_then(|raw| normalize_path(raw).ok())
        .unwrap_or_else(default_base_dir)
}

/// Whether an explicit runtime base-dir override is active (CLI `--skills-root`
/// / `--path`). Startup migration is skipped when it is — the caller chose a
/// specific library and the app's shared pending-migration marker doesn't apply.
fn base_dir_override_active() -> bool {
    BASE_DIR_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
}

pub fn set_runtime_base_dir_override(path: Option<PathBuf>) {
    *BASE_DIR_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = path;
}

pub fn set_runtime_skills_dir_override(path: Option<PathBuf>) {
    *SKILLS_DIR_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = path;
}

#[cfg(test)]
pub(crate) fn set_test_base_dir_override(path: Option<PathBuf>) {
    set_runtime_base_dir_override(path);
    set_runtime_skills_dir_override(None);
}

pub(crate) const SKILLS_DIR_NAME: &str = "skills";
const SCENARIOS_DIR_NAME: &str = "scenarios";
const CACHE_DIR_NAME: &str = "cache";
const LOGS_DIR_NAME: &str = "logs";

/// The skeleton directories [`ensure_central_repo`] pre-creates, in one place:
/// the same list both creates them and recognises them as App-owned Debris. Two
/// hand-kept lists drift, and a drift here silently re-opens the #449/#469 loop
/// by making a debris-only target read as user data.
const SKELETON_DIR_NAMES: [&str; 4] = [
    SKILLS_DIR_NAME,
    SCENARIOS_DIR_NAME,
    CACHE_DIR_NAME,
    LOGS_DIR_NAME,
];

/// The CLI bridge directory, created by `cli_bridge` at the *home* location
/// rather than at `base_dir()` — deliberately, so a skill can name it without
/// asking where the library went. That also means it is always present in the
/// default library location, so without this exclusion migrating *back* to the
/// default could never succeed: the target would always read as non-empty.
const CLI_BRIDGE_DIR_NAME: &str = "bin";

pub fn skills_dir() -> PathBuf {
    if let Some(path) = SKILLS_DIR_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .clone()
    {
        return path;
    }
    base_dir().join(SKILLS_DIR_NAME)
}

/// Derive a stable per-skills-root state directory under the user's default base.
///
/// CLI's `--skills-root` lets agents operate on an external skills checkout
/// (e.g. a freshly cloned `my-skills`) without touching the app's default repo.
/// The manager still needs a home for its DB, scenarios, cache, and logs — but
/// putting that state inside the external checkout would pollute the user's
/// repo, and putting it in the parent directory would silently litter wherever
/// the user happened to clone. Instead, namespace the state under
/// `<default-base>/external/<sanitized-name>-<short-hash>/`, keyed by the
/// canonical path of the skills root so repeat invocations reuse the same DB.
pub fn external_base_dir(skills_root: &Path) -> PathBuf {
    // canonicalize() requires the path to exist. For not-yet-cloned targets we
    // still want a stable namespace, so fall back to absolutizing + lexically
    // normalizing the path. Without this, `./my-skills`, `my-skills`, and
    // `a/../my-skills` would hash to different namespaces despite resolving
    // to the same location.
    let canonical = match skills_root.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            let absolute = if skills_root.is_absolute() {
                skills_root.to_path_buf()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(skills_root))
                    .unwrap_or_else(|_| skills_root.to_path_buf())
            };
            lexically_normalize(&absolute)
        }
    };
    let name = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("external");
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let short_hash: String = digest.iter().take(5).map(|b| format!("{:02x}", b)).collect();
    default_base_dir()
        .join("external")
        .join(format!("{}-{}", sanitize_dir_name(name), short_hash))
}

/// Lexically normalize `.` and `..` segments without touching the filesystem.
/// `..` over a normal segment cancels it; `..` over a root or another `..`
/// is preserved (so we don't pretend to escape the filesystem root).
fn lexically_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {
                    // can't go above root — drop the `..`
                }
                _ => out.push(comp),
            },
            other => out.push(other),
        }
    }
    out.iter().collect()
}

fn sanitize_dir_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "external".to_string()
    } else {
        cleaned
    }
}

pub fn scenarios_dir() -> PathBuf {
    base_dir().join(SCENARIOS_DIR_NAME)
}

pub fn cache_dir() -> PathBuf {
    base_dir().join(CACHE_DIR_NAME)
}

pub fn logs_dir() -> PathBuf {
    base_dir().join(LOGS_DIR_NAME)
}

pub fn db_path() -> PathBuf {
    base_dir().join("skills-manager.db")
}

/// Whether the user wants the library moved, or wants to use one that is
/// already at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepoPathIntent {
    /// Copy the current library to the requested path, then switch to it.
    Migrate,
    /// The destination already holds a library; use it as-is, copying nothing.
    Adopt,
}

/// Record where the user wants the library, without moving it there.
///
/// Saving is a promise, not an action (ADR 0001): the data stays where it is and
/// the switch happens on the next launch. Crucially the runtime override is pinned
/// to the current Live Location, because [`base_dir`] otherwise resolves through
/// the config this function just rewrote — which is exactly how the running
/// session came to write into the destination and poison the next migration.
///
/// Returns the **requested** path (what the caller should report back), not the
/// location in use.
pub fn set_base_dir_override(
    path: Option<String>,
    intent: RepoPathIntent,
) -> Result<PathBuf> {
    let current = base_dir();
    let mut config = load_config();

    // The actual on-disk data location can differ from `current` when the user
    // already changed the path once but hasn't restarted yet — `current` then
    // reflects the unsatisfied future target stored in `repo_path`, while the
    // data still sits at `pending_migration_from`. Track the true location so
    // multiple changes before restart still migrate from the right source.
    let data_location = match &config.pending_migration_from {
        Some(src) => match normalize_path(src) {
            Ok(path) if path.is_dir() => path,
            _ => current.clone(),
        },
        None => current.clone(),
    };

    let (next, persist_repo_path) = match path {
        Some(raw) => (normalize_path(&raw)?, true),
        None => (default_base_dir(), false),
    };

    config.repo_path = if persist_repo_path {
        Some(next.to_string_lossy().to_string())
    } else {
        None
    };
    config.pending_migration_from = if intent == RepoPathIntent::Adopt {
        // The library is already at the destination — nothing to move. Clearing
        // the marker is what makes the next launch land on it directly.
        None
    } else if next != data_location {
        Some(data_location.to_string_lossy().to_string())
    } else {
        None
    };
    save_config(&config)?;

    // Keep this session on the data it already has open.
    //
    // For a Migrate the pending marker alone does that: `base_dir()` resolves
    // the marker back to the source, so no override is needed — which matters,
    // because an active override also disables migration in `ensure_central_repo`
    // and the CLI's `repo set-path` relies on the *next* process migrating
    // immediately. For an Adopt the marker is gone, so `base_dir()` would jump to
    // the destination while the database handle and every open path still refer
    // to the old location — the split-session half of #449/#469. Pin only when
    // the resolution would otherwise move out from under the session.
    if live_base_dir(&config) != data_location {
        set_runtime_base_dir_override(Some(data_location));
    }

    Ok(next)
}

/// Abandon a pending switch: keep using the library the session is on, and stop
/// intending to move anywhere. Distinct from [`set_base_dir_override`] with
/// `None`, which *does* intend a move (back to the default location).
///
/// "Stay here" is expressed as a preference for the current Live Location. The
/// pending marker alone is not enough to recover it: after an adoption there is
/// none (nothing is migrated), so clearing the marker and leaving `repo_path`
/// pointing at the destination would silently discard the running library.
/// Written as a preference for the Live Location rather than an absolute path so
/// that staying on the default location keeps tracking the default (no stored
/// path, so it still follows a future change of `$HOME`).
pub fn cancel_pending_migration() -> Result<()> {
    let mut config = load_config();
    let data_location = base_dir();
    config.pending_migration_from = None;
    config.repo_path = if paths_are_same_dir(&data_location, &default_base_dir()) {
        None
    } else {
        Some(data_location.to_string_lossy().to_string())
    };
    save_config(&config)
}

/// Where the library is headed on the next launch, if that differs from where it
/// is now — whether the user asked for a move or to adopt a library already
/// there. Comparing the two facts beats tracking which intent produced them:
/// the UI only ever needs "are these the same place?", and a pending marker
/// cannot express a pending adoption (nothing is migrated).
pub fn pending_switch_target() -> Option<PathBuf> {
    let config = load_config();
    let live = base_dir();
    let requested = requested_base_dir(&config);
    (requested != live).then_some(requested)
}

/// What a candidate destination turns out to hold.
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TargetInspection {
    /// Safe to migrate into (absent, or App-owned Debris only).
    Empty { requested_path: String },
    /// An existing Skills Manager library.
    ExistingLibrary {
        requested_path: String,
        skill_count: usize,
    },
    /// Someone else's data. Neither migratable nor adoptable here.
    NotEmpty { requested_path: String },
}

/// Classify a destination the user picked, without touching it.
///
/// Adoption of foreign skill directories is deliberately out of scope (ADR
/// 0001): we only recognise our own library, so anything else is refused with
/// the user still at the wheel.
///
/// The normalised path is returned so the caller can save exactly what was
/// inspected (expanding `~`, collapsing `..`), rather than re-normalising later
/// and risking a different result.
pub fn inspect_target(raw: &str) -> Result<TargetInspection> {
    let path = normalize_path(raw)?;
    let requested_path = path.to_string_lossy().to_string();
    if !target_has_user_data(&path)? {
        return Ok(TargetInspection::Empty { requested_path });
    }
    if path.join("skills-manager.db").is_file() {
        // Only offer to adopt a library that actually holds skills. A database
        // with none is either debris from a failed relocation or a brand-new
        // empty library; offering it as "use this library" would switch the user
        // onto an empty one, which reads as data loss. An unreadable database is
        // likewise not something to hand over to.
        if let Some(skill_count) = read_library_skill_count(&path) {
            if skill_count > 0 {
                return Ok(TargetInspection::ExistingLibrary {
                    requested_path,
                    skill_count,
                });
            }
        }
    }
    Ok(TargetInspection::NotEmpty { requested_path })
}

/// Skill count from an existing library's database, if it can be read at all.
///
/// `None` means "cannot tell" — missing, corrupt, or a schema this build does not
/// understand. Callers must treat that as not-adoptable, never as zero skills.
fn read_library_skill_count(base: &Path) -> Option<usize> {
    let db = base.join("skills-manager.db");
    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM skills", [], |row| row.get(0))
        .ok()?;
    Some(count.max(0) as usize)
}

/// Names the operating system, not the user, drops into a directory: Finder view
/// state, Windows thumbnail and folder config, AppleDouble resource forks.
///
/// None of them is user data, and a folder the user has merely *looked at* in
/// Finder has a `.DS_Store` — so without this an otherwise empty destination was
/// un-migratable on macOS, and the pending move retried forever.
fn is_os_metadata_name(name: &str) -> bool {
    matches!(name, ".DS_Store" | "desktop.ini" | "Thumbs.db" | ".localized")
        || name.starts_with("._")
}

/// Whether `dir` contains nothing but the OS's own metadata files, and so counts
/// as empty for migration purposes.
fn dir_holds_only_os_metadata(dir: &Path) -> Result<bool> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Ok(false);
        };
        if !is_os_metadata_name(name) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether `entry` is something the app itself leaves in a library location
/// before any migration runs: the write lock file, one of the bare skeleton
/// directories, or the CLI bridge directory. OS metadata never counts as
/// content either.
///
/// Every other name counts as content — deliberately including
/// `skills-manager.db` and `.secret.key`, which this app also creates. Those
/// carry the library's own state, so treating them as debris risks overwriting
/// real data, the failure #252 exists to prevent. The cost is that a target
/// polluted with them is refused rather than healed; the save-time prompt tells
/// the user to choose an empty folder, which is the safe resolution.
///
/// The lock file is exempt as a *file*; a skeleton name is exempt only as a
/// still-empty *directory*. A regular file that happens to share a skeleton's
/// name is not ours, and neither is a directory with anything in it.
fn is_app_owned_debris(entry: &fs::DirEntry) -> Result<bool> {
    let name = entry.file_name();
    let Some(name) = name.to_str() else {
        // An undecodable name is something we did not write.
        return Ok(false);
    };
    if is_os_metadata_name(name) {
        return Ok(true);
    }
    let file_type = entry.file_type()?;
    if name == crate::core::repo_lock::LOCK_FILE_NAME {
        return Ok(file_type.is_file());
    }
    if SKELETON_DIR_NAMES.contains(&name) {
        // A skeleton holding only OS metadata is still a bare skeleton.
        return Ok(file_type.is_dir() && dir_holds_only_os_metadata(&entry.path())?);
    }
    if name == CLI_BRIDGE_DIR_NAME {
        // The bridge is debris only while it holds nothing but its own files. A
        // `bin/` with anything else in it is the user's, and must block the move.
        return Ok(file_type.is_dir() && bridge_dir_is_owned(&entry.path())?);
    }
    Ok(false)
}

/// Whether a `bin/` directory contains nothing but CLI-bridge files.
fn bridge_dir_is_owned(dir: &Path) -> Result<bool> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Ok(false);
        };
        if is_os_metadata_name(name) && entry.file_type()?.is_file() {
            continue;
        }
        if !crate::core::cli_bridge::is_bridge_owned_file_name(name) || !entry.file_type()?.is_file() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether `path` holds anything the user would miss if it were overwritten.
///
/// A location containing only App-owned Debris is still an Empty Target, so it
/// stays migratable. The whitelist is fixed and membership is decided per entry
/// kind (see [`is_app_owned_debris`]) — deliberately *not* a heuristic, because
/// mistaking real data for debris would destroy it.
fn target_has_user_data(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    for entry in fs::read_dir(path)? {
        if !is_app_owned_debris(&entry?)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Copy a whole library tree, preserving symbolic links.
///
/// Deliberately not shared with `sync_engine`'s payload copy, which serves the
/// opposite need: that one copies a single skill's contents and skips `.git`,
/// while a library move must carry `skills/.git` (the backup repository). Two
/// callers, two jobs — the shared name was the only thing they had in common.
///
/// Links are recreated, never followed. Following them failed outright for a
/// directory link ("the source path is neither a regular file nor a symlink to a
/// regular file") and silently turned a file link into a real file. A directory
/// link aborting the copy also aborted the whole relocation, putting the user
/// back on the retry-forever loop (#449/#469) whenever the library contained one.
fn copy_library_tree(source: &Path, target: &Path) -> Result<()> {
    for entry in WalkDir::new(source) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        // Never carry the live lock into the destination: it names *this*
        // process's in-flight operation and is meaningless in the new location,
        // where a fresh one is created on demand. Leaving it behind would also
        // re-introduce the very debris that blocked migration (#449/#469).
        if relative == Path::new(crate::core::repo_lock::LOCK_FILE_NAME) {
            continue;
        }
        let destination = target.join(relative);
        let file_type = entry.file_type();
        if file_type.is_symlink() {
            copy_link(entry.path(), &destination, source, target)?;
            continue;
        }
        if file_type.is_dir() {
            fs::create_dir_all(&destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(entry.path(), &destination).with_context(|| {
            format!(
                "Failed to copy {} to {}",
                entry.path().display(),
                destination.display()
            )
        })?;
    }
    Ok(())
}

/// Recreate the link at `destination` without following it.
///
/// An absolute target that pointed inside the old tree is re-based onto the new
/// one, so a link aimed at another part of the library keeps working. A relative
/// target is left alone: the whole tree moves together, so it still resolves.
fn copy_link(link: &Path, destination: &Path, source_root: &Path, target_root: &Path) -> Result<()> {
    let raw = fs::read_link(link)
        .with_context(|| format!("Failed to read symlink {}", link.display()))?;
    let target = match raw.strip_prefix(source_root) {
        Ok(relative) if raw.is_absolute() => target_root.join(relative),
        _ => raw.clone(),
    };
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    // A dangling link reports "not a directory", which is the best we can do and
    // still preserves the link rather than replacing it with nothing.
    if target.is_dir() {
        create_dir_link(&target, destination)
    } else {
        create_file_link(&target, destination)
    }
}

pub(crate) fn create_dir_link(target: &Path, link: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).with_context(|| {
            format!("Failed to create symlink {} -> {}", link.display(), target.display())
        })
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(target, link).is_err() {
            // No SeCreateSymbolicLinkPrivilege is the common case; a junction
            // needs none on local NTFS and is equivalent for our purposes.
            junction::create(target, link).with_context(|| {
                format!("Failed to create junction {} -> {}", link.display(), target.display())
            })?;
        }
        Ok(())
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        anyhow::bail!("directory links are unsupported on this platform")
    }
}

pub(crate) fn create_file_link(target: &Path, link: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).with_context(|| {
            format!("Failed to create symlink {} -> {}", link.display(), target.display())
        })
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).with_context(|| {
            format!("Failed to create symlink {} -> {}", link.display(), target.display())
        })
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        anyhow::bail!("file links are unsupported on this platform")
    }
}

/// Whether two paths resolve to the same directory. Falls back to a lexical
/// comparison when either side can't be canonicalized (e.g. the target does not
/// exist yet), so a purely cosmetic difference (case, `8.3` names, a symlink)
/// isn't mistaken for a real relocation.
fn paths_are_same_dir(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// What the caller should do after attempting a pending central-repo move.
#[derive(Debug)]
enum MigrationOutcome {
    /// No move was pending, or it completed. Run against the configured base.
    Proceed,
    /// The move could not complete safely; the intact library still lives at
    /// this path. Run this session against it and retry on the next launch.
    UseSource(PathBuf),
}

/// Try to satisfy a pending central-repository relocation.
///
/// This runs before the logger, the panic hook, and the window exist (see
/// `run()` in lib.rs), so it must never return an error that would panic the
/// process into a windowless death (#252). Every failure instead records a
/// startup warning + a deferred log line and falls back to the source, where
/// the user's data is known to be intact. It mutates `config` in place but
/// does NOT persist it — the caller saves once, which also keeps this unit
/// testable without touching the real config file.
fn migrate_repo_if_needed(config: &mut RepoPathConfig, target: &Path) -> MigrationOutcome {
    let Some(source_raw) = config.pending_migration_from.clone() else {
        return MigrationOutcome::Proceed;
    };
    let source = match normalize_path(&source_raw) {
        Ok(path) => path,
        Err(err) => {
            // The stored path is unusable, so the move can never proceed. Drop
            // the marker to stop retrying every launch and run against target.
            record_startup_error(format!(
                "central repo: pending migration source {source_raw:?} is invalid ({err}); dropping it"
            ));
            config.pending_migration_from = None;
            return MigrationOutcome::Proceed;
        }
    };

    // Nothing left to move: the source is gone (moved already, or the old
    // location was removed), or source and target are the same directory.
    // Compare canonically, not just lexically — on a case-insensitive volume
    // `D:\Skills` and `d:\skills` are one directory (likewise 8.3 vs long, or a
    // symlink), and a lexical mismatch would otherwise loop forever on
    // `migration_incomplete`, telling the user to empty their own library.
    if !source.exists() || paths_are_same_dir(&source, target) {
        config.pending_migration_from = None;
        return MigrationOutcome::Proceed;
    }

    // A target nested inside the source can never be a valid destination.
    if target.starts_with(&source) {
        record_startup_error(format!(
            "central repo: migration target {} is inside source {}; keeping data at the source",
            target.display(),
            source.display()
        ));
        push_startup_warning("migration_incomplete");
        return MigrationOutcome::UseSource(source);
    }

    // Only ever move into an absent/empty target — never blind-merge. A
    // non-empty target is either a real library we must not overwrite or debris
    // from a failed attempt we cannot tell apart; keeping the user on their
    // intact source is lossless, overwriting is not. A fresh target also means
    // the recursive copy only ever creates new files, so it can never hit the
    // read-only git pack files that overwriting bricked startup on (#252).
    //
    // "Empty" ignores App-owned Debris (ADR 0001): the lock file and the bare
    // skeleton dirs this app creates on its own. Without that exclusion a single
    // stray lock file — written by any `RepoLock` between the user saving a new
    // path and the next launch — made the move fail forever (#449/#469).
    let target_has_data = match target_has_user_data(target) {
        Ok(has_data) => has_data,
        Err(err) => {
            record_startup_error(format!(
                "central repo: cannot inspect migration target {} ({err}); keeping data at source {}",
                target.display(),
                source.display()
            ));
            push_startup_warning("migration_incomplete");
            return MigrationOutcome::UseSource(source);
        }
    };
    if target_has_data {
        record_startup_error(format!(
            "central repo: migration target {} is not empty; keeping data at source {}",
            target.display(),
            source.display()
        ));
        push_startup_warning("migration_incomplete");
        return MigrationOutcome::UseSource(source);
    }

    if let Some(parent) = target.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            record_startup_error(format!(
                "central repo: cannot create migration target parent {} ({err}); keeping data at source {}",
                parent.display(),
                source.display()
            ));
            push_startup_warning("migration_incomplete");
            return MigrationOutcome::UseSource(source);
        }
    }

    // Same volume: an atomic rename moves the whole tree cheaply. Cross volume
    // (or a rename the OS refuses): copy into the empty target. Because the
    // target is empty, no existing file is ever overwritten.
    if fs::rename(&source, target).is_err() {
        if let Err(err) = copy_library_tree(&source, target) {
            record_startup_error(format!(
                "central repo: migration copy from {} to {} failed ({err:#}); keeping data at source",
                source.display(),
                target.display()
            ));
            push_startup_warning("migration_incomplete");
            return MigrationOutcome::UseSource(source);
        }
    }

    config.pending_migration_from = None;
    // Tell the caller a move happened, so the agent-side links that pointed into
    // the old location can be re-pointed once the store is available.
    record_migration(&source, target);
    MigrationOutcome::Proceed
}

pub fn ensure_central_repo() -> Result<()> {
    // A config file that exists but cannot be used means the app is about to
    // run against the default location even though the user configured (and
    // populated) another one. Never let that pass silently — it presents as
    // "the library was rebuilt empty, all skills lost" (#228 review).
    let mut config = match load_config_state() {
        ConfigState::Valid(config) => {
            if let Some(raw) = config.repo_path.as_deref() {
                if let Err(err) = normalize_path(raw) {
                    log::error!(
                        "central repo: configured repo_path {raw:?} is invalid ({err}); \
                         falling back to the default location"
                    );
                    push_startup_warning("repo_path_invalid");
                }
            }
            config
        }
        ConfigState::Missing => RepoPathConfig::default(),
        ConfigState::Invalid(detail) => {
            log::error!(
                "central repo: config is unreadable ({detail}); \
                 falling back to the default location"
            );
            push_startup_warning("config_unreadable");
            RepoPathConfig::default()
        }
    };

    // Only auto-migrate the app's own config-driven base. When a runtime base
    // override is active (CLI `--skills-root` / `--path`), the pending marker in
    // the shared config belongs to a different library and must not be applied
    // to — or override — the explicitly chosen root. The app's own startup never
    // sets an override before this point, so the #252 path is unaffected.
    if !base_dir_override_active() {
        let pending_before = config.pending_migration_from.clone();
        // The migration's target is the *requested* path, not `base_dir()`: the
        // latter now resolves to the source while a migration is pending (ADR
        // 0001), so using it here would ask the move to overwrite its own source.
        let target = requested_base_dir(&config);
        let outcome = migrate_repo_if_needed(&mut config, &target);
        if config.pending_migration_from != pending_before {
            if let Err(err) = save_config(&config) {
                record_startup_error(format!(
                    "central repo: failed to persist migration state ({err}); it may retry next launch"
                ));
            }
        }
        if let MigrationOutcome::UseSource(source) = outcome {
            // Run this whole session against the intact source library. The
            // `base_dir()` resolution above already does this while the pending
            // marker survives, but pin it explicitly too: if the marker is
            // cleared later this session must not drift onto the target.
            set_runtime_base_dir_override(Some(source));
        }
    }
    // Re-resolve: a fallback override above may have changed the base.
    let current_base = base_dir();

    // Legacy `.agent-skills` migration must run before create_dir_all below:
    // it renames entries into `current_base` and skips ones that already
    // exist, so pre-created empty dirs would silently swallow it (the old
    // ordering made this branch dead code).
    let legacy_path = dirs::home_dir().map(|home| home.join(".agent-skills"));
    if let Some(old_path) = legacy_path {
        if old_path.exists() && !current_base.join("skills").exists() {
            log::info!("Migrating from old path {:?}", old_path);
            fs::create_dir_all(&current_base)?;
            if let Ok(entries) = fs::read_dir(&old_path) {
                for entry in entries.flatten() {
                    let dest = current_base.join(entry.file_name());
                    if !dest.exists() {
                        let _ = fs::rename(entry.path(), &dest);
                    }
                }
            }
        }
    }

    let dirs = [skills_dir(), scenarios_dir(), cache_dir(), logs_dir()];
    for d in &dirs {
        fs::create_dir_all(d)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── migrate_repo_if_needed (#252) ──

    fn config_migrating(source: &Path, target: &Path) -> RepoPathConfig {
        RepoPathConfig {
            repo_path: Some(target.to_string_lossy().to_string()),
            pending_migration_from: Some(source.to_string_lossy().to_string()),
        }
    }

    #[test]
    fn migration_into_empty_target_moves_and_clears_marker() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap(); // exists but empty
        fs::create_dir_all(src.path().join("skills")).unwrap();
        fs::write(src.path().join("skills/s.md"), b"skill").unwrap();

        let mut config = config_migrating(src.path(), dst.path());
        let outcome = migrate_repo_if_needed(&mut config, dst.path());

        assert!(matches!(outcome, MigrationOutcome::Proceed));
        assert_eq!(config.pending_migration_from, None);
        assert_eq!(fs::read(dst.path().join("skills/s.md")).unwrap(), b"skill");
    }

    #[test]
    fn migration_into_nonempty_target_keeps_source_and_marker() {
        // The whole point of #252's safety: never blind-merge over a
        // non-empty target (real data or failed-attempt debris we can't tell
        // apart). Fall back to the intact source and keep retrying.
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::write(src.path().join("a.txt"), b"src").unwrap();
        fs::write(dst.path().join("existing.txt"), b"dst-data").unwrap();

        let mut config = config_migrating(src.path(), dst.path());
        let outcome = migrate_repo_if_needed(&mut config, dst.path());

        match outcome {
            MigrationOutcome::UseSource(p) => {
                assert_eq!(p, normalize_path(&src.path().to_string_lossy()).unwrap());
            }
            _ => panic!("expected UseSource for a non-empty target"),
        }
        assert!(config.pending_migration_from.is_some(), "marker kept for retry");
        assert_eq!(fs::read(dst.path().join("existing.txt")).unwrap(), b"dst-data");
        assert_eq!(fs::read(src.path().join("a.txt")).unwrap(), b"src");
    }

    // ── App-owned Debris is not user data (ADR 0001, #449/#469) ──

    /// The bug: any `RepoLock` between the user saving a new path and the next
    /// launch wrote the lock file into the destination, so the migration saw a
    /// non-empty target and refused it — every launch, forever.
    #[test]
    fn migration_into_target_holding_only_the_lock_file_moves() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("skills")).unwrap();
        fs::write(src.path().join("skills/s.md"), b"skill").unwrap();
        fs::write(
            dst.path().join(crate::core::repo_lock::LOCK_FILE_NAME),
            b"pid=4321\noperation=auto backup\n",
        )
        .unwrap();

        let mut config = config_migrating(src.path(), dst.path());
        let outcome = migrate_repo_if_needed(&mut config, dst.path());

        assert!(matches!(outcome, MigrationOutcome::Proceed), "got {outcome:?}");
        assert_eq!(config.pending_migration_from, None, "migration completed");
        assert!(dst.path().join("skills/s.md").exists());
    }

    /// The other half of the same bug: `ensure_central_repo` pre-creates the
    /// skeleton dirs, so a target that only ever held those must still move.
    #[test]
    fn migration_into_target_holding_only_empty_skeletons_moves() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("skills")).unwrap();
        fs::write(src.path().join("skills/s.md"), b"skill").unwrap();
        for dir in ["skills", "scenarios", "cache", "logs"] {
            fs::create_dir_all(dst.path().join(dir)).unwrap();
        }

        let mut config = config_migrating(src.path(), dst.path());
        let outcome = migrate_repo_if_needed(&mut config, dst.path());

        assert!(matches!(outcome, MigrationOutcome::Proceed), "got {outcome:?}");
        assert!(dst.path().join("skills/s.md").exists());
    }

    /// A skeleton dir that has anything in it is user data, not debris. This is
    /// the guard that keeps the debris whitelist from becoming a way to
    /// overwrite a real library (the #252 failure mode).
    #[test]
    fn migration_into_target_with_data_inside_a_skeleton_dir_is_refused() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::write(src.path().join("a.txt"), b"src").unwrap();
        fs::create_dir_all(dst.path().join("skills")).unwrap();
        fs::write(dst.path().join("skills/precious.md"), b"user-data").unwrap();

        let mut config = config_migrating(src.path(), dst.path());
        let outcome = migrate_repo_if_needed(&mut config, dst.path());

        assert!(matches!(outcome, MigrationOutcome::UseSource(_)), "got {outcome:?}");
        assert_eq!(
            fs::read(dst.path().join("skills/precious.md")).unwrap(),
            b"user-data",
            "the refused target must be left untouched"
        );
    }

    #[test]
    fn target_has_user_data_ignores_only_app_owned_entries() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!target_has_user_data(dir.path()).unwrap(), "empty dir");

        // The write lock, and the bare skeleton dirs, are debris.
        fs::write(dir.path().join(crate::core::repo_lock::LOCK_FILE_NAME), b"x").unwrap();
        for name in SKELETON_DIR_NAMES {
            fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        assert!(
            !target_has_user_data(dir.path()).unwrap(),
            "the lock file plus bare skeletons is debris"
        );

        // A populated skeleton dir is not.
        fs::write(dir.path().join(SKILLS_DIR_NAME).join("mine.md"), b"user").unwrap();
        assert!(
            target_has_user_data(dir.path()).unwrap(),
            "a populated skeleton dir is content"
        );
        fs::remove_file(dir.path().join(SKILLS_DIR_NAME).join("mine.md")).unwrap();

        // The database and key carry the library's own state, so neither is
        // debris: exempting them would risk overwriting a real library (#252).
        fs::write(dir.path().join("skills-manager.db"), b"db").unwrap();
        assert!(
            target_has_user_data(dir.path()).unwrap(),
            "the database is content, not debris"
        );
        fs::remove_file(dir.path().join("skills-manager.db")).unwrap();
        fs::write(dir.path().join(".secret.key"), b"key").unwrap();
        assert!(
            target_has_user_data(dir.path()).unwrap(),
            "the secret key is content, not debris"
        );

        // A skeleton *name* that is a plain file is not ours. An earlier revision
        // exempted any regular file by these names, silently ignoring user data.
        let lone = tempfile::tempdir().unwrap();
        fs::write(lone.path().join(CACHE_DIR_NAME), b"a file, not a dir").unwrap();
        assert!(
            target_has_user_data(lone.path()).unwrap(),
            "a file named like a skeleton must not be exempt"
        );
    }

    // ── Live Location vs Requested Path (ADR 0001) ──

    /// While a migration is pending, the session must keep running against the
    /// data. Following `repo_path` immediately is what split a session across
    /// two locations and let the lock file poison the destination.
    #[test]
    fn pending_migration_keeps_the_session_on_the_source() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let config = config_migrating(src.path(), dst.path());

        assert_eq!(live_base_dir(&config), src.path());
        assert_eq!(requested_base_dir(&config), dst.path());
    }

    #[test]
    fn without_a_pending_migration_the_requested_path_is_live() {
        let dst = tempfile::tempdir().unwrap();
        let config = RepoPathConfig {
            repo_path: Some(dst.path().to_string_lossy().to_string()),
            pending_migration_from: None,
        };

        assert_eq!(live_base_dir(&config), dst.path());
    }

    /// A source that no longer exists cannot be where the data is; falling back
    /// to the requested path avoids pinning a session to a deleted directory.
    #[test]
    fn pending_migration_with_a_vanished_source_falls_back_to_the_target() {
        let dst = tempfile::tempdir().unwrap();
        let gone = dst.path().join("removed");
        let config = config_migrating(&gone, dst.path());

        assert_eq!(live_base_dir(&config), dst.path());
    }

    #[test]
    fn inspect_target_classifies_empty_library_and_foreign_data() {
        // Absent and debris-only destinations are migratable.
        let fresh = tempfile::tempdir().unwrap();
        let missing = fresh.path().join("missing");
        assert_eq!(
            inspect_target(&missing.to_string_lossy()).unwrap(),
            TargetInspection::Empty {
                requested_path: missing.to_string_lossy().to_string(),
            }
        );
        let debris = tempfile::tempdir().unwrap();
        fs::write(
            debris.path().join(crate::core::repo_lock::LOCK_FILE_NAME),
            b"x",
        )
        .unwrap();
        fs::create_dir_all(debris.path().join("skills")).unwrap();
        assert_eq!(
            inspect_target(&debris.path().to_string_lossy()).unwrap(),
            TargetInspection::Empty {
                requested_path: debris.path().to_string_lossy().to_string(),
            }
        );

        // A real library is recognised as such.
        let library = tempfile::tempdir().unwrap();
        let db = library.path().join("skills-manager.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE skills (id TEXT PRIMARY KEY, name TEXT NOT NULL, \
             description TEXT, source_type TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO skills (id, name, source_type) VALUES ('1', 'a', 'local'), ('2', 'b', 'local')",
            [],
        )
        .unwrap();
        drop(conn);
        assert_eq!(
            inspect_target(&library.path().to_string_lossy()).unwrap(),
            TargetInspection::ExistingLibrary {
                requested_path: library.path().to_string_lossy().to_string(),
                skill_count: 2,
            }
        );

        // Someone else's data is neither migratable nor adoptable here.
        let foreign = tempfile::tempdir().unwrap();
        fs::write(foreign.path().join("notes.txt"), b"mine").unwrap();
        assert_eq!(
            inspect_target(&foreign.path().to_string_lossy()).unwrap(),
            TargetInspection::NotEmpty {
                requested_path: foreign.path().to_string_lossy().to_string(),
            }
        );

        // An app database with no skills is either debris from a failed
        // relocation or a brand-new empty library. Offering it for adoption would
        // switch the user onto an empty library, which reads as data loss.
        let empty_library = tempfile::tempdir().unwrap();
        let empty_db = empty_library.path().join("skills-manager.db");
        let conn = rusqlite::Connection::open(&empty_db).unwrap();
        conn.execute_batch("CREATE TABLE skills (id TEXT PRIMARY KEY, name TEXT NOT NULL);")
            .unwrap();
        drop(conn);
        assert_eq!(
            inspect_target(&empty_library.path().to_string_lossy()).unwrap(),
            TargetInspection::NotEmpty {
                requested_path: empty_library.path().to_string_lossy().to_string(),
            },
            "an empty library must not be offered for adoption"
        );

        // A database we cannot read is likewise not something to hand over to.
        let unreadable = tempfile::tempdir().unwrap();
        fs::write(
            unreadable.path().join("skills-manager.db"),
            b"not a database",
        )
        .unwrap();
        assert_eq!(
            inspect_target(&unreadable.path().to_string_lossy()).unwrap(),
            TargetInspection::NotEmpty {
                requested_path: unreadable.path().to_string_lossy().to_string(),
            },
            "an unreadable library must not be offered for adoption"
        );
    }

    /// The lock names *this* process's in-flight operation, so it must not be
    /// carried into the destination — that would re-create the poisoned target.
    #[test]
    fn copy_library_tree_leaves_the_lock_file_behind() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::write(src.path().join("keep.md"), b"keep").unwrap();
        fs::write(
            src.path().join(crate::core::repo_lock::LOCK_FILE_NAME),
            b"pid=1\n",
        )
        .unwrap();

        let target = dst.path().join("out");
        copy_library_tree(src.path(), &target).unwrap();

        assert!(target.join("keep.md").exists());
        assert!(
            !target.join(crate::core::repo_lock::LOCK_FILE_NAME).exists(),
            "the lock file must not be copied"
        );
    }

    /// Links must be recreated, not followed: following a directory link aborted
    /// the whole copy (and so the relocation), and following a file link turned
    /// it into a real file.
    #[cfg(unix)]
    #[test]
    fn copy_library_tree_preserves_symlinks() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        fs::create_dir_all(src.path().join("skills/real")).unwrap();
        fs::write(src.path().join("skills/real/SKILL.md"), b"x").unwrap();
        // Absolute link to another part of the library.
        std::os::unix::fs::symlink(
            src.path().join("skills/real"),
            src.path().join("skills/link-in"),
        )
        .unwrap();
        // Link to something outside the library.
        fs::write(outside.path().join("ext.md"), b"e").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("ext.md"),
            src.path().join("link-out.md"),
        )
        .unwrap();
        // Link to a file inside the library.
        std::os::unix::fs::symlink(
            src.path().join("skills/real/SKILL.md"),
            src.path().join("link-in-file.md"),
        )
        .unwrap();

        let target = dst.path().join("out");
        copy_library_tree(src.path(), &target).unwrap();

        let link_in = target.join("skills/link-in");
        assert!(
            fs::symlink_metadata(&link_in).unwrap().file_type().is_symlink(),
            "an in-library directory link must stay a link"
        );
        assert_eq!(fs::read_link(&link_in).unwrap(), target.join("skills/real"));
        assert_eq!(fs::read_to_string(link_in.join("SKILL.md")).unwrap(), "x");
        assert_eq!(
            fs::read_link(target.join("link-out.md")).unwrap(),
            outside.path().join("ext.md"),
            "an external link keeps pointing where it did"
        );
        assert!(
            fs::symlink_metadata(target.join("link-in-file.md"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "an in-library file link must stay a link, not become a file"
        );
    }

    /// End-to-end: a library containing a directory link used to abort the move,
    /// which is the retry-forever loop of #449/#469 in a different guise.
    ///
    /// The target is pre-seeded with debris so `fs::rename` fails and the copy
    /// path actually runs — otherwise same-volume relocation renames the tree and
    /// this test would pass without touching the code it is meant to guard.
    #[cfg(unix)]
    #[test]
    fn migration_of_a_library_containing_a_symlink_completes() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("skills/real")).unwrap();
        fs::write(src.path().join("skills/real/SKILL.md"), b"x").unwrap();
        std::os::unix::fs::symlink(
            src.path().join("skills/real"),
            src.path().join("skills/linked"),
        )
        .unwrap();
        // Debris only, so the target stays migratable while still existing.
        fs::write(
            dst.path().join(crate::core::repo_lock::LOCK_FILE_NAME),
            b"pid=1\n",
        )
        .unwrap();

        let mut config = config_migrating(src.path(), dst.path());
        let outcome = migrate_repo_if_needed(&mut config, dst.path());

        assert!(matches!(outcome, MigrationOutcome::Proceed), "got {outcome:?}");
        assert!(dst.path().join("skills/real/SKILL.md").exists());
        assert!(fs::symlink_metadata(dst.path().join("skills/linked"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    /// The reverse of the lock-file case: the CLI bridge directory sits in the
    /// default location on every machine, so migrating *back* to the default
    /// could never succeed without treating the app's own files as debris.
    #[test]
    fn target_holding_only_the_cli_bridge_is_migratable() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join(crate::core::cli_bridge::BRIDGE_BIN_NAME), b"binary").unwrap();
        fs::write(bin.join(crate::core::cli_bridge::BRIDGE_STAMP_NAME), b"1.40.0").unwrap();
        assert!(
            !target_has_user_data(dir.path()).unwrap(),
            "the bridge directory alone is debris"
        );

        fs::write(bin.join("notes.txt"), b"mine").unwrap();
        assert!(
            target_has_user_data(dir.path()).unwrap(),
            "a foreign file inside bin must block the move"
        );
    }

    /// The operating system drops metadata into any directory the user has looked
    /// at in Finder, so a destination can be otherwise empty and still hold one.
    /// Treating it as content made migrating back to the default impossible on
    /// macOS, and the pending move retried on every launch.
    #[test]
    fn target_holding_only_os_metadata_is_migratable() {
        let dir = tempfile::tempdir().unwrap();
        for name in [".DS_Store", "desktop.ini", "Thumbs.db"] {
            fs::write(dir.path().join(name), b"os junk").unwrap();
        }
        assert!(
            !target_has_user_data(dir.path()).unwrap(),
            "OS metadata alone is not user data"
        );

        // A skeleton holding only OS metadata is still a bare skeleton.
        let skeleton = dir.path().join("skills");
        fs::create_dir_all(&skeleton).unwrap();
        fs::write(skeleton.join(".DS_Store"), b"junk").unwrap();
        assert!(
            !target_has_user_data(dir.path()).unwrap(),
            "a skeleton with only OS metadata is still empty"
        );

        // A real file next to it still blocks the move.
        fs::write(skeleton.join("mine.md"), b"user").unwrap();
        assert!(
            target_has_user_data(dir.path()).unwrap(),
            "real content must still block the move"
        );
    }

    /// End to end: an empty default location that Finder has touched must still
    /// accept the migration.
    #[test]
    fn migration_into_a_target_holding_only_os_metadata_moves() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("skills")).unwrap();
        fs::write(src.path().join("skills/s.md"), b"skill").unwrap();
        fs::write(dst.path().join(".DS_Store"), b"finder junk").unwrap();

        let mut config = config_migrating(src.path(), dst.path());
        let outcome = migrate_repo_if_needed(&mut config, dst.path());

        assert!(matches!(outcome, MigrationOutcome::Proceed), "got {outcome:?}");
        assert!(dst.path().join("skills/s.md").exists());
    }

    // ── save-then-restart round trip (the #449/#469 report) ──

    /// A save must not move the session onto the destination. Before the fix,
    /// `base_dir()` followed `repo_path` the instant it was written, so any
    /// `RepoLock` in the same session created the destination and wrote its lock
    /// file there — poisoning the migration that was supposed to run next launch.
    #[test]
    fn saving_a_new_path_keeps_the_session_on_the_current_library() {
        let _guard = test_base_dir_lock();
        let home = tempfile::tempdir().unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        set_test_home_dir_override(Some(home.path().to_path_buf()));
        set_test_config_path_override(Some(cfg_dir.path().join(CONFIG_FILE_NAME)));
        set_runtime_base_dir_override(None);

        // Start on a populated default library.
        let live = home.path().join(".skills-manager");
        fs::create_dir_all(live.join("skills")).unwrap();
        fs::write(live.join("skills/keep.md"), b"skill").unwrap();
        assert_eq!(base_dir(), live);

        let target = home.path().join("moved");
        set_base_dir_override(
            Some(target.to_string_lossy().to_string()),
            RepoPathIntent::Migrate,
        )
        .unwrap();

        // The session must still be on the data, and the destination untouched.
        assert_eq!(base_dir(), live, "session moved before the migration");
        assert!(
            !target.exists(),
            "the destination must not be created before the migration"
        );

        // Now the restart: the migration runs and completes.
        set_runtime_base_dir_override(None);
        ensure_central_repo().unwrap();
        assert_eq!(base_dir(), target, "migration did not complete on restart");
        assert!(target.join("skills/keep.md").exists());

        set_test_config_path_override(None);
        set_test_home_dir_override(None);
        set_runtime_base_dir_override(None);
    }

    /// The end-to-end shape of the bug report: the destination picks up a lock
    /// file from a stray operation, and the migration must still succeed next
    /// launch instead of retrying forever.
    #[test]
    fn migration_succeeds_when_the_target_only_holds_app_debris() {
        let _guard = test_base_dir_lock();
        let home = tempfile::tempdir().unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        set_test_home_dir_override(Some(home.path().to_path_buf()));
        set_test_config_path_override(Some(cfg_dir.path().join(CONFIG_FILE_NAME)));
        set_runtime_base_dir_override(None);

        let live = home.path().join(".skills-manager");
        fs::create_dir_all(live.join("skills")).unwrap();
        fs::write(live.join("skills/keep.md"), b"skill").unwrap();

        let target = home.path().join("moved");
        set_base_dir_override(
            Some(target.to_string_lossy().to_string()),
            RepoPathIntent::Migrate,
        )
        .unwrap();

        // Whatever created this (an older build, a stray CLI run) left debris.
        fs::create_dir_all(target.join("skills")).unwrap();
        fs::write(
            target.join(crate::core::repo_lock::LOCK_FILE_NAME),
            b"pid=9\n",
        )
        .unwrap();

        set_runtime_base_dir_override(None);
        ensure_central_repo().unwrap();

        assert_eq!(base_dir(), target, "debris-only target blocked the migration");
        assert!(target.join("skills/keep.md").exists());
        assert!(!target.join("skills").is_empty());

        set_test_config_path_override(None);
        set_test_home_dir_override(None);
        set_runtime_base_dir_override(None);
    }

    /// A save that is never acted on must be abandonable, leaving the library
    /// exactly where it is.
    #[test]
    fn cancelling_a_pending_migration_keeps_the_library_in_place() {
        let _guard = test_base_dir_lock();
        let home = tempfile::tempdir().unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        set_test_home_dir_override(Some(home.path().to_path_buf()));
        set_test_config_path_override(Some(cfg_dir.path().join(CONFIG_FILE_NAME)));
        set_runtime_base_dir_override(None);

        let live = home.path().join(".skills-manager");
        fs::create_dir_all(live.join("skills")).unwrap();
        fs::write(live.join("skills/keep.md"), b"skill").unwrap();

        let target = home.path().join("moved");
        set_base_dir_override(
            Some(target.to_string_lossy().to_string()),
            RepoPathIntent::Migrate,
        )
        .unwrap();
        assert_eq!(pending_switch_target(), Some(target.clone()));

        cancel_pending_migration().unwrap();
        set_runtime_base_dir_override(None);
        ensure_central_repo().unwrap();

        assert_eq!(base_dir(), live, "cancel must keep the library where it is");
        assert!(pending_switch_target().is_none());
        assert!(!target.exists());

        set_test_config_path_override(None);
        set_test_home_dir_override(None);
        set_runtime_base_dir_override(None);
    }

    /// Cancelling must work even when nothing is pending — the shape left by an
    /// adoption. Clearing the marker and leaving `repo_path` on the destination
    /// would drop the running library and fall back to an empty default (#228).
    #[test]
    fn cancelling_after_an_adoption_keeps_the_adopted_library() {
        let _guard = test_base_dir_lock();
        let home = tempfile::tempdir().unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        set_test_home_dir_override(Some(home.path().to_path_buf()));
        set_test_config_path_override(Some(cfg_dir.path().join(CONFIG_FILE_NAME)));
        set_runtime_base_dir_override(None);

        let live = home.path().join(".skills-manager");
        fs::create_dir_all(live.join("skills")).unwrap();
        fs::write(live.join("skills/keep.md"), b"skill").unwrap();

        let other = home.path().join("other-library");
        fs::create_dir_all(other.join("skills")).unwrap();

        // Adopt, then restart into it, so the session runs on `other` with no
        // pending marker at all.
        set_base_dir_override(
            Some(other.to_string_lossy().to_string()),
            RepoPathIntent::Adopt,
        )
        .unwrap();
        set_runtime_base_dir_override(None);
        ensure_central_repo().unwrap();
        assert_eq!(base_dir(), other);

        cancel_pending_migration().unwrap();
        set_runtime_base_dir_override(None);
        ensure_central_repo().unwrap();

        assert_eq!(
            base_dir(),
            other,
            "cancelling must not strand the session on an empty default"
        );
        assert!(other.join("skills").exists());

        set_test_config_path_override(None);
        set_test_home_dir_override(None);
        set_runtime_base_dir_override(None);
    }

    /// Adopting the default location — what the UI's "reset to default" does when
    /// the default already holds a library. It must store "no path" rather than
    /// that path written out, or the UI would start calling the default custom.
    #[test]
    fn adopting_the_default_location_stores_no_path() {
        let _guard = test_base_dir_lock();
        let home = tempfile::tempdir().unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        set_test_home_dir_override(Some(home.path().to_path_buf()));
        set_test_config_path_override(Some(cfg_dir.path().join(CONFIG_FILE_NAME)));
        set_runtime_base_dir_override(None);

        let live = home.path().join("elsewhere");
        fs::create_dir_all(live.join("skills")).unwrap();
        set_runtime_base_dir_override(Some(live.clone()));

        set_base_dir_override(None, RepoPathIntent::Adopt).unwrap();

        let config = load_config();
        assert_eq!(config.repo_path, None, "the default must stay 'no path'");
        assert_eq!(config.pending_migration_from, None, "nothing to migrate");
        // This session keeps using the data it already has...
        assert_eq!(base_dir(), live);
        // ...and the next one resolves to the default.
        assert_eq!(live_base_dir(&config), home.path().join(".skills-manager"));

        set_test_config_path_override(None);
        set_test_home_dir_override(None);
        set_runtime_base_dir_override(None);
    }

    /// Adopting a library that is already at the destination switches there on
    /// the next launch without copying anything.
    #[test]
    fn adopting_an_existing_library_switches_without_copying() {
        let _guard = test_base_dir_lock();
        let home = tempfile::tempdir().unwrap();
        let cfg_dir = tempfile::tempdir().unwrap();
        set_test_home_dir_override(Some(home.path().to_path_buf()));
        set_test_config_path_override(Some(cfg_dir.path().join(CONFIG_FILE_NAME)));
        set_runtime_base_dir_override(None);

        let live = home.path().join(".skills-manager");
        fs::create_dir_all(live.join("skills")).unwrap();
        fs::write(live.join("skills/keep.md"), b"skill").unwrap();

        let other = home.path().join("other-library");
        fs::create_dir_all(other.join("skills")).unwrap();
        fs::write(other.join("skills/other.md"), b"other").unwrap();

        set_base_dir_override(
            Some(other.to_string_lossy().to_string()),
            RepoPathIntent::Adopt,
        )
        .unwrap();

        // Still running on the old data until the restart...
        assert_eq!(base_dir(), live);

        set_runtime_base_dir_override(None);
        ensure_central_repo().unwrap();

        // ...and now on the adopted library, whose contents are untouched.
        assert_eq!(base_dir(), other);
        assert!(other.join("skills/other.md").exists());
        assert!(
            !other.join("skills/keep.md").exists(),
            "adoption must not copy the old library in"
        );

        set_test_config_path_override(None);
        set_test_home_dir_override(None);
        set_runtime_base_dir_override(None);
    }

    #[test]
    #[cfg(unix)]
    fn migration_same_dir_via_symlink_clears_marker() {
        // A cosmetic path difference that resolves to the same directory (here
        // a symlink; on Windows, case / 8.3 names) must not be mistaken for a
        // real relocation — otherwise it loops forever on `migration_incomplete`
        // telling the user to empty their own library.
        let real = tempfile::tempdir().unwrap();
        fs::create_dir_all(real.path().join("skills")).unwrap();
        let link_parent = tempfile::tempdir().unwrap();
        let link = link_parent.path().join("aliased");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        let mut config = config_migrating(real.path(), &link);
        let outcome = migrate_repo_if_needed(&mut config, &link);

        assert!(matches!(outcome, MigrationOutcome::Proceed));
        assert_eq!(config.pending_migration_from, None, "same-dir move clears marker");
        // The real library is untouched.
        assert!(real.path().join("skills").exists());
    }

    #[test]
    fn migration_with_missing_source_clears_marker() {
        let dst = tempfile::tempdir().unwrap();
        let missing = dst.path().join("does-not-exist");
        let mut config = config_migrating(&missing, dst.path());

        let outcome = migrate_repo_if_needed(&mut config, dst.path());
        assert!(matches!(outcome, MigrationOutcome::Proceed));
        assert_eq!(config.pending_migration_from, None);
    }

    #[test]
    fn no_pending_migration_is_a_noop() {
        let dst = tempfile::tempdir().unwrap();
        let mut config = RepoPathConfig {
            repo_path: Some(dst.path().to_string_lossy().to_string()),
            pending_migration_from: None,
        };
        let outcome = migrate_repo_if_needed(&mut config, dst.path());
        assert!(matches!(outcome, MigrationOutcome::Proceed));
        assert_eq!(config.pending_migration_from, None);
    }

    #[test]
    fn copy_library_tree_copies_read_only_source_files() {
        // git pack files (.idx/.pack/.rev) are read-only. Copying them into a
        // fresh target must succeed — the #252 brick only happened when
        // OVERWRITING an existing read-only file, which migration now avoids by
        // only ever moving into an empty target.
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let pack = src.path().join("pack.idx");
        fs::write(&pack, b"packdata").unwrap();
        let mut perms = fs::metadata(&pack).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&pack, perms).unwrap();

        let target = dst.path().join("out");
        copy_library_tree(src.path(), &target).unwrap();
        assert_eq!(fs::read(target.join("pack.idx")).unwrap(), b"packdata");
    }

    // ── load_config_state_from ──

    #[test]
    fn config_state_missing_file_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let state = load_config_state_from(&tmp.path().join("repo-config.json"));
        assert!(matches!(state, ConfigState::Missing));
    }

    #[test]
    fn config_state_valid_json_is_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("repo-config.json");
        fs::write(&path, r#"{ "repo_path": "/tmp/lib", "pending_migration_from": null }"#)
            .unwrap();
        match load_config_state_from(&path) {
            ConfigState::Valid(config) => {
                assert_eq!(config.repo_path.as_deref(), Some("/tmp/lib"));
            }
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[test]
    fn config_state_corrupt_json_is_invalid_not_fresh_install() {
        // A corrupt config must never be treated like a missing one — that is
        // the "library rebuilt empty, all skills lost" failure mode (#228).
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("repo-config.json");
        fs::write(&path, "{ not json").unwrap();
        let state = load_config_state_from(&path);
        assert!(matches!(state, ConfigState::Invalid(_)), "{state:?}");
    }

    #[test]
    fn external_base_dir_lives_under_default_base_external() {
        let dir = external_base_dir(Path::new("/tmp/some/my-skills"));
        let prefix = default_base_dir().join("external");
        assert!(
            dir.starts_with(&prefix),
            "expected {} to start with {}",
            dir.display(),
            prefix.display()
        );
    }

    #[test]
    fn external_base_dir_is_stable_for_same_path() {
        let a = external_base_dir(Path::new("/tmp/some/my-skills"));
        let b = external_base_dir(Path::new("/tmp/some/my-skills"));
        assert_eq!(a, b);
    }

    #[test]
    fn external_base_dir_differs_for_different_paths() {
        let a = external_base_dir(Path::new("/tmp/one/my-skills"));
        let b = external_base_dir(Path::new("/tmp/two/my-skills"));
        assert_ne!(a, b);
    }

    #[test]
    fn external_base_dir_does_not_pollute_skills_root_or_its_parent() {
        let skills_root = Path::new("/tmp/external-test/my-skills");
        let dir = external_base_dir(skills_root);
        assert!(!dir.starts_with(skills_root));
        assert!(!dir.starts_with(skills_root.parent().unwrap()));
    }

    #[test]
    fn sanitize_dir_name_replaces_unsafe_characters() {
        assert_eq!(sanitize_dir_name("my skills"), "my-skills");
        assert_eq!(sanitize_dir_name("a/b\\c:d"), "a-b-c-d");
        assert_eq!(sanitize_dir_name(""), "external");
    }

    #[test]
    fn external_base_dir_relative_path_is_stable_against_absolute_form() {
        // For a not-yet-existing target, a relative path should namespace the
        // same as its cwd-absolutized form. We simulate by passing both forms
        // and asserting they match.
        let cwd = std::env::current_dir().unwrap();
        let rel = Path::new("nonexistent-skills-target-xyz");
        let abs = cwd.join(rel);
        assert_eq!(external_base_dir(rel), external_base_dir(&abs));
    }

    #[test]
    fn external_base_dir_normalizes_redundant_segments() {
        // `./x`, `x`, and `a/../x` should all hash to the same namespace when
        // none of them exist on disk.
        let plain = external_base_dir(Path::new("nonexistent-norm-target"));
        let dot = external_base_dir(Path::new("./nonexistent-norm-target"));
        let parent = external_base_dir(Path::new("a/../nonexistent-norm-target"));
        assert_eq!(plain, dot);
        assert_eq!(plain, parent);
    }

    #[test]
    fn lexically_normalize_handles_basic_cases() {
        assert_eq!(
            lexically_normalize(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            lexically_normalize(Path::new("./a/b")),
            PathBuf::from("a/b")
        );
        assert_eq!(lexically_normalize(Path::new("/..")), PathBuf::from("/"));
    }
}
