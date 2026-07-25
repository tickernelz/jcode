//! Single-flight coordination for OAuth token refreshes.
//!
//! Without coordination, two concurrent tasks can both observe an expired
//! access token and both call the provider token endpoint. For providers
//! that rotate refresh tokens (Anthropic, OpenAI), the slower writer then
//! persists an already-consumed refresh token, which can permanently break
//! the account until the user logs in again.
//!
//! [`single_flight`] serializes refreshes per `(provider, account)` key.
//! While holding the lock it reloads the stored credentials so that:
//! - if another task already refreshed while we waited, we return the fresh
//!   stored tokens without another network call, and
//! - if a refresh is still needed, it uses the *newest* stored refresh token
//!   rather than the possibly stale one the caller observed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use sha2::{Digest, Sha256};

static REFRESH_LOCKS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct AccountCredentialGuard {
    _process: crate::auth::account_store::CrossProcessFileLock,
    _transition: Option<crate::session::AccountTransitionFileLock>,
    _local: tokio::sync::OwnedMutexGuard<()>,
}

struct CancelTransitionWaitOnDrop(Arc<std::sync::atomic::AtomicBool>);

impl Drop for CancelTransitionWaitOnDrop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

fn lock_for(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut map = REFRESH_LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(key.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn cross_process_lock_path(key: &str) -> anyhow::Result<PathBuf> {
    let digest = format!("{:x}", Sha256::digest(key.as_bytes()));
    Ok(crate::storage::jcode_dir()?
        .join("state")
        .join("oauth-refresh")
        .join(format!("{digest}.lock")))
}

async fn acquire_account_credential_guard(key: &str) -> anyhow::Result<AccountCredentialGuard> {
    let local = lock_for(key).lock_owned().await;
    let transition = if crate::session::account_transition_admission_held() {
        None
    } else {
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_on_drop = CancelTransitionWaitOnDrop(Arc::clone(&cancelled));
        let worker_cancelled = Arc::clone(&cancelled);
        let transition = tokio::task::spawn_blocking(move || {
            crate::session::AccountTransitionFileLock::acquire_shared_cancellable(&worker_cancelled)
        })
        .await??;
        drop(cancel_on_drop);
        Some(transition)
    };
    let lock_path = cross_process_lock_path(key)?;
    let process = tokio::task::spawn_blocking(move || {
        crate::auth::account_store::CrossProcessFileLock::acquire(&lock_path)
    })
    .await??;
    Ok(AccountCredentialGuard {
        _process: process,
        _transition: transition,
        _local: local,
    })
}

/// Run a synchronous credential replacement under the same per-account and
/// account-transition leases used by rotating-token refresh. Login/import can
/// therefore never be overwritten later by a refresh that started first.
pub async fn replace_account_credentials<T, Replace>(
    key: String,
    replace: Replace,
) -> anyhow::Result<T>
where
    T: Send + 'static,
    Replace: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    let _guard = acquire_account_credential_guard(&key).await?;
    tokio::task::spawn_blocking(replace).await?
}

pub async fn replace_account_credentials_async<T, Replace, Fut>(
    key: String,
    replace: Replace,
) -> anyhow::Result<T>
where
    Replace: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let _guard = acquire_account_credential_guard(&key).await?;
    replace().await
}

/// Expiry check shared by refresh freshness probes: a token is considered
/// usable when it has at least this many milliseconds of validity left.
pub const FRESHNESS_MARGIN_MS: i64 = 60_000;

/// Returns true when `expires_at_ms` is still comfortably in the future.
pub fn expiry_is_fresh(expires_at_ms: i64) -> bool {
    expires_at_ms > chrono::Utc::now().timestamp_millis() + FRESHNESS_MARGIN_MS
}

/// Run `refresh` under a per-key async lock.
///
/// After acquiring the lock, `reload` fetches the currently stored
/// credentials. If `already_refreshed` accepts them (typically: the stored
/// tokens differ from what the caller observed and are not expired), they are
/// returned directly. Otherwise `refresh` runs with the reloaded state so it
/// can prefer the newest stored refresh token.
pub async fn single_flight<T, Reload, Fresh, Refresh, Fut>(
    key: String,
    reload: Reload,
    already_refreshed: Fresh,
    refresh: Refresh,
) -> anyhow::Result<T>
where
    Reload: FnOnce() -> Option<T>,
    Fresh: FnOnce(&T) -> bool,
    Refresh: FnOnce(Option<T>) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let _guard = acquire_account_credential_guard(&key).await?;
    let current = reload();
    if let Some(current) = current {
        if already_refreshed(&current) {
            return Ok(current);
        }
        return refresh(Some(current)).await;
    }
    refresh(None).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn external_holder_serializes_same_account_refresh() {
        let _env = crate::storage::lock_test_env();
        let home = tempfile::TempDir::new().expect("temp home");
        crate::env::set_var("JCODE_HOME", home.path());
        tokio::runtime::Runtime::new()
            .expect("test runtime")
            .block_on(async {
                let key = "test:cross-process";
                let path = cross_process_lock_path(key).expect("refresh lock path");
                std::fs::create_dir_all(path.parent().unwrap()).expect("lock parent");
                let gate = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(path)
                    .expect("open external lock");
                gate.lock().expect("hold external lock");

                let mut refresh = tokio::spawn(single_flight(
                    key.to_string(),
                    || None::<String>,
                    |_| false,
                    |_| async { Ok("refreshed".to_string()) },
                ));
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(50), &mut refresh)
                        .await
                        .is_err(),
                    "a second process must wait before redeeming the same rotating token"
                );

                drop(gate.unlock());
                assert_eq!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), refresh)
                        .await
                        .expect("refresh unblocked")
                        .expect("refresh task")
                        .expect("refresh result"),
                    "refreshed"
                );
            });
        crate::env::remove_var("JCODE_HOME");
    }

    #[test]
    fn account_transition_excludes_target_account_refresh() {
        let _env = crate::storage::lock_test_env();
        let home = tempfile::TempDir::new().expect("temp home");
        crate::env::set_var("JCODE_HOME", home.path());
        tokio::runtime::Runtime::new()
            .expect("test runtime")
            .block_on(async {
                let transition = crate::session::AccountTransitionFileLock::acquire_exclusive()
                    .expect("hold account transition");
                let mut refresh = tokio::spawn(single_flight(
                    "test:transition-target".to_string(),
                    || None::<String>,
                    |_| false,
                    |_| async { Ok("refreshed".to_string()) },
                ));
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(75), &mut refresh)
                        .await
                        .is_err(),
                    "refresh must not advance a target generation during account transition"
                );

                drop(transition);
                assert_eq!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), refresh)
                        .await
                        .expect("refresh unblocked")
                        .expect("refresh task")
                        .expect("refresh result"),
                    "refreshed"
                );
            });
        crate::env::remove_var("JCODE_HOME");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_refreshes_for_same_key_run_once() {
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let stored: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let refresh_calls = Arc::clone(&refresh_calls);
            let stored = Arc::clone(&stored);
            handles.push(tokio::spawn(async move {
                let reload_stored = Arc::clone(&stored);
                single_flight(
                    "test:same-key".to_string(),
                    move || reload_stored.lock().unwrap().clone(),
                    |current: &String| current == "refreshed",
                    move |_current| async move {
                        refresh_calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        *stored.lock().unwrap() = Some("refreshed".to_string());
                        Ok("refreshed".to_string())
                    },
                )
                .await
            }));
        }

        for handle in handles {
            let result = handle.await.expect("join").expect("refresh result");
            assert_eq!(result, "refreshed");
        }
        assert_eq!(
            refresh_calls.load(Ordering::SeqCst),
            1,
            "only the first waiter should hit the token endpoint"
        );
    }

    #[tokio::test]
    async fn refresh_uses_newest_stored_state() {
        // The caller observed an old refresh token, but a fresher one is on
        // disk (still expired, so a refresh is required). The refresh closure
        // must receive the stored state, not the caller's stale observation.
        let result = single_flight(
            "test:newest-state".to_string(),
            || Some("stored-newer-token".to_string()),
            |_current: &String| false,
            |current| async move {
                assert_eq!(current.as_deref(), Some("stored-newer-token"));
                Ok("ok".to_string())
            },
        )
        .await
        .expect("refresh result");
        assert_eq!(result, "ok");
    }

    #[tokio::test]
    async fn distinct_keys_do_not_serialize() {
        let started = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for i in 0..4 {
            let started = Arc::clone(&started);
            handles.push(tokio::spawn(async move {
                single_flight(
                    format!("test:distinct-{i}"),
                    || None::<usize>,
                    |_| false,
                    move |_| async move {
                        started.fetch_add(1, Ordering::SeqCst);
                        Ok(i)
                    },
                )
                .await
            }));
        }
        for handle in handles {
            handle.await.expect("join").expect("result");
        }
        assert_eq!(started.load(Ordering::SeqCst), 4);
    }
}
