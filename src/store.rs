//! Atomic filesystem storage for plans, trigger records, and fired-event state.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use directories::ProjectDirs;
use fs2::FileExt;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    lifecycle::Event,
    model::{BlockId, Lead, Plan, PlanDate, PlanError, ScheduleRev},
};

#[derive(Debug, Clone)]
pub struct Store {
    data: PathBuf,
    config: PathBuf,
    state: PathBuf,
}

impl Store {
    #[must_use]
    pub fn new(base_dir: &Path) -> Self {
        Self {
            data: base_dir.join("data").join("ccplan"),
            config: base_dir.join("config").join("ccplan"),
            state: base_dir.join("state").join("ccplan"),
        }
    }

    /// Creates a store rooted in the current user's platform-specific directories.
    ///
    /// # Errors
    ///
    /// Returns an error when the OS/user directory provider cannot determine project directories.
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub fn for_user() -> Result<Self, StoreError> {
        let dirs = ProjectDirs::from("io", "ccplan", "ccplan")
            .ok_or(StoreError::ProjectDirsUnavailable)?;
        Ok(Self {
            data: dirs.data_dir().to_path_buf(),
            config: dirs.config_dir().to_path_buf(),
            state: dirs
                .state_dir()
                .unwrap_or_else(|| dirs.data_dir())
                .to_path_buf(),
        })
    }

    #[must_use]
    pub fn plan_path(&self, date: &PlanDate) -> PathBuf {
        self.plans_dir().join(format!("{date}.toml"))
    }

    #[must_use]
    pub fn archive_path(&self, date: &PlanDate) -> PathBuf {
        self.archive_dir().join(format!("{date}.toml"))
    }

    #[must_use]
    pub fn fire_log_path(&self) -> PathBuf {
        self.log_dir().join("fire.log")
    }

    /// Acquires the store's exclusive mutation lock.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Locked` if another writer already holds the lock.
    pub fn try_lock(&self) -> Result<StoreLock, StoreError> {
        map_io_result(fs::create_dir_all(&self.state), &self.state)?;
        let lock_path = self.lock_path();
        let file = map_io_result(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&lock_path),
            &lock_path,
        )?;

        lock_file(file, lock_path)
    }

    /// Loads and validates the plan for a date.
    ///
    /// # Errors
    ///
    /// Returns an error if reading the file fails or the file is not a valid plan.
    pub fn load_plan(&self, date: &PlanDate) -> Result<Option<Plan>, StoreError> {
        self.load_plan_with_default(date, Lead::from_seconds_const(300))
    }

    /// Loads and validates the plan for a date with a specified default notification lead.
    ///
    /// # Errors
    ///
    /// Returns an error if reading the file fails or the file is not a valid plan.
    pub fn load_plan_with_default(
        &self,
        date: &PlanDate,
        default_lead: Lead,
    ) -> Result<Option<Plan>, StoreError> {
        self.load_plan_unlocked_with_default(date, default_lead)
    }

    /// Merges and persists a plan using the terminal-history policy.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is locked, the incoming/existing plan is invalid, or terminal
    /// history would be altered without `HistoryPolicy::Override`.
    pub fn set_plan(&self, incoming: &Plan, policy: HistoryPolicy) -> Result<Plan, StoreError> {
        self.set_plan_with_default(incoming, policy, Lead::from_seconds_const(300))
    }

    /// Merges and persists a plan using the terminal-history policy and a specified default notification lead.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is locked, the incoming/existing plan is invalid, or terminal
    /// history would be altered without `HistoryPolicy::Override`.
    pub fn set_plan_with_default(
        &self,
        incoming: &Plan,
        policy: HistoryPolicy,
        default_lead: Lead,
    ) -> Result<Plan, StoreError> {
        let _lock = self.try_lock()?;
        let merged = self.merge_plan_with_default(incoming, policy, default_lead)?;
        self.write_plan_unlocked(&merged)?;
        Ok(merged)
    }

    /// Runs a read-modify-write transaction for a date under the exclusive lock (Inv-17).
    ///
    /// Loads the plan (or `None`), hands it to `mutate`, then merges the result with the
    /// preserve-terminal-history policy and writes it — holding the lock for the entire
    /// load→mutate→write window. This serializes concurrent mutations so two writers editing the
    /// same day cannot lose each other's blocks (the load-outside-the-lock-then-`set` race that a
    /// plain `load_plan` + `set_plan` pair is subject to).
    ///
    /// The closure's error type `E` need only absorb `StoreError` (`E: From<StoreError>`), so
    /// command-layer errors flow straight through without a manual conversion at each call site.
    ///
    /// # Errors
    ///
    /// Returns the closure's error, or a `StoreError` (lock contention, invalid plan,
    /// terminal-history conflict, or I/O) surfaced through `E`.
    pub fn update<F, E>(&self, date: &PlanDate, default_lead: Lead, mutate: F) -> Result<Plan, E>
    where
        F: FnOnce(Option<Plan>) -> Result<Plan, E>,
        E: From<StoreError>,
    {
        let _lock = self.try_lock()?;
        let existing = self.load_plan_unlocked_with_default(date, default_lead)?;
        let next = mutate(existing)?;
        let merged = self.merge_plan_with_default(&next, HistoryPolicy::Preserve, default_lead)?;
        self.write_plan_unlocked(&merged)?;
        Ok(merged)
    }

    /// Moves the canonical plan for a date to the archive directory.
    ///
    /// # Errors
    ///
    /// Returns an error if filesystem operations fail.
    pub fn archive(&self, date: &PlanDate) -> Result<bool, StoreError> {
        let _lock = self.try_lock()?;
        let plan_path = self.plan_path(date);
        if !plan_path.exists() {
            return Ok(false);
        }

        let archive_path = self.archive_path(date);
        ensure_parent(&archive_path)?;
        if archive_path.exists() {
            map_io_result(fs::remove_file(&archive_path), &archive_path)?;
        }
        map_io_result(fs::rename(&plan_path, &archive_path), &archive_path)?;
        self.prune_fired_for_date(date)?;
        Ok(true)
    }

    /// Removes any canonical or archived plan for a date.
    ///
    /// # Errors
    ///
    /// Returns an error if filesystem removal fails.
    pub fn purge(&self, date: &PlanDate) -> Result<bool, StoreError> {
        let _lock = self.try_lock()?;
        let mut removed = false;
        for path in [self.plan_path(date), self.archive_path(date)] {
            if remove_file_if_exists(&path)? {
                removed = true;
            }
        }
        self.prune_fired_for_date(date)?;
        Ok(removed)
    }

    /// Atomically records a fired event key if it has not already been recorded.
    ///
    /// # Errors
    ///
    /// Returns an error if the ledger cannot be read or written.
    pub fn check_and_set_fired(&self, key: FiredEventKey) -> Result<FiredStatus, StoreError> {
        let _lock = self.try_lock()?;
        let path = self.fired_path();
        let mut ledger = read_fired_ledger(&path)?;
        if ledger.fired.contains(&key) {
            return Ok(FiredStatus::AlreadyFired);
        }

        ledger.fired.push(key);
        write_fired_ledger(&path, &ledger)?;
        Ok(FiredStatus::Recorded)
    }

    /// Drops every fired-event key recorded for a date.
    ///
    /// The fired ledger is otherwise append-only (`check_and_set_fired` never removes), so without
    /// this it would grow without bound and every fire would re-read and re-scan the whole file.
    /// `archive`/`purge` retire a day's plan, so its fired keys can never be consulted again — they
    /// are pruned here under the same lock the callers already hold.
    fn prune_fired_for_date(&self, date: &PlanDate) -> Result<(), StoreError> {
        let path = self.fired_path();
        let mut ledger = read_fired_ledger(&path)?;
        let before = ledger.fired.len();
        ledger.fired.retain(|key| &key.date != date);
        if ledger.fired.len() != before {
            write_fired_ledger(&path, &ledger)?;
        }
        Ok(())
    }

    /// Records or replaces an owned trigger descriptor.
    ///
    /// # Errors
    ///
    /// Returns an error if trigger state cannot be read or written.
    pub fn record_trigger(&self, trigger: TriggerRecord) -> Result<(), StoreError> {
        let _lock = self.try_lock()?;
        let path = self.triggers_path();
        let mut ledger = read_trigger_ledger(&path)?;
        ledger
            .triggers
            .retain(|existing| existing.backend_id != trigger.backend_id);
        ledger.triggers.push(trigger);
        write_trigger_ledger(&path, &ledger)
    }

    /// Lists all owned trigger descriptors.
    ///
    /// # Errors
    ///
    /// Returns an error if trigger state cannot be read.
    pub fn list_triggers(&self) -> Result<Vec<TriggerRecord>, StoreError> {
        let path = self.triggers_path();
        Ok(read_trigger_ledger(&path)?.triggers)
    }

    /// Removes one owned trigger descriptor by backend id.
    ///
    /// # Errors
    ///
    /// Returns an error if trigger state cannot be read or written.
    pub fn remove_trigger(&self, backend_id: &str) -> Result<bool, StoreError> {
        let _lock = self.try_lock()?;
        let path = self.triggers_path();
        let mut ledger = read_trigger_ledger(&path)?;
        let original_len = ledger.triggers.len();
        ledger
            .triggers
            .retain(|trigger| trigger.backend_id != backend_id);
        let removed = ledger.triggers.len() != original_len;
        if removed {
            write_trigger_ledger(&path, &ledger)?;
        }
        Ok(removed)
    }
    fn load_plan_unlocked_with_default(
        &self,
        date: &PlanDate,
        default_lead: Lead,
    ) -> Result<Option<Plan>, StoreError> {
        let path = self.plan_path(date);
        let input = match fs::read_to_string(&path) {
            Ok(input) => input,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(io_error(path, source)),
        };
        Ok(Some(Plan::from_toml_with_default(&input, default_lead)?))
    }
    fn merge_plan_with_default(
        &self,
        incoming: &Plan,
        policy: HistoryPolicy,
        default_lead: Lead,
    ) -> Result<Plan, StoreError> {
        incoming.validate().map_err(PlanError::from)?;
        if policy == HistoryPolicy::Override {
            return Ok(incoming.clone());
        }

        let Some(existing) = self.load_plan_unlocked_with_default(&incoming.date, default_lead)?
        else {
            return Ok(incoming.clone());
        };
        let terminal_blocks: Vec<_> = existing
            .blocks
            .iter()
            .filter(|block| block.status.is_terminal())
            .collect();
        if existing.timezone != incoming.timezone {
            if let Some(terminal) = terminal_blocks.first() {
                return Err(StoreError::TerminalHistory {
                    id: terminal.id.clone(),
                });
            }
        }

        let mut blocks = incoming.blocks.clone();
        for terminal in terminal_blocks {
            match blocks.iter().find(|block| block.id == terminal.id) {
                Some(incoming_terminal) if incoming_terminal == terminal => {}
                Some(_) => {
                    return Err(StoreError::TerminalHistory {
                        id: terminal.id.clone(),
                    });
                }
                None => blocks.push(terminal.clone()),
            }
        }

        let merged = Plan {
            date: incoming.date.clone(),
            timezone: incoming.timezone.clone(),
            blocks,
        };
        merged.validate().map_err(PlanError::from)?;
        Ok(merged)
    }

    fn write_plan_unlocked(&self, plan: &Plan) -> Result<(), StoreError> {
        let path = self.plan_path(&plan.date);
        let toml = plan.to_toml()?;
        atomic_write(&path, toml.as_bytes())
    }

    fn plans_dir(&self) -> PathBuf {
        self.data.join("plans")
    }

    fn archive_dir(&self) -> PathBuf {
        self.data.join("archive")
    }

    fn log_dir(&self) -> PathBuf {
        self.data.join("log")
    }

    fn lock_path(&self) -> PathBuf {
        self.state.join("store.lock")
    }

    fn fired_path(&self) -> PathBuf {
        self.state.join("fired.json")
    }

    fn triggers_path(&self) -> PathBuf {
        self.state.join("triggers.json")
    }

    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.config.join("config.toml")
    }
}

#[derive(Debug)]
pub struct StoreLock {
    file: File,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn lock_file(file: File, lock_path: PathBuf) -> Result<StoreLock, StoreError> {
    match file.try_lock_exclusive() {
        Ok(()) => Ok(StoreLock { file }),
        Err(error) if is_lock_contention(&error) => Err(StoreError::Locked),
        Err(source) => Err(io_error(lock_path, source)),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn is_lock_contention(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::PermissionDenied
    ) {
        return true;
    }
    // On Windows an exclusive-lock conflict surfaces as a raw OS error rather than a mapped
    // ErrorKind: ERROR_SHARING_VIOLATION (32) or ERROR_LOCK_VIOLATION (33). These codes are
    // Windows-specific, so only consult them there — on Unix 32/33 are unrelated errno values.
    #[cfg(windows)]
    {
        matches!(error.raw_os_error(), Some(32 | 33))
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryPolicy {
    Preserve,
    Override,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FiredEventKey {
    pub date: PlanDate,
    pub block_id: BlockId,
    pub event: Event,
    pub rev: ScheduleRev,
    pub scheduled_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiredStatus {
    Recorded,
    AlreadyFired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerRecord {
    pub backend_id: String,
    pub date: PlanDate,
    pub block_id: BlockId,
    pub event: Event,
    pub rev: ScheduleRev,
    pub scheduled_at: Timestamp,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FiredLedger {
    #[serde(default)]
    fired: Vec<FiredEventKey>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TriggerLedger {
    #[serde(default)]
    triggers: Vec<TriggerRecord>,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("project directories are unavailable")]
    ProjectDirsUnavailable,
    #[error("store is locked by another writer")]
    Locked,
    #[error("I/O error at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid plan in store: {0}")]
    Plan(#[from] PlanError),
    #[error("invalid JSON at `{path}`: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("terminal block `{id}` would be altered without override")]
    TerminalHistory { id: BlockId },
}

impl StoreError {
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Plan(error) => error.exit_code(),
            Self::TerminalHistory { .. } => 6,
            Self::ProjectDirsUnavailable | Self::Locked | Self::Io { .. } | Self::Json { .. } => 1,
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    ensure_parent(path)?;
    let temp_path = temp_path_for(path);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = map_io_result(options.open(&temp_path), &temp_path)?;
    map_io_result(file.write_all(bytes), &temp_path)?;
    map_io_result(file.sync_all(), &temp_path)?;
    drop(file);
    map_io_result(fs::rename(&temp_path, path), path)?;
    sync_parent_dir(path);
    Ok(())
}

/// Persists the directory entry produced by `atomic_write`'s rename so the atomic replace survives a
/// crash. fsyncing the file alone leaves the rename itself only in the page cache on POSIX; this
/// flushes the containing directory. Best-effort: a failure here can't corrupt data (the rename has
/// already happened), so it must not turn a successful write into an error.
#[cfg(unix)]
fn sync_parent_dir(path: &Path) {
    // Best-effort: opening the directory and fsyncing it can't fail in a way that corrupts data
    // (the rename has already landed), so any error is intentionally discarded.
    let _ = File::open(resolved_parent(path)).map(|dir| dir.sync_all());
}

// Windows has no portable directory-fsync; NTFS metadata durability is handled by the OS.
#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) {}

fn ensure_parent(path: &Path) -> Result<(), StoreError> {
    let parent = resolved_parent(path);
    map_io_result(fs::create_dir_all(parent), parent)
}

/// Resolves the directory a file lives in, treating a bare filename (empty parent) as the CWD.
fn resolved_parent(path: &Path) -> &Path {
    let parent = path.parent().unwrap_or(Path::new("."));
    if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    }
}

fn remove_file_if_exists(path: &Path) -> Result<bool, StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error(path, source)),
    }
}

fn read_fired_ledger(path: &Path) -> Result<FiredLedger, StoreError> {
    let Some(input) = read_state_file(path)? else {
        return Ok(FiredLedger::default());
    };
    map_json_result(serde_json::from_str(&input), path)
}

fn read_trigger_ledger(path: &Path) -> Result<TriggerLedger, StoreError> {
    let Some(input) = read_state_file(path)? else {
        return Ok(TriggerLedger::default());
    };
    map_json_result(serde_json::from_str(&input), path)
}

fn read_state_file(path: &Path) -> Result<Option<String>, StoreError> {
    match fs::read_to_string(path) {
        Ok(input) => Ok(Some(input)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(path, source)),
    }
}

fn write_fired_ledger(path: &Path, ledger: &FiredLedger) -> Result<(), StoreError> {
    let json = serialize_ledger(ledger);
    atomic_write(path, &json)
}

fn write_trigger_ledger(path: &Path, ledger: &TriggerLedger) -> Result<(), StoreError> {
    let json = serialize_ledger(ledger);
    atomic_write(path, &json)
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn serialize_ledger<T: Serialize>(ledger: &T) -> Vec<u8> {
    serde_json::to_vec_pretty(ledger).expect("ccplan ledger JSON serialization should not fail")
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn map_io_result<T>(result: io::Result<T>, path: impl AsRef<Path>) -> Result<T, StoreError> {
    result.map_err(|source| io_error(path, source))
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn map_json_result<T>(result: serde_json::Result<T>, path: &Path) -> Result<T, StoreError> {
    result.map_err(|source| json_error(path, source))
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn io_error(path: impl AsRef<Path>, source: io::Error) -> StoreError {
    StoreError::Io {
        path: path.as_ref().to_path_buf(),
        source,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn json_error(path: &Path, source: serde_json::Error) -> StoreError {
    StoreError::Json {
        path: path.to_path_buf(),
        source,
    }
}

fn temp_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("ccplan");
    path.with_file_name(format!(
        ".{file_name}.tmp.{}.{}",
        std::process::id(),
        next_temp_suffix()
    ))
}

fn next_temp_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use assert_fs::TempDir;

    #[test]
    fn atomic_write_replaces_existing_file() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("ledger.json");

        atomic_write(&path, b"one").expect("initial atomic write should succeed");
        atomic_write(&path, b"two").expect("replacement atomic write should succeed");

        assert_eq!(fs::read(&path).unwrap(), b"two");
    }

    #[test]
    fn ensure_parent_treats_empty_parent_as_current_dir() {
        // A bare filename has an empty parent; ensure_parent must fall back to "." rather than
        // erroring. `create_dir_all(".")` is a no-op on the existing CWD, so this writes nothing
        // (no temp-dir use, no CWD pollution) while still exercising the empty-parent branch.
        ensure_parent(Path::new("bare-name-with-no-parent.json"))
            .expect("empty parent should resolve to the current directory");
    }
}
