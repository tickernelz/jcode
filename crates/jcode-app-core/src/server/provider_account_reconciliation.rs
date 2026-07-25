fn account_reconciliation_path() -> anyhow::Result<PathBuf> {
    // Account credentials and sessions are shared across server processes even
    // when each server has its own JCODE_RUNTIME_DIR. The marker and OS lock
    // must therefore live in the same JCODE_HOME namespace, never a per-runtime
    // directory.
    Ok(crate::storage::jcode_dir()?
        .join("state")
        .join(ACCOUNT_RECONCILIATION_FILE))
}

fn load_pending_account_reconciliation() -> anyhow::Result<Option<PendingAccountReconciliation>> {
    let path = account_reconciliation_path()?;
    if !path.exists() {
        return Ok(None);
    }
    // Do not use backup recovery here. A recovered old intent could switch the
    // active account backwards after the primary marker was successfully cleared.
    let marker: PendingAccountReconciliation = serde_json::from_slice(&std::fs::read(&path)?)?;
    if marker.schema_version != ACCOUNT_RECONCILIATION_SCHEMA_VERSION
        || marker.account_label.trim().is_empty()
        || marker.account_id.trim().is_empty()
        || marker.account_generation == 0
    {
        anyhow::bail!("invalid pending provider account reconciliation marker");
    }
    Ok(Some(marker))
}

pub fn ensure_no_pending_account_reconciliation() -> anyhow::Result<()> {
    match load_pending_account_reconciliation() {
        Ok(None) => Ok(()),
        Ok(Some(marker)) => anyhow::bail!(
            "Provider account transition for {:?} is pending durable session reconciliation",
            marker.runtime_key
        ),
        Err(error) => {
            anyhow::bail!("Provider account transition safety marker cannot be verified: {error}")
        }
    }
}

fn persist_pending_account_reconciliation(
    marker: &PendingAccountReconciliation,
) -> anyhow::Result<()> {
    let path = account_reconciliation_path()?;
    if let Some(parent) = path.parent() {
        crate::storage::ensure_dir(parent)?;
    }
    crate::storage::write_json(&path, marker)
}

fn clear_pending_account_reconciliation() -> anyhow::Result<()> {
    let path = account_reconciliation_path()?;
    for candidate in [&path, &path.with_extension("bak")] {
        match std::fs::remove_file(candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn pending_account_identity_matches(
    marker: &PendingAccountReconciliation,
    identity: &jcode_provider_core::ExactRuntimeIdentity,
) -> bool {
    identity.route.runtime_key == marker.runtime_key
        && identity.account_label.as_deref() == Some(marker.account_label.as_str())
        && identity.account_id.as_deref() == Some(marker.account_id.as_str())
        && identity.account_generation == Some(marker.account_generation)
}

fn legacy_session_may_use_runtime(
    session: &crate::session::Session,
    runtime_key: &jcode_provider_core::RuntimeKey,
) -> bool {
    if session.exact_runtime_identity.is_some() || session.provider_session_identity.is_some() {
        return false;
    }
    let provider = session
        .provider_key
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let api_method = session
        .route_api_method
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match runtime_key {
        jcode_provider_core::RuntimeKey::OpenAIOAuth => {
            matches!(provider.as_str(), "openai" | "openai-codex")
                || api_method.contains("openai-oauth")
                || api_method.contains("codex")
        }
        jcode_provider_core::RuntimeKey::ClaudeOAuth => {
            matches!(provider.as_str(), "claude" | "claude-code" | "anthropic")
                || api_method.contains("claude-oauth")
        }
        _ => false,
    }
}

fn active_account_identity_for_runtime(
    runtime_key: &jcode_provider_core::RuntimeKey,
) -> Option<(String, String, u64)> {
    match runtime_key {
        jcode_provider_core::RuntimeKey::ClaudeOAuth => {
            crate::auth::claude::active_account_identity()
        }
        jcode_provider_core::RuntimeKey::OpenAIOAuth => {
            crate::auth::codex::active_account_identity()
        }
        _ => None,
    }
}

fn set_active_account_for_runtime(
    runtime_key: &jcode_provider_core::RuntimeKey,
    label: &str,
) -> anyhow::Result<()> {
    match runtime_key {
        jcode_provider_core::RuntimeKey::ClaudeOAuth => {
            crate::auth::claude::set_active_account(label)
        }
        jcode_provider_core::RuntimeKey::OpenAIOAuth => {
            crate::auth::codex::set_active_account(label)
        }
        _ => anyhow::bail!("unsupported account reconciliation runtime: {runtime_key:?}"),
    }
}

fn marker_for_target_account(
    runtime_key: jcode_provider_core::RuntimeKey,
    label: &str,
    target_session_ids: Vec<String>,
) -> anyhow::Result<PendingAccountReconciliation> {
    let (account_label, account_id, account_generation) = match &runtime_key {
        jcode_provider_core::RuntimeKey::ClaudeOAuth => {
            let auth = crate::auth::claude::load_auth_file()?;
            if !auth
                .anthropic_accounts
                .iter()
                .any(|account| account.label == label)
            {
                anyhow::bail!("selected Anthropic account does not exist");
            }
            let identity = auth.account_identities.get(label).ok_or_else(|| {
                anyhow::anyhow!("selected Anthropic account has no stable non-secret identity")
            })?;
            (label.to_string(), identity.id.clone(), identity.generation)
        }
        jcode_provider_core::RuntimeKey::OpenAIOAuth => {
            let auth = crate::auth::codex::load_auth_file()?;
            if !auth
                .openai_accounts
                .iter()
                .any(|account| account.label == label)
            {
                anyhow::bail!("selected OpenAI account does not exist");
            }
            let identity = auth.account_identities.get(label).ok_or_else(|| {
                anyhow::anyhow!("selected OpenAI account has no stable non-secret identity")
            })?;
            (label.to_string(), identity.id.clone(), identity.generation)
        }
        _ => anyhow::bail!("unsupported account reconciliation runtime: {runtime_key:?}"),
    };
    Ok(PendingAccountReconciliation {
        schema_version: ACCOUNT_RECONCILIATION_SCHEMA_VERSION,
        runtime_key,
        account_label,
        account_id,
        account_generation,
        target_session_ids,
    })
}

fn durable_target_session_ids(
    runtime_key: &jcode_provider_core::RuntimeKey,
) -> anyhow::Result<BTreeSet<String>> {
    let mut targets = BTreeSet::new();
    let sessions_dir = crate::storage::jcode_dir()?.join("sessions");
    match std::fs::read_dir(sessions_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                    continue;
                }
                let Some(session_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };
                let session = crate::session::Session::load(session_id).map_err(|error| {
                    anyhow::anyhow!(
                        "cannot classify durable session {session_id} for account transition: {error}"
                    )
                })?;
                let applies = session
                    .exact_runtime_identity
                    .as_ref()
                    .is_some_and(|identity| &identity.route.runtime_key == runtime_key)
                    || session
                        .provider_session_identity
                        .as_ref()
                        .is_some_and(|identity| &identity.route.runtime_key == runtime_key)
                    || legacy_session_may_use_runtime(&session, runtime_key);
                if applies {
                    targets.insert(session_id.to_string());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(targets)
}

async fn managed_target_session_ids(
    sessions: &SessionAgents,
    current: &Arc<Mutex<Agent>>,
    runtime_key: &jcode_provider_core::RuntimeKey,
) -> anyhow::Result<Vec<String>> {
    let mut agents: Vec<_> = sessions.read().await.values().cloned().collect();
    if !agents
        .iter()
        .any(|candidate| Arc::ptr_eq(candidate, current))
    {
        agents.push(Arc::clone(current));
    }
    let mut targets = durable_target_session_ids(runtime_key)?;
    for candidate in agents {
        let agent = candidate.lock().await;
        if agent.account_transition_applies_to(runtime_key) {
            targets.insert(agent.session_id().to_string());
        }
    }
    Ok(targets.into_iter().collect())
}

/// Completion half of a local TUI account switch.
///
/// The durable marker and cross-process file lock remain owned until cached
/// provider credentials have been invalidated. Dropping this value without
/// calling [`Self::finish`] deliberately leaves the marker in place, so model
/// admission remains fail-closed and startup can roll the transition forward.
pub struct LocalAccountSwitchCompletion {
    provider: Arc<dyn Provider>,
    marker: PendingAccountReconciliation,
    _account_transition: crate::provider::AccountTransitionGuard,
    _file_lock: AccountReconciliationFileLock,
}

impl LocalAccountSwitchCompletion {
    pub async fn finish(self) -> anyhow::Result<()> {
        self.provider.invalidate_credentials().await;
        record_completed_account_transition(&self.marker)?;
        clear_pending_account_reconciliation()
    }
}

/// Prepare a local TUI account switch as a durable roll-forward transaction.
///
/// Every affected session is reconciled before the active credential changes.
/// This clears provider resume bindings and deactivates derived context while
/// retaining canonical raw messages and immutable context nodes. The marker is
/// cleared only by [`LocalAccountSwitchCompletion::finish`] after provider
/// credential caches have also been invalidated.
pub fn prepare_local_account_switch(
    provider: Arc<dyn Provider>,
    runtime_key: jcode_provider_core::RuntimeKey,
    account_label: &str,
    current_session_id: &str,
) -> anyhow::Result<(crate::session::Session, bool, LocalAccountSwitchCompletion)> {
    let account_transition = crate::provider::begin_account_transition();
    let file_lock = AccountReconciliationFileLock::try_acquire()?;
    if load_pending_account_reconciliation()?.is_some() {
        anyhow::bail!(
            "a provider account transition is already pending durable reconciliation; restart Jcode to retry it"
        );
    }
    crate::session::Session::load(current_session_id).map_err(|error| {
        anyhow::anyhow!(
            "current session {current_session_id} is not durably readable before account switch: {error}"
        )
    })?;

    // The durable session directory, not the current process's live map, is the
    // ownership boundary. Other processes fail closed on the marker and prove
    // the new exact identity again at their next admission.
    let target_session_ids = durable_target_session_ids(&runtime_key)?
        .into_iter()
        .collect::<Vec<_>>();
    let current_is_target = target_session_ids
        .iter()
        .any(|session_id| session_id == current_session_id);
    let marker = marker_for_target_account(runtime_key, account_label, target_session_ids)?;
    persist_pending_account_reconciliation(&marker)?;

    let mut errors = Vec::new();
    for session_id in &marker.target_session_ids {
        if let Err(error) = reconcile_unloaded_session(session_id, &marker) {
            errors.push(format!("{session_id}: {error}"));
        }
    }
    if !errors.is_empty() {
        anyhow::bail!(
            "account identity preparation failed for {} session(s); the durable transition marker was retained: {}",
            errors.len(),
            errors.join("; ")
        );
    }

    // Credentials are selected only after every affected durable session is
    // safe for the destination identity. Any later failure retains the marker,
    // making the transition recoverable and blocking stale-account turns.
    set_active_account_for_runtime(&marker.runtime_key, &marker.account_label).map_err(|error| {
        anyhow::anyhow!(
            "account activation failed after durable preparation; the transition marker was retained for recovery: {error}"
        )
    })?;
    let active = active_account_identity_for_runtime(&marker.runtime_key);
    if active.as_ref()
        != Some(&(
            marker.account_label.clone(),
            marker.account_id.clone(),
            marker.account_generation,
        ))
    {
        anyhow::bail!(
            "activated provider account identity does not match the durable transition marker"
        );
    }
    provider.on_auth_changed_preserve_current_provider();

    let current_session = crate::session::Session::load(current_session_id)?;
    Ok((
        current_session,
        current_is_target,
        LocalAccountSwitchCompletion {
            provider,
            marker,
            _account_transition: account_transition,
            _file_lock: file_lock,
        },
    ))
}

async fn live_agent_by_session_id(
    sessions: &SessionAgents,
    current: &Arc<Mutex<Agent>>,
    session_id: &str,
) -> Option<Arc<Mutex<Agent>>> {
    if let Some(agent) = sessions.read().await.get(session_id).cloned() {
        return Some(agent);
    }
    if current.lock().await.session_id() == session_id {
        return Some(Arc::clone(current));
    }
    None
}

async fn reconcile_pending_account_marker(
    sessions: &SessionAgents,
    current: &Arc<Mutex<Agent>>,
) -> anyhow::Result<bool> {
    let Some(marker) = load_pending_account_reconciliation()? else {
        return Ok(false);
    };
    set_active_account_for_runtime(&marker.runtime_key, &marker.account_label)?;
    let active = active_account_identity_for_runtime(&marker.runtime_key);
    if active.as_ref()
        != Some(&(
            marker.account_label.clone(),
            marker.account_id.clone(),
            marker.account_generation,
        ))
    {
        anyhow::bail!("pending provider account identity no longer matches credential storage");
    }
    // A prior attempt may have persisted the marker and selected the account but
    // crashed before refreshing provider credential caches. Retry must refresh
    // before asking each live Agent to prove the destination exact identity.
    current
        .lock()
        .await
        .provider_handle()
        .invalidate_credentials()
        .await;
    crate::agent::invalidate_all_live_agent_credentials().await;

    let mut errors = Vec::new();
    for session_id in &marker.target_session_ids {
        let result =
            if let Some(agent) = live_agent_by_session_id(sessions, current, session_id).await {
                agent
                    .lock()
                    .await
                    .reconcile_pending_account_transition(&marker.runtime_key, |identity| {
                        pending_account_identity_matches(&marker, identity)
                    })
            } else {
                reconcile_unloaded_session(session_id, &marker)
            };
        if let Err(error) = result {
            errors.push(format!("{session_id}: {error}"));
        }
    }
    if !errors.is_empty() {
        anyhow::bail!(
            "account identity reconciliation failed for {} session(s): {}",
            errors.len(),
            errors.join("; ")
        );
    }
    // The marker is the fail-closed gate. It is removed only after every target
    // has durably installed cleanup and the new exact identity.
    record_completed_account_transition(&marker)?;
    clear_pending_account_reconciliation()?;
    Ok(true)
}

fn reconcile_unloaded_session(
    session_id: &str,
    marker: &PendingAccountReconciliation,
) -> anyhow::Result<()> {
    #[cfg(test)]
    if FAIL_ACCOUNT_RECONCILIATION_SESSION
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_deref()
        == Some(session_id)
    {
        anyhow::bail!("injected provider account reconciliation failure");
    }
    let mut session = crate::session::Session::load(session_id)?;
    let applies = session
        .exact_runtime_identity
        .as_ref()
        .is_some_and(|identity| identity.route.runtime_key == marker.runtime_key)
        || session
            .provider_session_identity
            .as_ref()
            .is_some_and(|identity| identity.route.runtime_key == marker.runtime_key)
        || legacy_session_may_use_runtime(&session, &marker.runtime_key);
    if !applies {
        return Ok(());
    }
    if session
        .exact_runtime_identity
        .as_ref()
        .is_some_and(|identity| pending_account_identity_matches(marker, identity))
        && session.provider_session_id.is_none()
        && session.provider_session_identity.is_none()
        && session.compaction.is_none()
        && session.context_frontier.as_ref().is_none_or(|frontier| {
            frontier.active_node_ids.is_empty() && frontier.covered_message_count == 0
        })
    {
        return Ok(());
    }
    let Some(mut identity) = session.exact_runtime_identity.clone() else {
        // Pre-identity sessions cannot prove which account owned resume or
        // derived context. Clear it fail closed, retain canonical raw history,
        // and let a later provider restore stamp an actually observed route.
        session.provider_session_id = None;
        session.provider_session_identity = None;
        session.reset_context_graph_for_identity_transition();
        return session.save_during_account_transition();
    };
    identity.account_label = Some(marker.account_label.clone());
    identity.account_id = Some(marker.account_id.clone());
    identity.account_generation = Some(marker.account_generation);
    if !pending_account_identity_matches(marker, &identity) {
        anyhow::bail!("target session route does not match pending account runtime");
    }
    session.exact_runtime_identity = Some(identity);
    session.provider_session_id = None;
    session.provider_session_identity = None;
    session.reset_context_graph_for_identity_transition();
    session.save_during_account_transition()
}

pub(crate) fn reconcile_pending_account_transition_on_startup_sync(
    provider: &Arc<dyn Provider>,
) -> anyhow::Result<bool> {
    let _file_lock = AccountReconciliationFileLock::acquire()?;
    let Some(marker) = load_pending_account_reconciliation()? else {
        return Ok(false);
    };
    // Startup happens before agents are loaded, so reconcile every durable target
    // directly. Live Agent compaction managers seed from this cleaned state later.
    set_active_account_for_runtime(&marker.runtime_key, &marker.account_label)?;
    provider.on_auth_changed_preserve_current_provider();
    let active = active_account_identity_for_runtime(&marker.runtime_key);
    if active.as_ref()
        != Some(&(
            marker.account_label.clone(),
            marker.account_id.clone(),
            marker.account_generation,
        ))
    {
        anyhow::bail!("pending provider account identity no longer matches credential storage");
    }
    let mut errors = Vec::new();
    for session_id in &marker.target_session_ids {
        if let Err(error) = reconcile_unloaded_session(session_id, &marker) {
            errors.push(format!("{session_id}: {error}"));
        }
    }
    if !errors.is_empty() {
        anyhow::bail!(
            "startup account reconciliation failed: {}",
            errors.join("; ")
        );
    }
    record_completed_account_transition(&marker)?;
    clear_pending_account_reconciliation()?;
    Ok(true)
}

fn record_completed_account_transition(
    marker: &PendingAccountReconciliation,
) -> anyhow::Result<()> {
    crate::session::record_completed_account_transition(
        marker.runtime_key.clone(),
        &marker.account_label,
        &marker.account_id,
        marker.account_generation,
    )
}

pub fn reconcile_pending_account_transition_for_admission(
    provider: &Arc<dyn Provider>,
) -> anyhow::Result<()> {
    // Model turns already hold the shared admission lease when tools reach
    // this check. An exclusive transition cannot publish a marker while that
    // lease is held, so avoid recursively escalating the same file lock when
    // there is nothing to reconcile. Before admission, a marker racing this
    // read is caught when shared acquisition waits for the transition and the
    // caller rechecks under that lease.
    if load_pending_account_reconciliation()?.is_none() {
        return Ok(());
    }
    reconcile_pending_account_transition_on_startup_sync(provider).map_err(|error| {
        anyhow::anyhow!(
            "Provider account transition is blocked by pending durable session reconciliation: {error}"
        )
    })?;
    ensure_no_pending_account_reconciliation()
}

pub async fn reconcile_pending_account_transition_for_admission_async(
    provider: Arc<dyn Provider>,
) -> anyhow::Result<()> {
    if load_pending_account_reconciliation()?.is_none() {
        return Ok(());
    }
    tokio::task::spawn_blocking(move || {
        reconcile_pending_account_transition_for_admission(&provider)
    })
    .await?
}
