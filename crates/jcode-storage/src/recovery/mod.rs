use super::*;
pub enum StorageRecoveryEvent<'a> {
    UnreadablePrimary {
        path: &'a Path,
        error: &'a std::io::Error,
    },
    CorruptPrimary {
        path: &'a Path,
        error: &'a serde_json::Error,
    },
    RecoveredFromBackup {
        backup_path: &'a Path,
    },
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    read_json_with_recovery_handler(path, |event| match event {
        StorageRecoveryEvent::UnreadablePrimary { path, error } => {
            eprintln!(
                "Cannot read JSON at {}, trying backup: {}",
                path.display(),
                error
            );
        }
        StorageRecoveryEvent::CorruptPrimary { path, error } => {
            eprintln!(
                "Corrupt JSON at {}, trying backup: {}",
                path.display(),
                error
            );
        }
        StorageRecoveryEvent::RecoveredFromBackup { backup_path } => {
            eprintln!("Recovered from backup: {}", backup_path.display());
        }
    })
}

pub fn read_json_with_recovery_handler<T, F>(path: &Path, mut on_recovery: F) -> Result<T>
where
    T: DeserializeOwned,
    F: FnMut(StorageRecoveryEvent<'_>),
{
    let _path_lock = AtomicPathLock::acquire(path)?;
    let bak_path = path.with_extension("bak");
    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        Err(primary_error) => {
            if primary_error.kind() != std::io::ErrorKind::NotFound {
                return Err(anyhow::anyhow!(
                    "Cannot read JSON at {}: {}",
                    path.display(),
                    primary_error
                ));
            }
            on_recovery(StorageRecoveryEvent::UnreadablePrimary {
                path,
                error: &primary_error,
            });
            let backup_data = std::fs::read_to_string(&bak_path).map_err(|backup_error| {
                anyhow::anyhow!(
                    "Cannot read JSON at {} ({}), backup unavailable at {} ({})",
                    path.display(),
                    primary_error,
                    bak_path.display(),
                    backup_error
                )
            })?;
            let value = serde_json::from_str(&backup_data).map_err(|backup_error| {
                anyhow::anyhow!(
                    "Cannot read JSON at {} ({}), backup corrupt at {} ({})",
                    path.display(),
                    primary_error,
                    bak_path.display(),
                    backup_error
                )
            })?;
            write_bytes_inner_locked(path, backup_data.as_bytes(), true, false, false)?;
            on_recovery(StorageRecoveryEvent::RecoveredFromBackup {
                backup_path: &bak_path,
            });
            return Ok(value);
        }
    };
    match serde_json::from_str(&data) {
        Ok(val) => Ok(val),
        Err(e) => {
            if bak_path.exists() {
                on_recovery(StorageRecoveryEvent::CorruptPrimary { path, error: &e });
                let bak_data = std::fs::read_to_string(&bak_path)?;
                match serde_json::from_str(&bak_data) {
                    Ok(val) => {
                        write_bytes_inner_locked(path, bak_data.as_bytes(), true, false, false)?;
                        on_recovery(StorageRecoveryEvent::RecoveredFromBackup {
                            backup_path: &bak_path,
                        });
                        Ok(val)
                    }
                    Err(bak_err) => Err(anyhow::anyhow!(
                        "Corrupt JSON at {} ({}), backup also corrupt ({})",
                        path.display(),
                        e,
                        bak_err
                    )),
                }
            } else {
                Err(anyhow::anyhow!("Corrupt JSON at {}: {}", path.display(), e))
            }
        }
    }
}

/// Fast append of a single JSON value followed by a newline.
/// Intended for append-only journals where per-write fsync is not required.
///
/// The entire line (value + trailing newline) is serialized into one buffer
/// and appended with a single `write_all`. Streaming the serializer straight
/// into the file issued many small writes, so a concurrent reader (or a
/// process killed mid-append) could observe a torn half-line, and two
/// concurrent appenders could interleave fragments. A single `O_APPEND` write
/// of the complete line keeps each journal line intact.
pub fn append_json_line_fast<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    append_json_line(path, value, false)
}

pub fn append_json_line_durable<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    append_json_line(path, value, true)
}

fn append_json_line<T: Serialize + ?Sized>(path: &Path, value: &T, durable: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let _path_lock = AtomicPathLock::acquire(path)?;
    #[cfg(feature = "test-support")]
    inject_test_write_failure(path)?;
    #[cfg(unix)]
    let existed = path.exists();

    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let original_len = file.metadata()?.len();
    file.write_all(&line)?;
    if durable {
        let durability_result = (|| -> Result<()> {
            #[cfg(any(test, feature = "test-support"))]
            inject_test_post_append_failure(path)?;
            file.sync_all()?;
            #[cfg(unix)]
            if !existed && let Some(parent) = path.parent() {
                std::fs::File::open(parent)?.sync_all()?;
            }
            Ok(())
        })();
        if let Err(first_error) = durability_result {
            drop(file);
            let exact_line_is_visible = std::fs::read(path).is_ok_and(|bytes| {
                let start = original_len as usize;
                bytes.len() == start.saturating_add(line.len())
                    && bytes.get(start..) == Some(line.as_slice())
            });
            if exact_line_is_visible {
                if let Err(confirm_error) = confirm_publication_durable(path) {
                    // Never truncate a complete replayable line while trying to
                    // resolve an ambiguous fsync result. Also never acknowledge
                    // durable success when the caller-visible recovery record
                    // could not be synchronized after the first fsync failure.
                    return Err(anyhow::anyhow!(
                        "Journal append to {} is visible but durability could not be confirmed after initial synchronization failed: {}; confirmation failed: {}",
                        path.display(),
                        first_error,
                        confirm_error
                    ));
                }
                return Ok(());
            }
            return Err(anyhow::anyhow!(
                "Journal append to {} failed before an exact complete line was published: {}",
                path.display(),
                first_error
            ));
        }
    }
    Ok(())
}
