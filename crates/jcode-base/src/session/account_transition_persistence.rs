use anyhow::{Result, bail};
use std::fs::OpenOptions;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::storage;

tokio::task_local! {
    static ACCOUNT_TRANSITION_ADMISSION: AccountTransitionAdmission;
}

#[derive(Clone)]
pub struct AccountTransitionAdmission(Arc<AtomicBool>);

impl AccountTransitionAdmission {
    fn active(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

struct DeactivateAccountTransitionAdmission(AccountTransitionAdmission);

impl Drop for DeactivateAccountTransitionAdmission {
    fn drop(&mut self) {
        self.0.0.store(false, Ordering::Release);
    }
}

pub fn capture_account_transition_admission() -> Option<AccountTransitionAdmission> {
    ACCOUNT_TRANSITION_ADMISSION
        .try_with(Clone::clone)
        .ok()
        .filter(AccountTransitionAdmission::active)
}

pub fn account_transition_admission_held() -> bool {
    capture_account_transition_admission().is_some()
}

pub async fn with_account_transition_admission<F: std::future::Future>(future: F) -> F::Output {
    if let Some(admission) = capture_account_transition_admission() {
        return ACCOUNT_TRANSITION_ADMISSION.scope(admission, future).await;
    }

    let admission = AccountTransitionAdmission(Arc::new(AtomicBool::new(true)));
    let deactivate = DeactivateAccountTransitionAdmission(admission.clone());
    let output = ACCOUNT_TRANSITION_ADMISSION.scope(admission, future).await;
    drop(deactivate);
    output
}

/// Propagate a captured admission into a spawned child task without extending
/// its lifetime. If the owning turn has ended, nested persistence reacquires a
/// real shared lease instead of trusting stale task-local state.
pub async fn with_inherited_account_transition_admission<F: std::future::Future>(
    admission: Option<AccountTransitionAdmission>,
    future: F,
) -> F::Output {
    match admission.filter(AccountTransitionAdmission::active) {
        Some(admission) => ACCOUNT_TRANSITION_ADMISSION.scope(admission, future).await,
        None => future.await,
    }
}

pub(super) const ACCOUNT_RECONCILIATION_FILE: &str = "provider-account-reconciliation.json";
const COMPLETED_ACCOUNT_TRANSITIONS_FILE: &str = "provider-account-transitions.json";
const COMPLETED_ACCOUNT_TRANSITIONS_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(super) struct CompletedAccountTransition {
    pub(super) runtime_key: jcode_provider_core::RuntimeKey,
    pub(super) account_label: String,
    pub(super) account_id: String,
    pub(super) account_generation: u64,
}

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
pub(super) struct CompletedAccountTransitions {
    schema_version: u32,
    pub(super) transitions: Vec<CompletedAccountTransition>,
}

pub(super) fn account_transition_state_path(file: &str) -> Result<std::path::PathBuf> {
    Ok(storage::jcode_dir()?.join("state").join(file))
}

fn account_transition_lock_path() -> Result<std::path::PathBuf> {
    account_transition_state_path("provider-account-reconciliation.lock")
}

fn account_transition_turnstile_path() -> Result<std::path::PathBuf> {
    account_transition_state_path("provider-account-reconciliation.turnstile.lock")
}

pub(super) fn load_completed_account_transitions() -> Result<CompletedAccountTransitions> {
    let path = account_transition_state_path(COMPLETED_ACCOUNT_TRANSITIONS_FILE)?;
    if !path.exists() {
        return Ok(CompletedAccountTransitions {
            schema_version: COMPLETED_ACCOUNT_TRANSITIONS_SCHEMA_VERSION,
            transitions: Vec::new(),
        });
    }
    let state: CompletedAccountTransitions = serde_json::from_slice(&std::fs::read(&path)?)?;
    if state.schema_version != COMPLETED_ACCOUNT_TRANSITIONS_SCHEMA_VERSION {
        bail!(
            "unsupported completed account transition schema {}",
            state.schema_version
        );
    }
    Ok(state)
}

/// Record the identity made current by a completed transition. The caller must
/// own the exclusive account-transition lease. Ordinary delayed session saves
/// use this durable watermark after acquiring their shared lease, so they
/// cannot publish stale account-bound state after the transition releases.
#[doc(hidden)]
pub fn record_completed_account_transition(
    runtime_key: jcode_provider_core::RuntimeKey,
    account_label: &str,
    account_id: &str,
    account_generation: u64,
) -> Result<()> {
    let path = account_transition_state_path(COMPLETED_ACCOUNT_TRANSITIONS_FILE)?;
    let mut state = load_completed_account_transitions()?;
    state
        .transitions
        .retain(|transition| transition.runtime_key != runtime_key);
    state.transitions.push(CompletedAccountTransition {
        runtime_key,
        account_label: account_label.to_string(),
        account_id: account_id.to_string(),
        account_generation,
    });
    storage::write_json(&path, &state)
}

/// Cross-process lock shared by ordinary session persistence and held
/// exclusively by explicit account transitions. Keeping it in the session
/// layer prevents newly created or cloned identity-bound sessions from falling
/// outside an account transition's durable target snapshot.
pub struct AccountTransitionFileLock(std::fs::File);

impl AccountTransitionFileLock {
    pub fn acquire_exclusive() -> Result<Self> {
        let turnstile = open_lock_file(&account_transition_turnstile_path()?)?;
        turnstile.lock()?;
        Self::acquire_main(false).inspect(|_| drop(turnstile))
    }

    pub fn acquire_shared() -> Result<Self> {
        // Every reader briefly passes through the exclusive turnstile. Once a
        // writer owns it and is waiting for existing readers to drain, later
        // readers cannot barge ahead and starve the account transition.
        let turnstile = open_lock_file(&account_transition_turnstile_path()?)?;
        turnstile.lock()?;
        Self::acquire_main(true).inspect(|_| drop(turnstile))
    }

    pub fn acquire_shared_cancellable(cancel: &AtomicBool) -> Result<Self> {
        let turnstile = open_lock_file(&account_transition_turnstile_path()?)?;
        turnstile.lock()?;
        if cancel.load(Ordering::Acquire) {
            bail!("account transition lock acquisition cancelled");
        }

        let main = open_lock_file(&account_transition_lock_path()?)?;
        while !try_lock_shared(&main)? {
            if cancel.load(Ordering::Acquire) {
                bail!("account transition lock acquisition cancelled");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        drop(turnstile);
        Ok(Self(main))
    }

    pub fn try_acquire_exclusive() -> Result<Option<Self>> {
        let turnstile = open_lock_file(&account_transition_turnstile_path()?)?;
        if !try_lock_exclusive(&turnstile)? {
            return Ok(None);
        }
        let main = open_lock_file(&account_transition_lock_path()?)?;
        if !try_lock_exclusive(&main)? {
            return Ok(None);
        }
        drop(turnstile);
        Ok(Some(Self(main)))
    }

    /// Acquire the writer turnstile and main lock without permanently
    /// occupying an async executor thread. Blocking on the short-lived
    /// turnstile queues the writer ahead of later readers; the main-lock wait
    /// remains cancellable while existing admitted turns drain.
    pub fn acquire_exclusive_cancellable(cancel: &AtomicBool) -> Result<Self> {
        let turnstile = open_lock_file(&account_transition_turnstile_path()?)?;
        turnstile.lock()?;
        if cancel.load(Ordering::Acquire) {
            bail!("account transition lock acquisition cancelled");
        }

        let main = open_lock_file(&account_transition_lock_path()?)?;
        while !try_lock_exclusive(&main)? {
            if cancel.load(Ordering::Acquire) {
                bail!("account transition lock acquisition cancelled");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        drop(turnstile);
        Ok(Self(main))
    }

    fn acquire_main(shared: bool) -> Result<Self> {
        let file = open_lock_file(&account_transition_lock_path()?)?;
        if shared {
            file.lock_shared()?;
        } else {
            file.lock()?;
        }
        Ok(Self(file))
    }

    #[cfg(test)]
    pub(crate) fn acquire_exclusive_notifying(
        turnstile_acquired: std::sync::mpsc::Sender<()>,
    ) -> Result<Self> {
        let turnstile = open_lock_file(&account_transition_turnstile_path()?)?;
        turnstile.lock()?;
        let _ = turnstile_acquired.send(());
        Self::acquire_main(false).inspect(|_| drop(turnstile))
    }
}

impl Drop for AccountTransitionFileLock {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            crate::logging::warn(&format!(
                "Failed to release account transition lock: {error}"
            ));
        }
    }
}

fn open_lock_file(path: &std::path::Path) -> Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?)
}

fn try_lock_exclusive(file: &std::fs::File) -> Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

fn try_lock_shared(file: &std::fs::File) -> Result<bool> {
    match file.try_lock_shared() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}
