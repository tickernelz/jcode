use super::*;
use std::time::Duration;

#[tokio::test]
async fn connected_session_snapshot_releases_connections_before_waiting_for_sessions() {
    let sessions = Arc::new(RwLock::new(HashMap::new()));
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));

    let sessions_gate = sessions.write().await;
    let snapshot = connected_session_snapshot(&sessions, &client_connections, &swarm_members);
    tokio::pin!(snapshot);
    let completed_early = tokio::select! {
        _ = &mut snapshot => true,
        _ = tokio::time::sleep(Duration::from_millis(20)) => false,
    };
    assert!(!completed_early, "snapshot unexpectedly completed");

    let connections_result =
        tokio::time::timeout(Duration::from_millis(100), client_connections.write()).await;
    assert!(
        connections_result.is_ok(),
        "debug snapshot retained connections while waiting for sessions: {:?}",
        connections_result.as_ref().err()
    );
    let Ok(connections_guard) = connections_result else {
        return;
    };
    drop(connections_guard);

    drop(sessions_gate);
    let snapshot_result = tokio::time::timeout(Duration::from_secs(1), &mut snapshot).await;
    assert!(
        snapshot_result.is_ok(),
        "debug snapshot deadlocked: {:?}",
        snapshot_result.as_ref().err()
    );
    let Ok((connected_agents, members)) = snapshot_result else {
        return;
    };
    assert!(connected_agents.is_empty());
    assert!(members.is_empty());
}

#[test]
fn spawned_swarm_agent_count_only_includes_live_owned_sessions() {
    let live_session_ids = HashSet::from([
        "root".to_string(),
        "worker-running".to_string(),
        "worker-ready".to_string(),
    ]);
    let spawned_session_ids = [
        "worker-running".to_string(),
        "worker-ready".to_string(),
        "worker-stale".to_string(),
    ];

    assert_eq!(
        count_live_spawned_swarm_agents(&live_session_ids, spawned_session_ids.iter()),
        2
    );
}

#[test]
fn memory_incident_classifies_runaway_live_sessions_before_allocator_retention() {
    let decision = classify_memory_incident(MemoryIncidentMetrics {
        pss_bytes: 4 * 1024 * 1024 * 1024,
        pss_growth_bytes: 3 * 1024 * 1024 * 1024,
        allocator_live_bytes: 3_800 * 1024 * 1024,
        allocator_retained_resident_bytes: 300 * 1024 * 1024,
        live_sessions: 1_145,
        headless_live_sessions: 1_140,
        connected_clients: 5,
    });

    assert_eq!(decision.severity, "critical");
    assert_eq!(decision.primary_cause, "runaway_live_session_population");
    assert_eq!(decision.confidence, "high");
}

#[test]
fn memory_incident_classifies_allocator_retention_when_live_heap_is_small() {
    let decision = classify_memory_incident(MemoryIncidentMetrics {
        pss_bytes: 1_500 * 1024 * 1024,
        pss_growth_bytes: 400 * 1024 * 1024,
        allocator_live_bytes: 500 * 1024 * 1024,
        allocator_retained_resident_bytes: 600 * 1024 * 1024,
        live_sessions: 8,
        headless_live_sessions: 3,
        connected_clients: 5,
    });

    assert_eq!(decision.severity, "warning");
    assert_eq!(decision.primary_cause, "allocator_retention");
    assert_eq!(decision.confidence, "high");
}

#[test]
fn memory_incident_reports_healthy_baseline() {
    let decision = classify_memory_incident(MemoryIncidentMetrics {
        pss_bytes: 220 * 1024 * 1024,
        pss_growth_bytes: 12 * 1024 * 1024,
        allocator_live_bytes: 150 * 1024 * 1024,
        allocator_retained_resident_bytes: 20 * 1024 * 1024,
        live_sessions: 5,
        headless_live_sessions: 1,
        connected_clients: 4,
    });

    assert_eq!(decision.severity, "healthy");
    assert_eq!(decision.primary_cause, "within_normal_operating_range");
}
