use anyhow::Result;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::collections::{HashMap, HashSet};
#[cfg(windows)]
use std::sync::{LazyLock, Mutex};
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
const SECRET_HARDEN_CACHE_TTL: Duration = Duration::from_secs(60);
#[cfg(windows)]
const SECRET_HARDEN_FAILURE_BACKOFF: Duration = Duration::from_secs(5);
#[cfg(windows)]
const SECRET_HARDEN_DEFER_DELAY: Duration = Duration::from_secs(30);

#[cfg(windows)]
#[derive(Clone, Copy)]
enum SecretHardenAttempt {
    InFlight,
    Succeeded(Instant),
    Failed(Instant),
}

#[cfg(windows)]
#[derive(Default)]
struct SecretHardenState {
    directories: HashMap<PathBuf, SecretHardenAttempt>,
    files: HashMap<PathBuf, SecretHardenAttempt>,
    pending_directories: HashSet<PathBuf>,
    pending_files: HashSet<PathBuf>,
    worker_running: bool,
}

#[cfg(windows)]
impl SecretHardenState {
    /// Queue a path for best-effort hardening. Returns true when the caller
    /// should start the single worker for this process.
    fn enqueue(&mut self, path: &Path, directory: bool, now: Instant) -> bool {
        let attempted = if directory {
            &self.directories
        } else {
            &self.files
        };
        let should_suppress = match attempted.get(path) {
            Some(SecretHardenAttempt::InFlight) => true,
            Some(SecretHardenAttempt::Succeeded(attempted_at)) => {
                now.saturating_duration_since(*attempted_at) < SECRET_HARDEN_CACHE_TTL
            }
            Some(SecretHardenAttempt::Failed(attempted_at)) => {
                now.saturating_duration_since(*attempted_at) < SECRET_HARDEN_FAILURE_BACKOFF
            }
            None => false,
        };
        if should_suppress {
            return false;
        }

        if directory {
            self.pending_directories.insert(path.to_path_buf());
        } else {
            self.pending_files.insert(path.to_path_buf());
        }
        if self.worker_running {
            false
        } else {
            self.worker_running = true;
            true
        }
    }
}

#[cfg(windows)]
static SECRET_HARDEN_STATE: LazyLock<Mutex<SecretHardenState>> =
    LazyLock::new(|| Mutex::new(SecretHardenState::default()));

mod active_pids;
pub use active_pids::{
    SessionCounts, SessionPresence, StreamingGuard, active_pids_dir, active_session_ids,
    find_active_session_id_by_pid, internal_pids_dir, mark_streaming, register_active_pid,
    session_counts, session_is_internal, session_presence, set_session_internal,
    streaming_pids_dir, unmark_streaming, unregister_active_pid, user_session_counts,
    user_session_presence,
};

/// Platform-aware runtime directory for sockets and ephemeral state.
///
/// - Linux: `$XDG_RUNTIME_DIR` (typically `/run/user/<uid>`)
/// - macOS: `$TMPDIR` (per-user, e.g. `/var/folders/xx/.../T/`)
/// - Fallback: `std::env::temp_dir()`
///
/// Can be overridden with `$JCODE_RUNTIME_DIR`.
pub fn runtime_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("JCODE_RUNTIME_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(dir) = std::env::var("TMPDIR") {
            return PathBuf::from(dir);
        }
    }

    let dir = fallback_runtime_dir();
    ensure_private_runtime_dir(&dir);
    dir
}

fn fallback_runtime_dir() -> PathBuf {
    std::env::temp_dir().join(format!("jcode-{}", runtime_user_discriminator()))
}

#[cfg(unix)]
fn runtime_user_discriminator() -> String {
    unsafe { libc::geteuid() }.to_string()
}

#[cfg(not(unix))]
fn runtime_user_discriminator() -> String {
    let raw = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".to_string());
    let sanitized: String = raw
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .take(64)
        .collect();
    if sanitized.is_empty() {
        "user".to_string()
    } else {
        sanitized
    }
}

fn ensure_private_runtime_dir(path: &Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(unix)]
    {
        let _ = jcode_core::fs::set_directory_permissions_owner_only(path);
    }
}

pub fn jcode_dir() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("JCODE_HOME") {
        return Ok(PathBuf::from(path));
    }

    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("No home directory"))?;
    Ok(home.join(".jcode"))
}

pub fn logs_dir() -> Result<PathBuf> {
    Ok(jcode_dir()?.join("logs"))
}

/// Durable state directory for state that must survive reboots.
///
/// [`runtime_dir`] typically resolves to a tmpfs (for example
/// `/run/user/<uid>` on Linux) that is wiped on reboot, so it must only hold
/// sockets and truly ephemeral state. State that has to outlive a reboot,
/// such as swarm plans and member records, belongs here instead: it resolves
/// to `~/.jcode/state` (respecting `JCODE_HOME`).
///
/// When `JCODE_RUNTIME_DIR` is set (tests and sandboxed temp servers), it
/// takes precedence so isolated runs never touch the real jcode home.
pub fn durable_state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("JCODE_RUNTIME_DIR") {
        return PathBuf::from(dir).join("durable-state");
    }
    match jcode_dir() {
        Ok(dir) => dir.join("state"),
        Err(_) => runtime_dir().join("durable-state"),
    }
}

/// Resolve jcode's app-owned config directory.
///
/// Default location is the platform config dir + `jcode` (for example
/// `~/.config/jcode` on Linux). When `JCODE_HOME` is set, sandbox this under
/// `$JCODE_HOME/config/jcode` so self-dev/tests do not leak into the user's
/// real config directory.
pub fn app_config_dir() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("JCODE_HOME") {
        return Ok(PathBuf::from(path).join("config").join("jcode"));
    }

    let config_dir =
        dirs::config_dir().ok_or_else(|| anyhow::anyhow!("No config directory found"))?;
    Ok(config_dir.join("jcode"))
}

/// Resolve a path under the user's home directory, but sandbox it under
/// `$JCODE_HOME/external/` when `JCODE_HOME` is set.
///
/// This keeps external provider auth files isolated during tests and sandboxed
/// runs without changing default on-disk locations for normal users.
pub fn user_home_path(relative: impl AsRef<Path>) -> Result<PathBuf> {
    let relative = relative.as_ref();
    if relative.is_absolute() {
        anyhow::bail!(
            "user_home_path expects a relative path, got {}",
            relative.display()
        );
    }

    if let Ok(path) = std::env::var("JCODE_HOME") {
        return Ok(PathBuf::from(path).join("external").join(relative));
    }

    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("No home directory"))?;
    Ok(home.join(relative))
}

/// Best-effort startup hardening for local config dirs that may store credentials.
///
/// This intentionally ignores failures so startup does not fail on exotic
/// filesystems, but it narrows exposure on typical Unix systems.
pub fn harden_user_config_permissions() {
    #[cfg(windows)]
    {
        if let Some(config_dir) = dirs::config_dir() {
            let jcode_config_dir = config_dir.join("jcode");
            if jcode_config_dir.exists() {
                schedule_windows_path_hardening(&jcode_config_dir, true);
            }
        }

        if let Ok(jcode_home) = jcode_dir()
            && jcode_home.exists()
        {
            schedule_windows_path_hardening(&jcode_home, true);
        }
        return;
    }

    #[cfg(not(windows))]
    {
        if let Some(config_dir) = dirs::config_dir() {
            let jcode_config_dir = config_dir.join("jcode");
            if jcode_config_dir.exists() {
                let _ = jcode_core::fs::set_directory_permissions_owner_only(&jcode_config_dir);
            }
        }

        if let Ok(jcode_home) = jcode_dir()
            && jcode_home.exists()
        {
            let _ = jcode_core::fs::set_directory_permissions_owner_only(&jcode_home);
        }
    }
}

/// Best-effort hardening for a secret-bearing file and its parent directory.
///
/// This is used before reading credential files so legacy permissive modes can
/// be tightened opportunistically.
pub fn harden_secret_file_permissions(path: &Path) {
    #[cfg(windows)]
    {
        harden_secret_file_permissions_windows(path);
        return;
    }

    #[cfg(not(windows))]
    {
        if let Some(parent) = path.parent() {
            let _ = jcode_core::fs::set_directory_permissions_owner_only(parent);
        }
        if path.exists() {
            let _ = jcode_core::fs::set_permissions_owner_only(path);
        }
    }
}

#[cfg(windows)]
fn harden_secret_file_permissions_windows(path: &Path) {
    // Windows ACL replacement is substantially more expensive than chmod and
    // security products can amplify it into seconds. Credential readers call
    // this helper frequently, including on the startup and TUI render paths.
    // Read-time hardening is opportunistic, while Jcode's own secret writes
    // harden synchronously below. Defer the opportunistic repair so first-frame
    // latency does not inherit multi-second SetNamedSecurityInfoW calls. The
    // worker coalesces repeated probes and retries paths after a short TTL.
    if let Some(parent) = path.parent() {
        schedule_windows_path_hardening(parent, true);
    }
    if path.exists() {
        schedule_windows_path_hardening(path, false);
    }
}

#[cfg(windows)]
fn schedule_windows_path_hardening(path: &Path, directory: bool) {
    let should_spawn = {
        let Ok(mut state) = SECRET_HARDEN_STATE.lock() else {
            return;
        };
        state.enqueue(path, directory, Instant::now())
    };

    if !should_spawn {
        return;
    }

    if std::thread::Builder::new()
        .name("jcode-windows-acl-harden".to_string())
        .spawn(|| {
            std::thread::sleep(SECRET_HARDEN_DEFER_DELAY);
            run_windows_hardening_worker();
        })
        .is_err()
        && let Ok(mut state) = SECRET_HARDEN_STATE.lock()
    {
        state.worker_running = false;
    }
}

#[cfg(windows)]
fn run_windows_hardening_worker() {
    loop {
        let (directories, files) = {
            let Ok(mut state) = SECRET_HARDEN_STATE.lock() else {
                return;
            };
            if state.pending_directories.is_empty() && state.pending_files.is_empty() {
                state.worker_running = false;
                return;
            }
            let directories = std::mem::take(&mut state.pending_directories);
            let files = std::mem::take(&mut state.pending_files);
            // Mark attempts before releasing the lock. Otherwise render-time
            // probes can requeue the same paths while a slow ACL call is in
            // flight, keeping the worker in an endless hardening loop.
            for path in &directories {
                state
                    .directories
                    .insert(path.clone(), SecretHardenAttempt::InFlight);
            }
            for path in &files {
                state
                    .files
                    .insert(path.clone(), SecretHardenAttempt::InFlight);
            }
            (directories, files)
        };

        let mut directory_results = Vec::with_capacity(directories.len());
        for path in &directories {
            let succeeded = jcode_core::fs::set_directory_permissions_owner_only(path).is_ok();
            directory_results.push((path.clone(), succeeded));
        }
        let mut file_results = Vec::with_capacity(files.len());
        for path in &files {
            let succeeded =
                !path.exists() || jcode_core::fs::set_permissions_owner_only(path).is_ok();
            file_results.push((path.clone(), succeeded));
        }

        let Ok(mut state) = SECRET_HARDEN_STATE.lock() else {
            return;
        };
        let completed_at = Instant::now();
        for (path, succeeded) in directory_results {
            state.directories.insert(
                path,
                if succeeded {
                    SecretHardenAttempt::Succeeded(completed_at)
                } else {
                    SecretHardenAttempt::Failed(completed_at)
                },
            );
        }
        for (path, succeeded) in file_results {
            state.files.insert(
                path,
                if succeeded {
                    SecretHardenAttempt::Succeeded(completed_at)
                } else {
                    SecretHardenAttempt::Failed(completed_at)
                },
            );
        }
        if state.pending_directories.is_empty() && state.pending_files.is_empty() {
            state.worker_running = false;
            return;
        }
        // New paths arrived while the ACL calls were running. Process them in
        // this worker without another startup delay.
        drop(state);
    }
}

/// Validate an external auth file managed by another tool before reading it.
///
/// jcode intentionally avoids mutating these files. We also reject obvious risky
/// cases like symlinks so a remembered trust decision stays bound to a real file
/// path rather than an arbitrary redirect.
pub fn validate_external_auth_file(path: &Path) -> Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to inspect external auth file {}: {}",
            path.display(),
            e
        )
    })?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!(
            "Refusing to read external auth file via symlink: {}",
            path.display()
        );
    }
    if !metadata.is_file() {
        anyhow::bail!(
            "External auth path is not a regular file: {}",
            path.display()
        );
    }
    std::fs::canonicalize(path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to canonicalize external auth file {}: {}",
            path.display(),
            e
        )
    })
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
        jcode_core::fs::set_directory_permissions_owner_only(path)?;
    }
    Ok(())
}

pub fn write_text_secret(path: &Path, content: &str) -> Result<()> {
    write_bytes_inner(path, content.as_bytes(), true, true, true)
}

pub fn upsert_env_file_value(path: &Path, env_key: &str, value: Option<&str>) -> Result<()> {
    let mut key_bytes = env_key.bytes();
    let key_is_safe = key_bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && key_bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if !key_is_safe {
        anyhow::bail!("invalid environment variable name");
    }
    if value.is_some_and(|value| value.contains(['\r', '\n'])) {
        anyhow::bail!("environment variable value cannot contain a newline");
    }

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let prefix = format!("{}=", env_key);

    let mut lines = Vec::new();
    let mut replaced = false;
    for line in existing.lines() {
        if line.starts_with(&prefix) {
            replaced = true;
            if let Some(value) = value {
                lines.push(format!("{}={}", env_key, value));
            }
        } else {
            lines.push(line.to_string());
        }
    }

    if !replaced && let Some(value) = value {
        lines.push(format!("{}={}", env_key, value));
    }

    let mut content = lines.join("\n");
    if !content.is_empty() {
        content.push('\n');
    }
    write_text_secret(path, &content)
}

pub fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    write_json_inner(path, value, true, false)
}

pub fn write_json_secret<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    write_json_inner(path, value, true, true)
}

/// Fast JSON write: atomic rename but no fsync. Good for frequent saves where
/// durability on power loss is not critical (e.g., session saves during tool execution).
/// Data is still safe against process crashes (atomic rename protects against partial writes).
pub fn write_json_fast<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    write_json_inner(path, value, false, false)
}

/// Atomically write raw bytes to `path` (temp file + rename), fsync'd for
/// durability. Used for editing user config files where a torn write would be
/// catastrophic.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    write_bytes_inner(path, bytes, true, false, true)
}

/// Atomically and durably write raw bytes without rotating the destination to
/// `<path>.bak`. Use this only when `path` is itself an explicitly managed
/// recovery copy. In particular, rotating a path already ending in `.bak`
/// aliases the destination on Windows and can restore the old bytes over the
/// newly published recovery copy.
pub fn write_bytes_without_backup(path: &Path, bytes: &[u8]) -> Result<()> {
    write_bytes_inner(path, bytes, true, false, false)
}

fn sync_file_contents(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    #[cfg(not(windows))]
    let file = std::fs::File::open(path)?;
    file.sync_all()
}

/// Confirm already-published bytes and their directory entry after an atomic
/// writer reported an ambiguous post-publication error. This never republishes
/// bytes and is safe to retry.
pub fn confirm_publication_durable(path: &Path) -> Result<()> {
    #[cfg(any(test, feature = "test-support"))]
    inject_test_confirmation_failure(path)?;
    sync_file_contents(path).map_err(|error| {
        anyhow::anyhow!(
            "Published file {} could not be synchronized: {}",
            path.display(),
            error
        )
    })?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                anyhow::anyhow!(
                    "Published directory entry for {} could not be synchronized: {}",
                    path.display(),
                    error
                )
            })?;
    }
    Ok(())
}

fn write_json_inner<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
    durable: bool,
    secret: bool,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    write_bytes_inner(path, &bytes, durable, secret, true)
}

#[cfg(feature = "test-support")]
static TEST_WRITE_FAILURE: std::sync::Mutex<Option<(Option<std::path::PathBuf>, usize, usize)>> =
    std::sync::Mutex::new(None);

#[cfg(feature = "test-support")]
static TEST_POST_PUBLICATION_FAILURE: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);

#[cfg(any(test, feature = "test-support"))]
static TEST_POST_APPEND_FAILURE: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);

#[cfg(any(test, feature = "test-support"))]
static TEST_CONFIRMATION_FAILURE: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);

/// Fail the next atomic write, optionally only when it targets `path`.
/// Compiled only for test-support builds.
#[cfg(feature = "test-support")]
pub fn inject_write_failure(path: Option<std::path::PathBuf>) -> TestWriteFailureGuard {
    inject_nth_write_failure(path, 1)
}

/// Fail the `nth` matching atomic write. Compiled only for test-support builds.
#[cfg(feature = "test-support")]
pub fn inject_nth_write_failure(
    path: Option<std::path::PathBuf>,
    nth: usize,
) -> TestWriteFailureGuard {
    inject_nth_write_failures(path, nth, 1)
}

/// Starting at the `nth` matching write, fail `count` consecutive matching
/// writes. Compiled only for test-support builds.
#[cfg(feature = "test-support")]
pub fn inject_nth_write_failures(
    path: Option<std::path::PathBuf>,
    nth: usize,
    count: usize,
) -> TestWriteFailureGuard {
    assert!(nth > 0, "write failure index is one-based");
    assert!(count > 0, "write failure count must be nonzero");
    *TEST_WRITE_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((path, nth, count));
    TestWriteFailureGuard
}

#[cfg(feature = "test-support")]
pub struct TestWriteFailureGuard;

#[cfg(feature = "test-support")]
impl Drop for TestWriteFailureGuard {
    fn drop(&mut self) {
        *TEST_WRITE_FAILURE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

/// Fail one durable atomic write after its replacement bytes have become the
/// visible destination. This models errors such as a parent-directory fsync
/// failure whose Result alone cannot tell a caller whether publication occurred.
#[cfg(feature = "test-support")]
pub fn inject_post_publication_failure(
    path: std::path::PathBuf,
) -> TestPostPublicationFailureGuard {
    *TEST_POST_PUBLICATION_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(path);
    TestPostPublicationFailureGuard
}

#[cfg(feature = "test-support")]
pub struct TestPostPublicationFailureGuard;

#[cfg(feature = "test-support")]
impl Drop for TestPostPublicationFailureGuard {
    fn drop(&mut self) {
        *TEST_POST_PUBLICATION_FAILURE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

/// Fail one durable journal append after the complete line is visible but
/// before its first synchronization attempt.
#[cfg(any(test, feature = "test-support"))]
pub fn inject_post_append_failure(path: std::path::PathBuf) -> TestPostAppendFailureGuard {
    *TEST_POST_APPEND_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(path);
    TestPostAppendFailureGuard
}

#[cfg(any(test, feature = "test-support"))]
pub struct TestPostAppendFailureGuard;

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestPostAppendFailureGuard {
    fn drop(&mut self) {
        *TEST_POST_APPEND_FAILURE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

/// Fail one explicit durability confirmation for an already-visible path.
/// Compiled only for test-support builds.
#[cfg(any(test, feature = "test-support"))]
pub fn inject_confirmation_failure(path: std::path::PathBuf) -> TestConfirmationFailureGuard {
    *TEST_CONFIRMATION_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(path);
    TestConfirmationFailureGuard
}

#[cfg(any(test, feature = "test-support"))]
pub struct TestConfirmationFailureGuard;

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestConfirmationFailureGuard {
    fn drop(&mut self) {
        *TEST_CONFIRMATION_FAILURE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

#[cfg(any(test, feature = "test-support"))]
fn inject_test_post_append_failure(path: &Path) -> Result<()> {
    let mut failure = TEST_POST_APPEND_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if failure.as_deref() == Some(path) {
        *failure = None;
        anyhow::bail!(
            "injected post-append durability failure for {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
fn inject_test_confirmation_failure(path: &Path) -> Result<()> {
    let mut failure = TEST_CONFIRMATION_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if failure.as_deref() == Some(path) {
        *failure = None;
        anyhow::bail!(
            "injected publication confirmation failure for {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(feature = "test-support")]
fn inject_test_post_publication_failure(path: &Path) -> Result<()> {
    let mut failure = TEST_POST_PUBLICATION_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if failure.as_deref() == Some(path) {
        *failure = None;
        anyhow::bail!(
            "injected post-publication durability failure for {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(feature = "test-support")]
fn inject_test_write_failure(path: &Path) -> Result<()> {
    let mut failure = TEST_WRITE_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((target, remaining, failures)) = failure.as_mut()
        && target.as_deref().is_none_or(|target| target == path)
    {
        if *remaining > 1 {
            *remaining -= 1;
        } else {
            *failures -= 1;
            if *failures == 0 {
                *failure = None;
            }
            anyhow::bail!("injected atomic write failure for {}", path.display());
        }
    }
    Ok(())
}

struct AtomicPathLock(std::fs::File);

impl AtomicPathLock {
    fn acquire(path: &Path) -> Result<Self> {
        let lock_path = path.with_extension("atomic.lock");
        if let Some(parent) = lock_path.parent() {
            ensure_dir(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        file.lock()?;
        Ok(Self(file))
    }
}

impl Drop for AtomicPathLock {
    fn drop(&mut self) {
        drop(self.0.unlock());
    }
}

fn write_bytes_inner(
    path: &Path,
    bytes: &[u8],
    durable: bool,
    secret: bool,
    preserve_backup: bool,
) -> Result<()> {
    let _path_lock = AtomicPathLock::acquire(path)?;
    write_bytes_inner_locked(path, bytes, durable, secret, preserve_backup)
}

fn write_bytes_inner_locked(
    path: &Path,
    bytes: &[u8],
    durable: bool,
    secret: bool,
    preserve_backup: bool,
) -> Result<()> {
    #[cfg(feature = "test-support")]
    inject_test_write_failure(path)?;
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
        if secret {
            // Writes remain strict even though read-time legacy repair is
            // deferred on Windows. Harden the container before any secret
            // bytes are created so a permissive inherited ACL is never
            // published, even briefly.
            jcode_core::fs::set_directory_permissions_owner_only(parent)?;
        }
    }

    let pid = std::process::id();
    let nonce: u64 = rand::random();
    let tmp_path = path.with_extension(format!("tmp.{}.{}", pid, nonce));
    let backup_tmp_path = path.with_extension(format!("bak.tmp.{}.{}", pid, nonce));
    let destination_existed = path.exists();

    let result = (|| -> Result<()> {
        let file = std::fs::File::create(&tmp_path)?;
        if secret {
            jcode_core::fs::set_permissions_owner_only(&tmp_path)?;
        }
        let mut writer = std::io::BufWriter::new(file);
        writer.write_all(bytes)?;
        let file = writer
            .into_inner()
            .map_err(|e| anyhow::anyhow!("flush failed: {}", e))?;

        if durable {
            file.sync_all()?;
        }
        drop(file);

        #[cfg(windows)]
        let mut destination_published = false;
        if destination_existed {
            let bak_path = path.with_extension("bak");
            if secret {
                jcode_core::fs::set_permissions_owner_only(path)?;
            }
            // Preserve the previous version as .bak without ever leaving the
            // primary path missing. On Unix, rename(tmp, path) atomically
            // replaces the destination, so the backup can be a hard link to
            // the old inode: concurrent readers always see either the old or
            // the new content, never ENOENT. (The old rename-away approach
            // opened a window where the primary did not exist, which made
            // concurrent load-all style readers silently drop entries, e.g.
            // self-dev build requests "disappearing" from the queue.)
            #[cfg(unix)]
            if preserve_backup {
                std::fs::hard_link(path, &backup_tmp_path)?;
                std::fs::rename(&backup_tmp_path, &bak_path)?;
            }
            // Stage the old primary before publishing. ReplaceFileW can expose
            // a transient missing path and fail with ERROR_UNABLE_TO_REMOVE_REPLACED
            // while readers open the destination. Rust's Windows rename uses
            // MoveFileExW with MOVEFILE_REPLACE_EXISTING (and a handle-based
            // fallback), avoiding that remove-then-move window.
            #[cfg(windows)]
            {
                if preserve_backup {
                    if let Err(link_error) = std::fs::hard_link(path, &backup_tmp_path) {
                        std::fs::copy(path, &backup_tmp_path).map_err(|copy_error| {
                            anyhow::anyhow!(
                                "Unable to stage backup for {}: hard link failed: {}; copy failed: {}",
                                path.display(),
                                link_error,
                                copy_error
                            )
                        })?;
                    }
                    if durable {
                        sync_file_contents(&backup_tmp_path)?;
                    }
                }
                std::fs::rename(&tmp_path, path)?;
                destination_published = true;
                if preserve_backup && bak_path.exists() {
                    if let Err(error) = std::fs::rename(&backup_tmp_path, &bak_path) {
                        eprintln!(
                            "Atomic write to {} was published, but backup rotation at {} failed: {}",
                            path.display(),
                            bak_path.display(),
                            error
                        );
                    }
                    if bak_path.exists() {
                        drop(std::fs::remove_file(&backup_tmp_path));
                    }
                } else if preserve_backup {
                    if let Err(error) = std::fs::rename(&backup_tmp_path, &bak_path) {
                        eprintln!(
                            "Atomic write to {} was published, but backup promotion to {} failed: {}",
                            path.display(),
                            bak_path.display(),
                            error
                        );
                    }
                } else {
                    drop(std::fs::remove_file(&backup_tmp_path));
                }
            }
            #[cfg(not(any(unix, windows)))]
            if preserve_backup {
                drop(std::fs::remove_file(&bak_path));
                drop(std::fs::rename(path, &bak_path));
            }
            if preserve_backup
                && secret
                && bak_path.exists()
                && let Err(error) = jcode_core::fs::set_permissions_owner_only(&bak_path)
            {
                eprintln!(
                    "Atomic write backup {} was published after owner-only source hardening, but permission recheck failed: {}",
                    bak_path.display(),
                    error
                );
            }
        }

        #[cfg(windows)]
        if !destination_published {
            std::fs::rename(&tmp_path, path)?;
        }
        #[cfg(not(windows))]
        std::fs::rename(&tmp_path, path)?;
        if secret && let Err(error) = jcode_core::fs::set_permissions_owner_only(path) {
            eprintln!(
                "Atomic write to {} was published after owner-only temporary-file hardening, but permission recheck failed: {}",
                path.display(),
                error
            );
        }

        #[cfg(feature = "test-support")]
        inject_test_post_publication_failure(path)?;

        #[cfg(unix)]
        if durable && let Some(parent) = path.parent() {
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Atomic write to {} was published, but parent-directory sync failed: {}",
                        path.display(),
                        error
                    )
                })?;
        }

        Ok(())
    })();

    if result.is_err() {
        if !destination_existed || path.exists() {
            drop(std::fs::remove_file(&backup_tmp_path));
        }
        #[cfg(not(windows))]
        drop(std::fs::remove_file(&tmp_path));
        #[cfg(windows)]
        if !destination_existed || path.exists() {
            drop(std::fs::remove_file(&tmp_path));
        }
    }

    result
}

mod recovery;

pub use recovery::*;

#[cfg(test)]
mod tests;
