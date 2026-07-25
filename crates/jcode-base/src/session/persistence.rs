use anyhow::{Result, bail};
use chrono::Utc;
#[cfg(any(unix, windows))]
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::Instant;

use super::account_transition_persistence::{
    ACCOUNT_RECONCILIATION_FILE, AccountTransitionFileLock, account_transition_state_path,
    load_completed_account_transitions,
};
use super::journal::{PersistVectorMode, SessionJournalEntry, metadata_requires_snapshot};
use super::storage_paths::{file_len_or_zero, session_journal_path_from_snapshot, session_path};
use super::{
    ContextGraphTransaction, MAX_SESSION_JOURNAL_BYTES, RemoteStartupSessionSnapshot, Session,
    SessionStartupStub,
};
use crate::storage;

/// Outcome of replaying one session journal file.
#[derive(Debug, Default)]
struct JournalReplayStats {
    entries: usize,
    skipped_lines: usize,
    salvaged_entries: usize,
    sequence_gap: bool,
}

#[cfg(unix)]
struct SessionWriterLock(std::fs::File);

#[cfg(unix)]
impl SessionWriterLock {
    fn acquire(snapshot_path: &Path) -> Result<Self> {
        let lock_path = snapshot_path.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(file))
    }
}

#[cfg(unix)]
impl Drop for SessionWriterLock {
    fn drop(&mut self) {
        if unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) } != 0 {
            crate::logging::warn(&format!(
                "Failed to release session writer lock: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
}

#[cfg(windows)]
struct SessionWriterLock(std::fs::File);

#[cfg(windows)]
impl SessionWriterLock {
    fn acquire(snapshot_path: &Path) -> Result<Self> {
        use windows_sys::Win32::Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LockFileEx};
        use windows_sys::Win32::System::IO::OVERLAPPED;

        let lock_path = snapshot_path.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        let result = unsafe {
            LockFileEx(
                file.as_raw_handle() as _,
                LOCKFILE_EXCLUSIVE_LOCK,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if result == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(file))
    }
}

#[cfg(windows)]
impl Drop for SessionWriterLock {
    fn drop(&mut self) {
        use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
        use windows_sys::Win32::System::IO::OVERLAPPED;

        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        if unsafe { UnlockFileEx(self.0.as_raw_handle() as _, 0, 1, 0, &mut overlapped) } == 0 {
            crate::logging::warn(&format!(
                "Failed to release session writer lock: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
}

impl JournalReplayStats {
    fn is_corrupt(&self) -> bool {
        self.skipped_lines > 0
    }
}

/// Attempt to recover complete entries from a journal line that failed the
/// strict one-entry-per-line parse.
///
/// If a writer died mid-append (torn line without a trailing newline), the
/// next successful append starts writing on the same line, producing
/// `<torn json><complete entry json>\n` or `<entry json><entry json>\n`.
/// Scan object starts and stream-parse consecutive complete entries from the
/// first position that yields any. This supports both legacy `{"meta":...}`
/// and sequenced `{"sequence":...}` entries.
fn salvage_glued_journal_entries(line: &str, mut apply: impl FnMut(SessionJournalEntry)) -> usize {
    let mut salvaged = 0usize;
    let mut search_from = 0usize;
    while let Some(rel) = line.get(search_from..).and_then(|rest| rest.find('{')) {
        let candidate_start = search_from + rel;
        let mut stream = serde_json::Deserializer::from_str(&line[candidate_start..])
            .into_iter::<SessionJournalEntry>();
        let mut parsed = Vec::new();
        for item in &mut stream {
            match item {
                Ok(entry) => parsed.push(entry),
                Err(_) => break,
            }
        }
        if !parsed.is_empty() {
            salvaged += parsed.len();
            for entry in parsed {
                apply(entry);
            }
            break;
        }
        search_from = candidate_start + 1;
    }
    salvaged
}

/// Replay every contiguous parseable entry from a session journal, tolerating
/// corrupt tail lines but failing closed on later non-contiguous records.
///
/// Journals are append-only JSONL written by `append_json_line_durable`. A crash,
/// full disk, or (historically) interleaved multi-write appends can leave a
/// torn or glued line behind. The old replay loop stopped at the first parse
/// failure, silently dropping every later entry, which surfaced as "my last
/// prompt is missing" after resuming a long session. We can still heal a
/// corrupt tail because no later durable record depends on it. Once a later
/// valid record appears with a sequence gap, accepting and checkpointing that
/// tail would seal data loss, so replay quarantines the original bytes and
/// returns an error instead.
fn replay_journal_lines(
    journal_path: &Path,
    snapshot_watermark: u64,
    mut apply: impl FnMut(SessionJournalEntry),
) -> Result<JournalReplayStats> {
    let mut stats = JournalReplayStats::default();
    if !journal_path.exists() {
        return Ok(stats);
    }

    let file = std::fs::File::open(journal_path)?;
    let reader = BufReader::new(file);
    let mut current_sequence = snapshot_watermark;
    for (line_idx, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<SessionJournalEntry>(trimmed) {
            Ok(mut entry) => {
                stats.entries += 1;
                // Legacy entries had no sequence. Assign them deterministic file-order
                // sequences so the next checkpoint can cover them. A non-zero snapshot
                // watermark can only have been written after all legacy entries present
                // at that checkpoint were applied, so any sequence-less entry left beside
                // such a snapshot is stale journal-retirement residue.
                if entry.sequence == 0 {
                    if snapshot_watermark > 0 {
                        continue;
                    }
                    entry.sequence = current_sequence.saturating_add(1);
                }
                if entry.sequence > snapshot_watermark && entry.sequence == current_sequence + 1 {
                    current_sequence = entry.sequence;
                    apply(entry);
                } else if entry.sequence > current_sequence + 1 {
                    stats.sequence_gap = true;
                    break;
                }
            }
            Err(err) => {
                stats.skipped_lines += 1;
                let salvaged = salvage_glued_journal_entries(trimmed, |mut entry| {
                    if entry.sequence == 0 {
                        if snapshot_watermark > 0 {
                            return;
                        }
                        entry.sequence = current_sequence.saturating_add(1);
                    }
                    if entry.sequence > snapshot_watermark && entry.sequence == current_sequence + 1
                    {
                        current_sequence = entry.sequence;
                        apply(entry);
                    } else if entry.sequence > current_sequence + 1 {
                        stats.sequence_gap = true;
                    }
                });
                stats.entries += salvaged;
                stats.salvaged_entries += salvaged;
                crate::logging::warn(&format!(
                    "Session journal parse failed at {} line {} ({}); salvaged {} glued entr{} and continuing replay",
                    journal_path.display(),
                    line_idx + 1,
                    err,
                    salvaged,
                    if salvaged == 1 { "y" } else { "ies" }
                ));
            }
        }
    }

    if stats.sequence_gap {
        let repair_path = journal_path.with_extension("repair-required.jsonl");
        if let Err(err) = std::fs::copy(journal_path, &repair_path) {
            crate::logging::warn(&format!(
                "Failed to preserve session journal with sequence gap {} to {}: {}",
                journal_path.display(),
                repair_path.display(),
                err
            ));
        }
        bail!(
            "session journal {} has a non-contiguous sequence after snapshot watermark {}; repair required",
            journal_path.display(),
            snapshot_watermark
        );
    }

    if stats.is_corrupt() {
        crate::logging::event_warn(
            "SESSION_PERSISTENCE",
            vec![
                ("phase", "journal_replay_corruption".to_string()),
                ("path", journal_path.display().to_string()),
                ("entries_replayed", stats.entries.to_string()),
                ("lines_skipped", stats.skipped_lines.to_string()),
                ("entries_salvaged", stats.salvaged_entries.to_string()),
            ],
        );
    }

    Ok(stats)
}

impl Session {
    fn verify_writer_base_is_current(&self, snapshot_path: &Path) -> Result<()> {
        if !snapshot_path.exists() {
            return Ok(());
        }
        // Compare with the graph generation actually stored on disk. Running
        // provenance validation here would derive the same fail-closed frontier
        // transition that this writer is trying to checkpoint, making a single
        // writer appear stale against its own deterministic repair.
        let durable = Session::load_from_path_locked_with_validation(snapshot_path, false)?;
        let durable_generation = durable
            .context_frontier
            .as_ref()
            .map_or(0, |frontier| frontier.generation);
        let writer_generation = self.persist_state.context_generation;
        let stale_graph = durable_generation != writer_generation
            || durable.last_context_op_id != self.persist_state.context_op_id;
        if durable.persistence_revision != self.persistence_revision || stale_graph {
            anyhow::bail!(
                "stale session writer rejected for {} (durable revision/generation {}/{}, writer {}/{})",
                self.id,
                durable.persistence_revision,
                durable_generation,
                self.persistence_revision,
                writer_generation
            );
        }
        Ok(())
    }

    /// Validate and durably checkpoint a context graph transaction before the
    /// live session can observe it. A failed write leaves `self` byte-for-byte
    /// unchanged from the caller's perspective.
    pub fn commit_context_graph_transaction(
        &mut self,
        transaction: ContextGraphTransaction,
    ) -> Result<bool> {
        self.commit_context_graph_transaction_with_compaction(transaction, None)
    }

    /// Commit an LCM graph transaction and its legacy/provider projection in
    /// the same durable snapshot. Provider context must never observe one
    /// without the other.
    pub fn commit_context_graph_transaction_with_compaction(
        &mut self,
        transaction: ContextGraphTransaction,
        compaction: Option<super::StoredCompactionState>,
    ) -> Result<bool> {
        if !self
            .exact_runtime_identity
            .as_ref()
            .is_some_and(|identity| identity.has_verifiable_account_binding())
        {
            anyhow::bail!(
                "Context graph publication is disabled because exact account identity is opaque or incomplete"
            );
        }
        if let Some(state) = compaction.as_ref()
            && (state.compacted_count != transaction.frontier.covered_message_count
                || state.openai_encrypted_content.is_some())
        {
            anyhow::bail!("LCM projection does not match graph frontier");
        }
        let snapshot_path = session_path(&self.id)?;
        if !self.persist_state.snapshot_exists || !snapshot_path.exists() {
            // Establish the canonical raw-session baseline before appending a
            // derived graph delta. A graph transaction must never be the only
            // record from which the raw session can be reconstructed.
            self.save()?;
        }
        let mut candidate = self.clone();
        if !candidate.apply_context_transaction_inner(&transaction, true)? {
            return Ok(false);
        }
        if let Some(state) = compaction {
            candidate.compaction = Some(state);
        }
        // `save` verifies the same durable writer base again under the
        // cross-process writer lock. With a pending context transaction it uses
        // one durable journal entry containing the node delta, frontier, and
        // paired projection. Only then may the candidate become live.
        candidate.save()?;
        *self = candidate;
        Ok(true)
    }

    fn apply_journal_entry(&mut self, entry: SessionJournalEntry) {
        self.journal_sequence = self.journal_sequence.max(entry.sequence);
        let previous_compaction = self.compaction.clone();
        let has_context_transaction = entry.context_transaction.is_some();
        let paired_compaction = entry.meta.compaction.clone();
        self.apply_journal_meta(entry.meta);
        if has_context_transaction {
            // Projection and graph are one logical journal transaction. Keep
            // the previous projection until the graph delta has validated.
            self.compaction = previous_compaction;
        }
        self.messages.extend(entry.append_messages);
        self.env_snapshots.extend(entry.append_env_snapshots);
        self.memory_injections
            .extend(entry.append_memory_injections);
        self.replay_events.extend(entry.append_replay_events);
        if let Some(transaction) = entry.context_transaction {
            match self.apply_context_transaction_inner(&transaction, false) {
                Ok(_) => self.compaction = paired_compaction,
                Err(err) => crate::logging::warn(&format!(
                    "Ignoring invalid context transaction {} and its paired projection for session {}: {}",
                    transaction.op_id, self.id, err
                )),
            }
        }
        self.mark_memory_profile_dirty();
    }

    fn checkpoint_snapshot(&mut self, snapshot_path: &Path, journal_path: &Path) -> Result<()> {
        let previous_revision = self.persistence_revision;
        let previous_watermark = self.journal_watermark;
        self.persistence_revision = self.persistence_revision.saturating_add(1);
        self.journal_watermark = self.journal_sequence;
        // Publish the authoritative snapshot before refreshing its recovery
        // copy. Writing the new generation to `.bak` first is unsafe: if the
        // primary write then fails, generic recovery can expose a generation
        // whose save returned Err.
        //
        // The storage primitive can report a parent-directory fsync error after
        // its atomic rename has already published the exact bytes. Detect that
        // outcome and explicitly reconfirm either the primary or recovery copy.
        // If neither can be confirmed, return an ambiguous-publication error so
        // callers do not acknowledge or install the candidate live.
        let snapshot_bytes = serde_json::to_vec(self)?;
        let mut primary_confirmation_error = None;
        let primary_durability_confirmed = if let Err(err) =
            storage::write_bytes(snapshot_path, &snapshot_bytes)
        {
            if !std::fs::read(snapshot_path).is_ok_and(|published| published == snapshot_bytes) {
                self.persistence_revision = previous_revision;
                self.journal_watermark = previous_watermark;
                return Err(err);
            }
            crate::logging::warn(&format!(
                "Session {} checkpoint bytes were published at {}, but final durability confirmation failed: {}",
                self.id,
                snapshot_path.display(),
                err
            ));
            match jcode_storage::confirm_publication_durable(snapshot_path) {
                Ok(()) => true,
                Err(confirm_error) => {
                    crate::logging::error(&format!(
                        "Session {} checkpoint is visible but primary durability remains unconfirmed: {}",
                        self.id, confirm_error
                    ));
                    primary_confirmation_error = Some(confirm_error);
                    false
                }
            }
        } else {
            true
        };
        // Recovery redundancy is refreshed only after primary commit. Failure
        // here leaves storage::write_bytes' previous acknowledged generation as
        // `.bak`; it is cleanup/redundancy debt, not a failed primary commit.
        let backup_path = snapshot_path.with_extension("bak");
        let mut backup_durability_confirmed = false;
        let mut last_backup_error = None;
        for _ in 0..3 {
            match storage::write_bytes_without_backup(&backup_path, &snapshot_bytes) {
                Ok(()) => {
                    backup_durability_confirmed = true;
                    break;
                }
                Err(error) => last_backup_error = Some(error),
            }
        }
        if !backup_durability_confirmed {
            crate::logging::warn(&format!(
                "Session {} checkpoint committed, but recovery snapshot {} could not be refreshed after retries: {}",
                self.id,
                backup_path.display(),
                last_backup_error
                    .as_ref()
                    .map_or_else(|| "unknown error".to_string(), ToString::to_string)
            ));
        }
        if !primary_durability_confirmed && !backup_durability_confirmed {
            crate::logging::error(&format!(
                "Session {} checkpoint is published but neither primary nor recovery durability could be confirmed; retaining the journal",
                self.id
            ));
            self.persistence_revision = previous_revision;
            self.journal_watermark = previous_watermark;
            anyhow::bail!(
                "Session {} checkpoint publication is ambiguous: neither primary ({}) nor recovery ({}) durability was confirmed",
                self.id,
                primary_confirmation_error.as_ref().map_or_else(
                    || "unknown confirmation error".to_string(),
                    ToString::to_string
                ),
                last_backup_error
                    .as_ref()
                    .map_or_else(|| "unknown write error".to_string(), ToString::to_string)
            );
        } else if backup_durability_confirmed && journal_path.exists() {
            // Both primary and recovery snapshots now cover the journal. A
            // retirement failure is cleanup debt, not a failed commit. Replay
            // ignores entries through `journal_watermark`, and a later
            // checkpoint will retry removal.
            if let Err(error) = std::fs::remove_file(journal_path) {
                crate::logging::warn(&format!(
                    "Session {} checkpoint committed, but covered journal {} could not be retired: {}",
                    self.id,
                    journal_path.display(),
                    error
                ));
            }
        } else if journal_path.exists() {
            crate::logging::warn(&format!(
                "Session {} retained covered journal {} until a current-generation recovery snapshot is durable",
                self.id,
                journal_path.display()
            ));
        }
        self.reset_persist_state(true);
        Ok(())
    }

    /// After replaying a journal that contained unparseable lines, force the
    /// next `save()` to checkpoint a full snapshot (which deletes the corrupt
    /// journal) so the salvaged in-memory state becomes durable and the bad
    /// lines can never be replayed again. A best-effort copy of the corrupt
    /// journal is kept next to it for forensics.
    fn schedule_checkpoint_after_corrupt_journal(&mut self, journal_path: &Path) {
        self.mark_messages_full_dirty();
        let backup_path = journal_path.with_extension("corrupt.jsonl");
        if let Err(err) = std::fs::copy(journal_path, &backup_path) {
            crate::logging::warn(&format!(
                "Failed to back up corrupt session journal {} to {}: {}",
                journal_path.display(),
                backup_path.display(),
                err
            ));
        }
        crate::logging::warn(&format!(
            "Session {} journal {} contained corrupt lines; next save will checkpoint a full snapshot (backup at {})",
            self.id,
            journal_path.display(),
            backup_path.display()
        ));
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let _writer_lock = SessionWriterLock::acquire(path)?;
        Self::load_from_path_locked(path)
    }

    fn load_from_path_locked(path: &Path) -> Result<Self> {
        Self::load_from_path_locked_with_validation(path, true)
    }

    fn load_from_path_locked_with_validation(
        path: &Path,
        validate_context_graph: bool,
    ) -> Result<Self> {
        let load_start = Instant::now();
        let snapshot_bytes = file_len_or_zero(path);
        let snapshot_start = Instant::now();
        let mut session: Session = storage::read_json(path)?;
        let snapshot_ms = snapshot_start.elapsed().as_millis();
        let journal_path = session_journal_path_from_snapshot(path);
        let journal_bytes = file_len_or_zero(&journal_path);
        let journal_start = Instant::now();
        let replay_stats =
            replay_journal_lines(&journal_path, session.journal_watermark, |entry| {
                session.apply_journal_entry(entry);
            })?;
        let journal_entries = replay_stats.entries;
        let journal_ms = journal_start.elapsed().as_millis();
        let finalize_start = Instant::now();
        session.reset_persist_state(path.exists());
        if validate_context_graph {
            // Validate after establishing the durable baseline so fail-closed graph
            // deactivation remains marked for a full checkpoint on the next save.
            session.discard_invalid_context_graph();
        }
        session.reset_provider_messages_cache();
        session.mark_memory_profile_dirty();
        if replay_stats.is_corrupt() {
            session.schedule_checkpoint_after_corrupt_journal(&journal_path);
        }
        let finalize_ms = finalize_start.elapsed().as_millis();
        crate::logging::info(&format!(
            "[TIMING] session_load: session={}, snapshot={}ms, journal={}ms, finalize={}ms, snapshot_bytes={}, journal_bytes={}, journal_entries={}, messages={}, env_snapshots={}, replay_events={}, total={}ms",
            session.id,
            snapshot_ms,
            journal_ms,
            finalize_ms,
            snapshot_bytes,
            journal_bytes,
            journal_entries,
            session.messages.len(),
            session.env_snapshots.len(),
            session.replay_events.len(),
            load_start.elapsed().as_millis(),
        ));
        crate::logging::event_info(
            "SESSION_PERSISTENCE",
            vec![
                ("phase", "load_done".to_string()),
                ("session_id", session.id.clone()),
                ("path", path.display().to_string()),
                ("status", format!("{:?}", session.status)),
                ("messages", session.messages.len().to_string()),
                ("env_snapshots", session.env_snapshots.len().to_string()),
                ("replay_events", session.replay_events.len().to_string()),
                ("snapshot_bytes", snapshot_bytes.to_string()),
                ("journal_bytes", journal_bytes.to_string()),
                ("journal_entries", journal_entries.to_string()),
                ("snapshot_ms", snapshot_ms.to_string()),
                ("journal_ms", journal_ms.to_string()),
                ("finalize_ms", finalize_ms.to_string()),
                ("elapsed_ms", load_start.elapsed().as_millis().to_string()),
            ],
        );
        Ok(session)
    }

    pub fn load(session_id: &str) -> Result<Self> {
        let path = session_path(session_id)?;
        Self::load_from_path(&path)
    }

    /// Load only the metadata needed for remote-client startup.
    ///
    /// This intentionally skips heavyweight transcript vectors so the remote
    /// client can paint quickly while the server performs the authoritative
    /// session restore + history bootstrap.
    pub fn load_startup_stub(session_id: &str) -> Result<Self> {
        let path = session_path(session_id)?;
        let reader = BufReader::new(std::fs::File::open(&path)?);
        let stub: SessionStartupStub = serde_json::from_reader(reader)?;
        Ok(Self::session_from_startup_stub(stub))
    }

    pub fn load_for_remote_startup(session_id: &str) -> Result<Self> {
        let path = session_path(session_id)?;
        let _writer_lock = SessionWriterLock::acquire(&path)?;
        let load_start = Instant::now();
        let snapshot_bytes = file_len_or_zero(&path);
        let snapshot_start = Instant::now();
        let reader = BufReader::new(std::fs::File::open(&path)?);
        let snapshot: RemoteStartupSessionSnapshot = serde_json::from_reader(reader)?;
        let snapshot_ms = snapshot_start.elapsed().as_millis();
        let mut session = Self::session_from_remote_startup_snapshot(snapshot);
        let journal_path = session_journal_path_from_snapshot(&path);
        let journal_bytes = file_len_or_zero(&journal_path);
        let journal_start = Instant::now();
        let mut journal_entries = 0usize;
        replay_journal_lines(&journal_path, session.journal_watermark, |entry| {
            journal_entries += 1;
            session.journal_sequence = session.journal_sequence.max(entry.sequence);
            let previous_compaction = session.compaction.clone();
            let has_context_transaction = entry.context_transaction.is_some();
            let paired_compaction = entry.meta.compaction.clone();
            session.apply_journal_meta(entry.meta);
            if has_context_transaction {
                session.compaction = previous_compaction;
            }
            session.messages.extend(entry.append_messages);
            session.replay_events.extend(entry.append_replay_events);
            if let Some(transaction) = entry.context_transaction {
                match session.apply_context_transaction_inner(&transaction, false) {
                    Ok(_) => session.compaction = paired_compaction,
                    Err(err) => crate::logging::warn(&format!(
                        "Ignoring invalid context transaction {} and its paired projection for remote session {}: {}",
                        transaction.op_id, session.id, err
                    )),
                }
            }
        })?;
        let journal_ms = journal_start.elapsed().as_millis();
        let finalize_start = Instant::now();
        session.reset_persist_state(path.exists());
        // As with the authoritative loader, provenance validation must happen
        // after the durable baseline reset. Otherwise snapshot-resident invalid
        // roots are deactivated in memory and then incorrectly marked clean.
        session.discard_invalid_context_graph();
        session.reset_provider_messages_cache();
        session.mark_memory_profile_dirty();
        let finalize_ms = finalize_start.elapsed().as_millis();
        crate::logging::info(&format!(
            "[TIMING] remote_startup_load: session={}, snapshot={}ms, journal={}ms, finalize={}ms, snapshot_bytes={}, journal_bytes={}, journal_entries={}, messages={}, total={}ms",
            session.id,
            snapshot_ms,
            journal_ms,
            finalize_ms,
            snapshot_bytes,
            journal_bytes,
            journal_entries,
            session.messages.len(),
            load_start.elapsed().as_millis(),
        ));
        crate::logging::event_info(
            "SESSION_PERSISTENCE",
            vec![
                ("phase", "remote_startup_load_done".to_string()),
                ("session_id", session.id.clone()),
                ("path", path.display().to_string()),
                ("status", format!("{:?}", session.status)),
                ("messages", session.messages.len().to_string()),
                ("snapshot_bytes", snapshot_bytes.to_string()),
                ("journal_bytes", journal_bytes.to_string()),
                ("journal_entries", journal_entries.to_string()),
                ("snapshot_ms", snapshot_ms.to_string()),
                ("journal_ms", journal_ms.to_string()),
                ("finalize_ms", finalize_ms.to_string()),
                ("elapsed_ms", load_start.elapsed().as_millis().to_string()),
            ],
        );
        Ok(session)
    }

    pub fn save(&mut self) -> Result<()> {
        let _account_admission = if super::account_transition_admission_held() {
            None
        } else {
            Some(AccountTransitionFileLock::acquire_shared()?)
        };
        if account_transition_state_path(ACCOUNT_RECONCILIATION_FILE)?.exists() {
            bail!("cannot save session while provider account reconciliation is pending");
        }
        self.reconcile_completed_account_transitions()?;
        self.save_during_account_transition()
    }

    fn reconcile_completed_account_transitions(&mut self) -> Result<()> {
        let state = load_completed_account_transitions()?;
        for transition in state.transitions {
            let exact_applies = self
                .exact_runtime_identity
                .as_ref()
                .is_some_and(|identity| identity.route.runtime_key == transition.runtime_key);
            let bound_applies = self
                .provider_session_identity
                .as_ref()
                .is_some_and(|identity| identity.route.runtime_key == transition.runtime_key);
            let legacy_applies = self.exact_runtime_identity.is_none()
                && self.provider_session_identity.is_none()
                && legacy_session_may_use_runtime(self, &transition.runtime_key);
            if !exact_applies && !bound_applies && !legacy_applies {
                continue;
            }

            let exact_is_stale = exact_applies
                && self
                    .exact_runtime_identity
                    .as_ref()
                    .is_some_and(|identity| {
                        identity.account_label.as_deref() != Some(transition.account_label.as_str())
                            || identity.account_id.as_deref()
                                != Some(transition.account_id.as_str())
                            || identity
                                .account_generation
                                .is_none_or(|generation| generation < transition.account_generation)
                    });
            let opaque_or_cross_runtime_binding =
                !exact_applies && (bound_applies || legacy_applies);
            let resume_binding_is_stale = match (
                self.provider_session_id.as_ref(),
                self.provider_session_identity.as_ref(),
            ) {
                (None, None) => false,
                (Some(_), Some(binding)) => Some(binding) != self.exact_runtime_identity.as_ref(),
                _ => true,
            };
            if !exact_is_stale && !opaque_or_cross_runtime_binding && !resume_binding_is_stale {
                continue;
            }

            if exact_is_stale && let Some(identity) = self.exact_runtime_identity.as_mut() {
                identity.account_label = Some(transition.account_label.clone());
                identity.account_id = Some(transition.account_id.clone());
                identity.account_generation = Some(transition.account_generation);
            }
            self.provider_session_id = None;
            self.provider_session_identity = None;
            self.reset_context_graph_for_identity_transition();
        }
        Ok(())
    }

    /// Persist while the caller already owns the exclusive account-transition
    /// file lock. Ordinary callers must use [`Session::save`].
    #[doc(hidden)]
    pub fn save_during_account_transition(&mut self) -> Result<()> {
        self.updated_at = Utc::now();
        let path = session_path(&self.id)?;
        let _writer_lock = SessionWriterLock::acquire(&path)?;
        self.verify_writer_base_is_current(&path)?;
        let journal_path = session_journal_path_from_snapshot(&path);
        let start = std::time::Instant::now();
        let snapshot_bytes_before = file_len_or_zero(&path);
        let journal_bytes_before = file_len_or_zero(&journal_path);
        let current_meta = self.journal_meta();
        let metadata_needs_snapshot = self
            .persist_state
            .last_meta
            .as_ref()
            .is_some_and(|prev| metadata_requires_snapshot(prev, &current_meta));
        let vectors_need_snapshot = !self.persist_state.snapshot_exists
            || self.persist_state.messages_mode == PersistVectorMode::Full
            || self.persist_state.env_snapshots_mode == PersistVectorMode::Full
            || self.persist_state.memory_injections_mode == PersistVectorMode::Full
            || self.persist_state.replay_events_mode == PersistVectorMode::Full
            || (self.persist_state.pending_context_transaction.is_none()
                && self.context_nodes.len() != self.persist_state.context_nodes_len)
            || self.messages.len() < self.persist_state.messages_len
            || self.env_snapshots.len() < self.persist_state.env_snapshots_len
            || self.memory_injections.len() < self.persist_state.memory_injections_len
            || self.replay_events.len() < self.persist_state.replay_events_len;

        let delta_messages = self
            .messages
            .len()
            .saturating_sub(self.persist_state.messages_len);
        let delta_env_snapshots = self
            .env_snapshots
            .len()
            .saturating_sub(self.persist_state.env_snapshots_len);
        let delta_memory_injections = self
            .memory_injections
            .len()
            .saturating_sub(self.persist_state.memory_injections_len);
        let delta_replay_events = self
            .replay_events
            .len()
            .saturating_sub(self.persist_state.replay_events_len);
        let strict_context_append = self.persist_state.pending_context_transaction.is_some();
        let (
            result,
            save_mode,
            entry_build_ms,
            append_ms,
            journal_stat_ms,
            checkpoint_ms,
            journal_bytes_after,
        ) = if metadata_needs_snapshot || vectors_need_snapshot {
            let checkpoint_start = Instant::now();
            let result = self.checkpoint_snapshot(&path, &journal_path);
            let checkpoint_ms = checkpoint_start.elapsed().as_millis();
            let journal_bytes_after = file_len_or_zero(&journal_path);
            (
                result,
                "snapshot",
                0,
                0,
                0,
                checkpoint_ms,
                journal_bytes_after,
            )
        } else {
            let entry_build_start = Instant::now();
            let next_revision = self.persistence_revision.saturating_add(1);
            let mut entry_meta = current_meta.clone();
            entry_meta.persistence_revision = Some(next_revision);
            let entry = SessionJournalEntry {
                sequence: self.journal_sequence.saturating_add(1),
                meta: entry_meta,
                append_messages: self.messages[self.persist_state.messages_len..].to_vec(),
                append_env_snapshots: self.env_snapshots[self.persist_state.env_snapshots_len..]
                    .to_vec(),
                append_memory_injections: self.memory_injections
                    [self.persist_state.memory_injections_len..]
                    .to_vec(),
                append_replay_events: self.replay_events[self.persist_state.replay_events_len..]
                    .to_vec(),
                context_transaction: self.persist_state.pending_context_transaction.clone(),
            };
            let entry_build_ms = entry_build_start.elapsed().as_millis();
            let append_start = Instant::now();
            let append_result = storage::append_json_line_durable(&journal_path, &entry);
            let append_ms = append_start.elapsed().as_millis();
            match append_result {
                Ok(()) => {
                    self.journal_sequence = entry.sequence;
                    self.persistence_revision = next_revision;
                    self.reset_persist_state(true);
                    let journal_stat_start = Instant::now();
                    let journal_bytes_after = file_len_or_zero(&journal_path);
                    let journal_stat_ms = journal_stat_start.elapsed().as_millis();
                    if journal_bytes_after > MAX_SESSION_JOURNAL_BYTES {
                        let checkpoint_start = Instant::now();
                        if let Err(error) = self.checkpoint_snapshot(&path, &journal_path) {
                            crate::logging::warn(&format!(
                                "Session {} journal is durable, but deferred checkpoint failed: {}",
                                self.id, error
                            ));
                        }
                        let checkpoint_ms = checkpoint_start.elapsed().as_millis();
                        let journal_bytes_after = file_len_or_zero(&journal_path);
                        (
                            Ok(()),
                            "append+checkpoint",
                            entry_build_ms,
                            append_ms,
                            journal_stat_ms,
                            checkpoint_ms,
                            journal_bytes_after,
                        )
                    } else {
                        (
                            Ok(()),
                            "append",
                            entry_build_ms,
                            append_ms,
                            journal_stat_ms,
                            0,
                            journal_bytes_after,
                        )
                    }
                }
                Err(err) if strict_context_append => {
                    crate::logging::warn(&format!(
                        "Strict context journal append failed for {}: {}",
                        self.id, err
                    ));
                    (
                        Err(err),
                        "context_append_failed",
                        entry_build_ms,
                        append_ms,
                        0,
                        0,
                        file_len_or_zero(&journal_path),
                    )
                }
                Err(err) => {
                    crate::logging::warn(&format!(
                        "Session journal append failed for {} ({}); checkpointing full snapshot",
                        self.id, err
                    ));
                    let checkpoint_start = Instant::now();
                    let result = self.checkpoint_snapshot(&path, &journal_path);
                    let checkpoint_ms = checkpoint_start.elapsed().as_millis();
                    let journal_bytes_after = file_len_or_zero(&journal_path);
                    (
                        result,
                        "append_failed_fallback_snapshot",
                        entry_build_ms,
                        append_ms,
                        0,
                        checkpoint_ms,
                        journal_bytes_after,
                    )
                }
            }
        };
        let elapsed = start.elapsed();
        let snapshot_bytes_after = file_len_or_zero(&path);
        let result_ok = result.is_ok();
        if elapsed.as_millis() > 50 {
            crate::logging::info(&format!(
                "Session save slow: total={:.0}ms mode={} metadata_snapshot={} vectors_snapshot={} entry_build={}ms append={}ms journal_stat={}ms checkpoint={}ms messages={} delta_messages={} delta_env_snapshots={} delta_memory_injections={} delta_replay_events={} snapshot_bytes_before={} journal_bytes_before={} journal_bytes_after={}",
                elapsed.as_secs_f64() * 1000.0,
                save_mode,
                metadata_needs_snapshot,
                vectors_need_snapshot,
                entry_build_ms,
                append_ms,
                journal_stat_ms,
                checkpoint_ms,
                self.messages.len(),
                delta_messages,
                delta_env_snapshots,
                delta_memory_injections,
                delta_replay_events,
                snapshot_bytes_before,
                journal_bytes_before,
                journal_bytes_after,
            ));
        }
        let mut fields = vec![
            ("phase", "save_done".to_string()),
            ("session_id", self.id.clone()),
            ("path", path.display().to_string()),
            ("status", format!("{:?}", self.status)),
            ("result", if result_ok { "ok" } else { "error" }.to_string()),
            ("save_mode", save_mode.to_string()),
            ("metadata_snapshot", metadata_needs_snapshot.to_string()),
            ("vectors_snapshot", vectors_need_snapshot.to_string()),
            ("messages", self.messages.len().to_string()),
            ("delta_messages", delta_messages.to_string()),
            ("delta_env_snapshots", delta_env_snapshots.to_string()),
            (
                "delta_memory_injections",
                delta_memory_injections.to_string(),
            ),
            ("delta_replay_events", delta_replay_events.to_string()),
            ("snapshot_bytes_before", snapshot_bytes_before.to_string()),
            ("snapshot_bytes_after", snapshot_bytes_after.to_string()),
            ("journal_bytes_before", journal_bytes_before.to_string()),
            ("journal_bytes_after", journal_bytes_after.to_string()),
            ("entry_build_ms", entry_build_ms.to_string()),
            ("append_ms", append_ms.to_string()),
            ("journal_stat_ms", journal_stat_ms.to_string()),
            ("checkpoint_ms", checkpoint_ms.to_string()),
            ("elapsed_ms", elapsed.as_millis().to_string()),
        ];
        if let Err(error) = &result {
            fields.push(("error", crate::util::format_error_chain(error)));
            crate::logging::event_warn("SESSION_PERSISTENCE", fields);
        } else {
            crate::logging::event_info("SESSION_PERSISTENCE", fields);
        }
        result
    }
}

fn legacy_session_may_use_runtime(
    session: &Session,
    runtime_key: &jcode_provider_core::RuntimeKey,
) -> bool {
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

#[cfg(test)]
mod tests {
    use super::SessionWriterLock;
    use std::time::Duration;

    #[test]
    fn session_writer_lock_serializes_contenders() {
        let dir = tempfile::tempdir().expect("temp session directory");
        let snapshot = dir.path().join("session.json");
        let first = SessionWriterLock::acquire(&snapshot).expect("first writer lock");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let contender_snapshot = snapshot.clone();
        let contender = std::thread::spawn(move || {
            started_tx.send(()).expect("started signal");
            let lock = SessionWriterLock::acquire(&contender_snapshot);
            acquired_tx.send(lock.is_ok()).expect("acquired signal");
            lock
        });

        started_rx.recv().expect("contender started");
        assert!(
            acquired_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err()
        );
        drop(first);
        assert!(
            acquired_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("contender should acquire after release")
        );
        contender
            .join()
            .expect("contender thread")
            .expect("second writer lock");
    }
}
