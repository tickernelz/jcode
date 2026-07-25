#[test]
fn async_exclusive_account_lock_does_not_block_single_worker_runtime() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    let previous_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp_home.path());

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("single-worker runtime")
        .block_on(async {
            let shared = AccountReconciliationFileLock::acquire_shared()
                .expect("hold active turn admission");
            let mut exclusive = tokio::spawn(AccountReconciliationFileLock::acquire_async());

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert!(
                !exclusive.is_finished(),
                "exclusive switch should still be waiting for the active turn"
            );
            drop(shared);

            tokio::time::timeout(std::time::Duration::from_secs(1), &mut exclusive)
                .await
                .expect("single-worker runtime remained responsive")
                .expect("exclusive acquisition task")
                .expect("exclusive transition lock");
        });

    if let Some(previous_home) = previous_home {
        crate::env::set_var("JCODE_HOME", previous_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn aborted_async_exclusive_wait_releases_writer_turnstile() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    let previous_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp_home.path());

    tokio::runtime::Runtime::new()
        .expect("test runtime")
        .block_on(async {
            let shared = AccountReconciliationFileLock::acquire_shared()
                .expect("hold active turn admission");
            let writer = tokio::spawn(AccountReconciliationFileLock::acquire_async());
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert!(!writer.is_finished(), "writer must wait for active turn");

            let mut late_reader = tokio::task::spawn_blocking(
                AccountReconciliationFileLock::acquire_shared,
            );
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(75),
                    &mut late_reader
                )
                .await
                .is_err(),
                "queued writer must close the turnstile to later readers"
            );

            writer.abort();
            assert!(matches!(writer.await, Err(error) if error.is_cancelled()));
            tokio::time::timeout(std::time::Duration::from_secs(1), &mut late_reader)
                .await
                .expect("aborted writer must release its turnstile")
                .expect("late reader blocking task")
                .expect("late shared admission");
            drop(shared);
        });

    if let Some(previous_home) = previous_home {
        crate::env::set_var("JCODE_HOME", previous_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}
