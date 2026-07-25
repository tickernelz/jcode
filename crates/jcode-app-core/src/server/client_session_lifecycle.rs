use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};

type SessionLifecycleGates = StdMutex<HashMap<String, Weak<Mutex<()>>>>;
type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<crate::agent::Agent>>>>>;

fn session_lifecycle_gates() -> &'static SessionLifecycleGates {
    static GATES: OnceLock<SessionLifecycleGates> = OnceLock::new();
    GATES.get_or_init(|| StdMutex::new(HashMap::new()))
}

fn session_lifecycle_gate(session_id: &str) -> Arc<Mutex<()>> {
    let mut gates = session_lifecycle_gates()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    gates.retain(|_, gate| gate.strong_count() > 0);

    if let Some(gate) = gates.get(session_id).and_then(Weak::upgrade) {
        return gate;
    }

    let gate = Arc::new(Mutex::new(()));
    gates.insert(session_id.to_string(), Arc::downgrade(&gate));
    gate
}

#[cfg(test)]
fn lifecycle_acquire_observers()
-> &'static StdMutex<HashMap<String, tokio::sync::oneshot::Sender<()>>> {
    static OBSERVERS: OnceLock<StdMutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>> =
        OnceLock::new();
    OBSERVERS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(test)]
pub(crate) fn observe_next_session_lifecycle_acquire(
    session_id: &str,
) -> tokio::sync::oneshot::Receiver<()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    lifecycle_acquire_observers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(session_id.to_string(), tx);
    rx
}

#[cfg(test)]
fn notify_session_lifecycle_acquire(session_id: &str) {
    if let Some(observer) = lifecycle_acquire_observers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id)
    {
        let _ = observer.send(());
    }
}

/// Serialize publication and revocation of live ownership for one session.
///
/// A resume must hold this lease from before it reads durable state until both
/// its Agent and connection ownership are visible. Disconnect cleanup holds it
/// through durable terminal publication and all session-id teardown. This makes
/// either transition happen first rather than allowing cleanup to terminalize a
/// replacement Agent that is between durable restore and connection insertion.
pub(crate) async fn acquire_session_lifecycle_lease(session_id: &str) -> OwnedMutexGuard<()> {
    #[cfg(test)]
    notify_session_lifecycle_acquire(session_id);
    session_lifecycle_gate(session_id).lock_owned().await
}

/// Acquire the source and target lifecycle domains in one stable order.
/// Reciprocal resumes therefore cannot each hold their target while waiting for
/// the other's source. Equal IDs intentionally collapse to one lease.
pub(crate) async fn acquire_session_lifecycle_pair(
    first_session_id: &str,
    second_session_id: &str,
) -> Vec<OwnedMutexGuard<()>> {
    if first_session_id == second_session_id {
        return vec![acquire_session_lifecycle_lease(first_session_id).await];
    }

    let (lower, upper) = if first_session_id < second_session_id {
        (first_session_id, second_session_id)
    } else {
        (second_session_id, first_session_id)
    };
    let lower = acquire_session_lifecycle_lease(lower).await;
    let upper = acquire_session_lifecycle_lease(upper).await;
    vec![lower, upper]
}

/// Acquire a peer dispatch barrier before taking any session lifecycle lease.
/// Connection IDs provide a cycle-free order for reciprocal requests.
pub(crate) async fn acquire_takeover_revocation_barrier(
    current_connection_id: &str,
    target_connection_id: &str,
) -> Option<OwnedMutexGuard<()>> {
    let barrier = super::client_lifecycle::connection_dispatch_barrier(target_connection_id);
    if current_connection_id < target_connection_id {
        Some(barrier.lock_owned().await)
    } else {
        let Ok(guard) = barrier.try_lock_owned() else {
            return None;
        };
        Some(guard)
    }
}

/// Remove a source only while holding the connection authority, in the global
/// connection-registry then session-map lock order.
pub(crate) async fn remove_detached_source_if_unclaimed(
    old_session_id: &str,
    client_connection_id: &str,
    source_agent: &Arc<Mutex<crate::agent::Agent>>,
    sessions: &SessionAgents,
    connections: &Arc<RwLock<HashMap<String, super::ClientConnectionInfo>>>,
) -> bool {
    let connections = connections.write().await;
    if connections
        .values()
        .any(|info| info.client_id != client_connection_id && info.session_id == old_session_id)
    {
        return false;
    }
    let mut sessions = sessions.write().await;
    let owns_source = sessions
        .get(old_session_id)
        .is_some_and(|current| Arc::ptr_eq(current, source_agent));
    if owns_source {
        sessions.remove(old_session_id);
    }
    owns_source
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn acquire_existing_takeover_barrier(
    current_connection_id: &str,
    target_session_id: &str,
    incoming_instance_id: Option<&str>,
    allow_takeover: bool,
    has_local_history: bool,
    connections: &std::sync::Arc<
        tokio::sync::RwLock<std::collections::HashMap<String, super::ClientConnectionInfo>>,
    >,
) -> Option<(String, OwnedMutexGuard<()>)> {
    let conflict = connections
        .read()
        .await
        .values()
        .find(|info| {
            info.client_id != current_connection_id && info.session_id == target_session_id
        })
        .cloned()?;
    let same = incoming_instance_id
        .zip(conflict.client_instance_id.as_deref())
        .is_some_and(|(incoming, existing)| incoming == existing);
    let distinct = incoming_instance_id
        .zip(conflict.client_instance_id.as_deref())
        .is_some_and(|(incoming, existing)| incoming != existing);
    if !allow_takeover || conflict.is_processing || !(same || (has_local_history && !distinct)) {
        return None;
    }
    acquire_takeover_revocation_barrier(current_connection_id, &conflict.client_id)
        .await
        .map(|guard| (conflict.client_id, guard))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn attach_after_cleanup_claim_waits_for_terminal_publication() {
        let session_id = "late-successor-after-cleanup-claim";
        let cleanup_lease = acquire_session_lifecycle_lease(session_id).await;

        let (waiting_tx, waiting_rx) = tokio::sync::oneshot::channel();
        let (attached_tx, mut attached_rx) = tokio::sync::oneshot::channel();
        let attach = tokio::spawn(async move {
            let _ = waiting_tx.send(());
            let _attach_lease = acquire_session_lifecycle_lease(session_id).await;
            let _ = attached_tx.send(());
        });
        waiting_rx
            .await
            .expect("successor should reach lifecycle lease acquisition");
        tokio::task::yield_now().await;

        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut attached_rx)
                .await
                .is_err(),
            "a successor must not publish ownership while terminal cleanup owns the session"
        );

        // Production cleanup retains this same lease after removing the Agent
        // and does not release it until terminal publication and id-based
        // control-plane teardown are complete.
        drop(cleanup_lease);
        tokio::time::timeout(Duration::from_secs(1), &mut attached_rx)
            .await
            .expect("successor should attach after terminal publication")
            .expect("successor task should report attachment");
        attach.await.expect("successor task should finish");
    }
}
