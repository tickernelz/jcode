#[test]
fn pending_account_transition_writer_blocks_later_shared_admissions() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("jcode-account-transition-turnstile-")
        .tempdir()
        .map_err(|error| anyhow!(error))?;
    let _home = EnvVarGuard::set("JCODE_HOME", temp_home.path().as_os_str());

    let initial_reader = AccountTransitionFileLock::acquire_shared()?;
    let (pending_tx, pending_rx) = std::sync::mpsc::channel();
    let (writer_acquired_tx, writer_acquired_rx) = std::sync::mpsc::channel();
    let (release_writer_tx, release_writer_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let lock = AccountTransitionFileLock::acquire_exclusive_notifying(pending_tx)
            .expect("acquire exclusive transition lock");
        writer_acquired_tx.send(()).unwrap();
        release_writer_rx.recv().unwrap();
        drop(lock);
    });
    pending_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("writer must own turnstile before waiting for readers");

    let mut admitted_session = Session::create(None, Some("admitted save".to_string()));
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let expired_admission = runtime.block_on(with_account_transition_admission(async {
            admitted_session
                .save()
                .expect("save must reuse the task-owned shared admission");

            let inherited = capture_account_transition_admission();
            let child = tokio::spawn(with_inherited_account_transition_admission(
                inherited,
                async move {
                    let mut child_session =
                        Session::create(None, Some("spawned admitted save".to_string()));
                    child_session
                        .save()
                        .expect("spawned save must inherit the live parent admission");
                    account_transition_admission_held()
                },
            ));
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(250), child)
                    .await
                    .expect("spawned admitted save must not wait behind the queued writer")
                    .expect("spawned admitted save task must complete")
            );
            capture_account_transition_admission().expect("capture live parent admission")
        }));
    runtime.block_on(with_inherited_account_transition_admission(
        Some(expired_admission),
        async {
            assert!(
                !account_transition_admission_held(),
                "a detached child must not retain admission after the owning turn ends"
            );
        },
    ));

    let (late_reader_tx, late_reader_rx) = std::sync::mpsc::channel();
    let late_reader = std::thread::spawn(move || {
        let lock = AccountTransitionFileLock::acquire_shared()
            .expect("acquire late shared transition lock");
        late_reader_tx.send(()).unwrap();
        drop(lock);
    });
    assert!(
        matches!(
            late_reader_rx.recv_timeout(std::time::Duration::from_millis(75)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "a reader arriving after a pending writer must wait at the turnstile"
    );

    drop(initial_reader);
    writer_acquired_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("writer must acquire after initial reader drains");
    assert!(
        matches!(
            late_reader_rx.recv_timeout(std::time::Duration::from_millis(75)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "late reader must remain blocked while the writer owns the main lock"
    );

    release_writer_tx.send(()).unwrap();
    late_reader_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("late reader must proceed after writer release");
    writer.join().unwrap();
    late_reader.join().unwrap();
    Ok(())
}
