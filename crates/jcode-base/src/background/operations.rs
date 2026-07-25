use super::*;

impl BackgroundTaskManager {
    pub async fn update_progress(
        &self,
        task_id: &str,
        progress: BackgroundTaskProgress,
    ) -> Result<Option<TaskStatusFile>> {
        self.update_progress_with_event_kind(task_id, progress, BackgroundTaskEventKind::Progress)
            .await
    }

    /// Record an explicit checkpoint for an existing background task.
    pub async fn update_checkpoint(
        &self,
        task_id: &str,
        progress: BackgroundTaskProgress,
    ) -> Result<Option<TaskStatusFile>> {
        self.update_progress_with_event_kind(task_id, progress, BackgroundTaskEventKind::Checkpoint)
            .await
    }

    async fn update_progress_with_event_kind(
        &self,
        task_id: &str,
        progress: BackgroundTaskProgress,
        event_kind: BackgroundTaskEventKind,
    ) -> Result<Option<TaskStatusFile>> {
        let status_path = self.status_path_for(task_id);
        let Some(mut status) = self.read_status_file(&status_path).await else {
            return Ok(None);
        };

        let progress = progress.normalize();
        if let Some(existing) = status.progress.as_ref() {
            if progress_equivalent(existing, &progress) {
                return Ok(Some(status));
            }

            let existing_is_more_determinate = existing.percent.is_some()
                || matches!((existing.current, existing.total), (_, Some(total)) if total > 0);
            let new_is_less_determinate = progress.percent.is_none()
                && !matches!((progress.current, progress.total), (_, Some(total)) if total > 0);
            if existing_is_more_determinate
                && new_is_less_determinate
                && matches!(progress.source, BackgroundTaskProgressSource::ParsedOutput)
            {
                return Ok(Some(status));
            }
        }

        status.progress = Some(progress.clone());
        push_task_event(
            &mut status,
            progress_event_record(event_kind, progress.clone()),
        );
        self.write_status_file(&status_path, &status).await;

        Bus::global().publish(BusEvent::BackgroundTaskProgress(
            BackgroundTaskProgressEvent {
                task_id: status.task_id.clone(),
                tool_name: status.tool_name.clone(),
                display_name: status.display_name.clone(),
                session_id: status.session_id.clone(),
                progress,
            },
        ));

        Ok(Some(status))
    }

    /// Update delivery behavior for an existing background task.
    ///
    /// This supports retroactively enabling notify/wake after the task was already started.
    pub async fn update_delivery(
        &self,
        task_id: &str,
        notify: bool,
        wake: bool,
    ) -> Result<Option<TaskStatusFile>> {
        let (notify, wake) = normalize_delivery(notify, wake);
        let status_path = self.status_path_for(task_id);
        let Some(mut status) = self.read_status_file(&status_path).await else {
            return Ok(None);
        };
        status.notify = notify;
        status.wake = wake;
        let event_status = status.status.clone();
        let event_exit_code = status.exit_code;
        let event_progress = status.progress.clone();
        push_task_event(
            &mut status,
            BackgroundTaskEventRecord {
                kind: BackgroundTaskEventKind::DeliveryUpdated,
                timestamp: Utc::now().to_rfc3339(),
                message: Some(format!("notify={}, wake={}", notify, wake)),
                status: Some(event_status),
                exit_code: event_exit_code,
                progress: event_progress,
            },
        );
        self.write_status_file(&status_path, &status).await;

        if let Some(task) = self.tasks.read().await.get(task_id) {
            let _ = task.delivery_flags.send((notify, wake));
        }

        Ok(Some(status))
    }

    /// Cancel a running task
    pub async fn cancel(&self, task_id: &str) -> Result<bool> {
        self.cancel_with_grace(task_id, std::time::Duration::from_millis(400))
            .await
    }

    /// Cancel a running task, allowing detached processes a configurable grace period
    /// between TERM and KILL on Unix.
    pub async fn cancel_with_grace(
        &self,
        task_id: &str,
        _graceful_timeout: std::time::Duration,
    ) -> Result<bool> {
        let mut tasks = self.tasks.write().await;
        if let Some(task) = tasks.remove(task_id) {
            task.handle.abort();

            // Update status file
            let (notify_flag, wake_flag) = *task.delivery_flags.borrow();
            let mut final_status = TaskStatusFile {
                task_id: task.task_id,
                tool_name: task.tool_name,
                display_name: task.display_name,
                session_id: task.session_id,
                status: BackgroundTaskStatus::Failed,
                exit_code: None,
                error: Some("Cancelled by user".to_string()),
                started_at: task.started_at_rfc3339,
                completed_at: Some(chrono::Utc::now().to_rfc3339()),
                duration_secs: Some(task.started_at.elapsed().as_secs_f64()),
                pid: None,
                owner_pid: Some(std::process::id()),
                owner_instance: Some(model::process_instance_token().to_string()),
                detached: false,
                notify: notify_flag,
                wake: wake_flag,
                progress: None,
                event_history: Vec::new(),
            };
            let event_status = final_status.status.clone();
            let event_exit_code = final_status.exit_code;
            let event_error = final_status.error.clone();
            push_task_event(
                &mut final_status,
                terminal_event_record(event_status, event_exit_code, event_error.as_deref()),
            );
            if let Ok(json) = serde_json::to_string_pretty(&final_status) {
                let _ = fs::write(&task.status_path, json).await;
            }

            Ok(true)
        } else {
            drop(tasks);

            let status_path = self.status_path_for(task_id);
            let Some(mut status) = self.read_status_file(&status_path).await else {
                return Ok(false);
            };
            status = self
                .finalize_detached_status_if_needed(status, &status_path)
                .await;
            if status.status != BackgroundTaskStatus::Running || !status.detached {
                return Ok(false);
            }

            let Some(pid) = status.pid else {
                return Ok(false);
            };

            #[cfg(unix)]
            {
                let _ = crate::platform::signal_detached_process_group(pid, libc::SIGTERM);
                tokio::time::sleep(_graceful_timeout).await;
                if crate::platform::is_process_running(pid) {
                    let _ = crate::platform::signal_detached_process_group(pid, libc::SIGKILL);
                }
            }
            #[cfg(windows)]
            {
                let _ = crate::platform::signal_detached_process_group(pid, 0);
            }

            let completed_at = Utc::now();
            status.status = BackgroundTaskStatus::Failed;
            status.exit_code = None;
            status.error = Some("Cancelled by user".to_string());
            status.completed_at = Some(completed_at.to_rfc3339());
            status.duration_secs = Self::status_duration_secs(&status.started_at, completed_at);
            let event_status = status.status.clone();
            let event_exit_code = status.exit_code;
            let event_error = status.error.clone();
            push_task_event(
                &mut status,
                terminal_event_record(event_status, event_exit_code, event_error.as_deref()),
            );
            self.write_status_file(&status_path, &status).await;
            Ok(true)
        }
    }

    /// Abort every live in-process task before an exec-based server reload.
    ///
    /// `exec` replaces the process image without running destructors, so
    /// without this the spawned task futures simply vanish: their
    /// `kill_on_drop` children (e.g. cargo builds) are never killed and keep
    /// running orphaned, and their status files stay `Running` until the next
    /// process's reconcile sweep happens to notice. Aborting the handles here
    /// drops the futures (killing children) and persisting a terminal
    /// `Failed` status makes the interruption deterministic and immediately
    /// visible to `bg wait`/`bg status` and self-dev queue reconciliation.
    ///
    /// Returns the number of tasks finalized.
    pub async fn abort_live_tasks_for_reload(&self) -> usize {
        let tasks: Vec<RunningTask> = {
            let mut map = self.tasks.write().await;
            map.drain().map(|(_, task)| task).collect()
        };
        let mut finalized = 0;

        for task in tasks {
            task.handle.abort();
            // Wait (bounded) for the aborted future to actually drop, so
            // kill_on_drop children are killed before the upcoming exec.
            let _ = tokio::time::timeout(Duration::from_secs(2), task.handle).await;

            let (notify_flag, wake_flag) = *task.delivery_flags.borrow();
            let prior_status = self.read_status_file(&task.status_path).await;
            // If the task won the race and finished naturally, keep its real
            // terminal status instead of stamping it as interrupted.
            if prior_status
                .as_ref()
                .is_some_and(|status| status.status != BackgroundTaskStatus::Running)
            {
                continue;
            }
            let error = "Interrupted by server reload: the owning server process was replaced before the task finished".to_string();
            let mut final_status = TaskStatusFile {
                task_id: task.task_id,
                tool_name: task.tool_name,
                display_name: prior_status
                    .as_ref()
                    .and_then(|status| status.display_name.clone())
                    .or(task.display_name),
                session_id: task.session_id,
                status: BackgroundTaskStatus::Failed,
                exit_code: None,
                error: Some(error.clone()),
                started_at: task.started_at_rfc3339,
                completed_at: Some(chrono::Utc::now().to_rfc3339()),
                duration_secs: Some(task.started_at.elapsed().as_secs_f64()),
                pid: None,
                owner_pid: Some(std::process::id()),
                owner_instance: Some(model::process_instance_token().to_string()),
                detached: false,
                notify: notify_flag,
                wake: wake_flag,
                progress: prior_status
                    .as_ref()
                    .and_then(|status| status.progress.clone()),
                event_history: prior_status
                    .map(|status| status.event_history)
                    .unwrap_or_default(),
            };
            push_task_event(
                &mut final_status,
                terminal_event_record(BackgroundTaskStatus::Failed, None, Some(&error)),
            );
            self.write_status_file(&task.status_path, &final_status)
                .await;
            finalized += 1;
        }

        finalized
    }

    /// Clean up old task files (older than specified hours)
    pub async fn cleanup(&self, max_age_hours: u64) -> Result<usize> {
        Ok(self
            .cleanup_filtered(max_age_hours, &std::collections::HashSet::new(), false)
            .await?
            .removed_files)
    }

    /// Clean up old task files, skipping running tasks and optionally filtering by status.
    pub async fn cleanup_filtered(
        &self,
        max_age_hours: u64,
        status_filter: &std::collections::HashSet<&str>,
        dry_run: bool,
    ) -> Result<BackgroundCleanupResult> {
        let mut result = BackgroundCleanupResult {
            matched_files: 0,
            removed_files: 0,
            skipped_running_files: 0,
        };
        let cutoff =
            std::time::SystemTime::now() - std::time::Duration::from_secs(max_age_hours * 3600);

        if let Ok(mut entries) = fs::read_dir(&self.output_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                let Ok(metadata) = fs::metadata(&path).await else {
                    continue;
                };
                let Ok(modified) = metadata.modified() else {
                    continue;
                };
                if modified >= cutoff {
                    continue;
                }

                let mut associated_status = None;
                if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    associated_status = self.read_status_file(&path).await;
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("output")
                    && let Some(task_id) = path.file_stem().and_then(|stem| stem.to_str())
                {
                    associated_status = self.status(task_id).await;
                }

                if let Some(status) = associated_status.as_ref() {
                    if status.status == BackgroundTaskStatus::Running {
                        result.skipped_running_files += 1;
                        continue;
                    }
                    let status_label = match status.status {
                        BackgroundTaskStatus::Running => "running",
                        BackgroundTaskStatus::Completed => "completed",
                        BackgroundTaskStatus::Superseded => "superseded",
                        BackgroundTaskStatus::Failed => "failed",
                    };
                    if !status_filter.is_empty() && !status_filter.contains(status_label) {
                        continue;
                    }
                } else if !status_filter.is_empty() {
                    continue;
                }

                result.matched_files += 1;
                if !dry_run {
                    let _ = fs::remove_file(&path).await;
                    result.removed_files += 1;
                }
            }
        }

        if dry_run {
            result.removed_files = result.matched_files;
        }

        Ok(result)
    }

    /// Best-effort synchronous snapshot of currently running tasks.
    /// This avoids async calls in render paths.
    pub fn running_snapshot(&self) -> (usize, Vec<String>, Option<RunningBackgroundProgress>) {
        let Ok(tasks) = self.tasks.try_read() else {
            return (0, Vec::new(), None);
        };

        let mut rows: Vec<RunningBackgroundProgress> = Vec::new();
        for task in tasks.values() {
            let status = std::fs::read_to_string(&task.status_path)
                .ok()
                .and_then(|content| serde_json::from_str::<TaskStatusFile>(&content).ok());
            let progress = status.as_ref().and_then(|status| status.progress.clone());
            let label = status
                .as_ref()
                .and_then(|status| status.display_name.clone())
                .or_else(|| task.display_name.clone())
                .unwrap_or_else(|| task.tool_name.clone());

            rows.push(RunningBackgroundProgress {
                task_id: task.task_id.clone(),
                tool_name: task.tool_name.clone(),
                label,
                detail: progress.map(|progress| format_progress_display(&progress, 10)),
            });
        }

        rows.sort_by(|a, b| b.task_id.cmp(&a.task_id));
        let latest = rows.iter().find(|row| row.detail.is_some()).cloned();

        (
            tasks.len(),
            rows.iter().map(|row| row.label.clone()).collect(),
            latest,
        )
    }

    /// Best-effort synchronous lookup of detached tasks that are still running
    /// for a specific session.
    ///
    /// This is primarily used during self-dev reload recovery, where the new
    /// process needs to remind the agent that a previous `bash` command was
    /// persisted into the background instead of being interrupted.
    pub fn persisted_detached_running_tasks_for_session(
        &self,
        session_id: &str,
    ) -> Vec<TaskStatusFile> {
        let mut matches = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.output_dir) else {
            return matches;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }

            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(status) = serde_json::from_str::<TaskStatusFile>(&content) else {
                continue;
            };

            if status.session_id != session_id
                || status.status != BackgroundTaskStatus::Running
                || !status.detached
            {
                continue;
            }

            let Some(pid) = status.pid else {
                continue;
            };

            if crate::platform::is_process_running(pid) {
                matches.push(status);
            }
        }

        matches.sort_by(|a, b| a.task_id.cmp(&b.task_id));
        matches
    }
}
