#[cfg(all(test, windows))]
mod windows_hardening_tests {
    use crate::*;

    #[test]
    fn first_path_starts_one_worker_and_repeated_paths_are_coalesced() {
        let mut state = SecretHardenState::default();
        let now = Instant::now();
        let directory = Path::new(r"C:\Users\test\.jcode");
        let file = directory.join("auth.json");

        assert!(state.enqueue(directory, true, now));
        assert!(!state.enqueue(directory, true, now));
        assert!(!state.enqueue(&file, false, now));
        assert!(state.worker_running);
        assert_eq!(state.pending_directories.len(), 1);
        assert_eq!(state.pending_files.len(), 1);
    }

    #[test]
    fn recently_attempted_paths_are_not_requeued() {
        let mut state = SecretHardenState::default();
        let attempted_at = Instant::now();
        let file = PathBuf::from(r"C:\Users\test\.jcode\auth.json");
        state
            .files
            .insert(file.clone(), SecretHardenAttempt::Succeeded(attempted_at));

        assert!(!state.enqueue(&file, false, attempted_at));
        assert!(!state.worker_running);
        assert!(state.pending_files.is_empty());
    }

    #[test]
    fn failed_paths_retry_after_shorter_backoff() {
        let mut state = SecretHardenState::default();
        let attempted_at = Instant::now();
        let file = PathBuf::from(r"C:\Users\test\.jcode\auth.json");
        state
            .files
            .insert(file.clone(), SecretHardenAttempt::Failed(attempted_at));

        assert!(!state.enqueue(&file, false, attempted_at));
        let retry_at = attempted_at + SECRET_HARDEN_FAILURE_BACKOFF;
        assert!(state.enqueue(&file, false, retry_at));
    }

    #[test]
    fn in_flight_paths_are_not_requeued() {
        let mut state = SecretHardenState::default();
        let now = Instant::now();
        let file = PathBuf::from(r"C:\Users\test\.jcode\auth.json");
        state
            .files
            .insert(file.clone(), SecretHardenAttempt::InFlight);

        assert!(!state.enqueue(&file, false, now));
        assert!(state.pending_files.is_empty());
    }
}

#[cfg(test)]
mod env_file_tests {}

#[cfg(test)]
mod storage_tests {
    use crate::*;
    #[test]
    fn env_upsert_rejects_key_and_value_injection() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("provider.env");

        assert!(upsert_env_file_value(&path, "SAFE_KEY", Some("safe-value")).is_ok());
        assert_eq!(
            std::fs::read_to_string(&path).expect("saved env"),
            "SAFE_KEY=safe-value\n"
        );
        assert!(upsert_env_file_value(&path, "SAFE_KEY\nINJECTED", Some("x")).is_err());
        assert!(upsert_env_file_value(&path, "SAFE_KEY", Some("x\nINJECTED=y")).is_err());
        assert!(upsert_env_file_value(&path, "BAD=KEY", Some("x")).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).expect("unchanged env"),
            "SAFE_KEY=safe-value\n"
        );
    }
}

#[cfg(test)]
mod atomic_write_tests {
    use crate::*;

    #[test]
    fn replacement_keeps_new_primary_and_previous_backup() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("state.json");
        write_json(&path, &serde_json::json!({"generation": 1})).expect("first write");
        write_json(&path, &serde_json::json!({"generation": 2})).expect("replacement write");

        let primary: serde_json::Value = read_json(&path).expect("primary");
        let backup: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.with_extension("bak")).expect("backup"))
                .expect("backup json");
        assert_eq!(primary["generation"], 2);
        assert_eq!(backup["generation"], 1);
    }

    #[test]
    fn explicit_recovery_copy_replaces_bak_path_without_rotating_it() {
        let temp = tempfile::tempdir().expect("temp dir");
        let backup = temp.path().join("state.bak");
        write_bytes_without_backup(&backup, br#"{"generation":1}"#).expect("first recovery copy");
        write_bytes_without_backup(&backup, br#"{"generation":2}"#)
            .expect("replacement recovery copy");

        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&backup).expect("recovery copy"))
                .expect("valid recovery json");
        assert_eq!(value["generation"], 2);
        assert!(!temp.path().join("state.bak.bak").exists());
    }

    #[test]
    fn replacement_never_exposes_missing_or_torn_primary_to_raw_readers() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("state.json");
        write_json(&path, &serde_json::json!({"generation": 0})).expect("initial write");

        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            for generation in 1..=100u64 {
                write_json(&writer_path, &serde_json::json!({"generation": generation}))
                    .expect("replacement write");
            }
        });

        while !writer.is_finished() {
            let bytes = std::fs::read(&path).expect("published primary must never disappear");
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).expect("published primary must never be torn");
            assert!(value["generation"].as_u64().is_some());
            std::thread::yield_now();
        }
        writer.join().expect("writer thread");
        let final_value: serde_json::Value = read_json(&path).expect("final primary");
        assert_eq!(final_value["generation"], 100);
    }

    #[test]
    fn missing_primary_recovers_from_valid_backup() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("state.json");
        let backup = path.with_extension("bak");
        std::fs::write(&backup, br#"{"generation":7}"#).expect("backup write");

        let recovered: serde_json::Value = read_json(&path).expect("backup recovery");
        assert_eq!(recovered["generation"], 7);
        assert!(
            path.exists(),
            "successful recovery should restore the primary"
        );
        assert_eq!(
            std::fs::read_to_string(&backup).expect("backup retained"),
            r#"{"generation":7}"#
        );
    }

    #[test]
    fn corrupt_primary_recovers_atomically_without_replacing_valid_backup() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("state.json");
        let backup = path.with_extension("bak");
        std::fs::write(&path, b"{corrupt").expect("corrupt primary");
        std::fs::write(&backup, br#"{"generation":9}"#).expect("backup write");

        let recovered: serde_json::Value = read_json(&path).expect("backup recovery");
        let restored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("restored primary"))
                .expect("valid restored primary");
        assert_eq!(recovered["generation"], 9);
        assert_eq!(restored, recovered);
        assert_eq!(
            std::fs::read_to_string(&backup).expect("backup retained"),
            r#"{"generation":9}"#
        );
    }

    #[test]
    fn missing_primary_and_backup_returns_error() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("state.json");
        let error = read_json::<serde_json::Value>(&path).expect_err("missing state must fail");
        assert!(error.to_string().contains("backup unavailable"));
    }

    #[test]
    fn visible_durable_append_fails_closed_when_confirmation_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("journal.jsonl");
        let _post_append = inject_post_append_failure(path.clone());
        let _confirmation = inject_confirmation_failure(path.clone());

        let error = append_json_line_durable(&path, &serde_json::json!({"generation": 1}))
            .expect_err("durable append must fail closed when confirmation also fails");
        assert!(
            error
                .to_string()
                .contains("durability could not be confirmed"),
            "unexpected error: {error:#}"
        );

        assert_eq!(
            std::fs::read_to_string(&path).expect("visible journal line"),
            "{\"generation\":1}\n"
        );
    }

    #[test]
    fn visible_durable_append_recovers_after_initial_sync_failure_and_reloads() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("journal.jsonl");
        let _post_append = inject_post_append_failure(path.clone());

        append_json_line_durable(&path, &serde_json::json!({"generation": 1}))
            .expect("visible line should be accepted only after confirmation sync succeeds");

        let reloaded = std::fs::read_to_string(&path).expect("journal reload");
        assert_eq!(reloaded, "{\"generation\":1}\n");
    }

    #[test]
    fn non_not_found_primary_read_error_does_not_publish_older_backup() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("state.json");
        std::fs::create_dir(&path).expect("primary directory");
        std::fs::write(path.with_extension("bak"), br#"{"generation":1}"#).expect("older backup");

        let error = read_json::<serde_json::Value>(&path).expect_err("directory is not JSON");

        assert!(error.to_string().contains("Cannot read JSON"));
        assert!(
            path.is_dir(),
            "recovery must not replace a newer unreadable primary"
        );
    }

    #[cfg(unix)]
    #[test]
    fn backup_promotion_failure_aborts_before_primary_publication() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("state.json");
        let backup = path.with_extension("bak");
        write_json(&path, &serde_json::json!({"generation": 1})).expect("first write");
        std::fs::create_dir(&backup).expect("blocking backup directory");

        write_json(&path, &serde_json::json!({"generation": 2}))
            .expect_err("backup failure must abort replacement");

        let primary: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("unchanged primary"))
                .expect("valid primary");
        assert_eq!(primary["generation"], 1);
        assert!(backup.is_dir());
        assert!(
            std::fs::read_dir(temp.path())
                .expect("temp listing")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains("bak.tmp")),
            "failed pre-publication backup should be cleaned"
        );
    }
}
