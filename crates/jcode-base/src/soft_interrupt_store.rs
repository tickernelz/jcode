use anyhow::Result;
use jcode_agent_runtime::{SoftInterruptMessage, SoftInterruptSource};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static TEST_DIR_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
pub struct TestDirGuard(Option<PathBuf>);

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestDirGuard {
    fn drop(&mut self) {
        TEST_DIR_PATH.with(|path| {
            path.replace(self.0.take());
        });
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn use_test_dir(path: PathBuf) -> TestDirGuard {
    TestDirGuard(TEST_DIR_PATH.with(|current| current.replace(Some(path))))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSoftInterrupt {
    content: String,
    urgent: bool,
    source: PersistedSoftInterruptSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PersistedSoftInterruptSource {
    User,
    System,
    BackgroundTask,
}

impl From<SoftInterruptSource> for PersistedSoftInterruptSource {
    fn from(value: SoftInterruptSource) -> Self {
        match value {
            SoftInterruptSource::User => Self::User,
            SoftInterruptSource::System => Self::System,
            SoftInterruptSource::BackgroundTask => Self::BackgroundTask,
        }
    }
}

impl From<PersistedSoftInterruptSource> for SoftInterruptSource {
    fn from(value: PersistedSoftInterruptSource) -> Self {
        match value {
            PersistedSoftInterruptSource::User => Self::User,
            PersistedSoftInterruptSource::System => Self::System,
            PersistedSoftInterruptSource::BackgroundTask => Self::BackgroundTask,
        }
    }
}

impl From<SoftInterruptMessage> for PersistedSoftInterrupt {
    fn from(value: SoftInterruptMessage) -> Self {
        Self {
            content: value.content,
            urgent: value.urgent,
            source: value.source.into(),
        }
    }
}

impl From<PersistedSoftInterrupt> for SoftInterruptMessage {
    fn from(value: PersistedSoftInterrupt) -> Self {
        Self {
            content: value.content,
            urgent: value.urgent,
            source: value.source.into(),
        }
    }
}

fn dir_path() -> Result<PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(path) = TEST_DIR_PATH.with(|path| path.borrow().clone()) {
        return Ok(path);
    }
    Ok(crate::storage::jcode_dir()?.join("pending-soft-interrupts"))
}

fn path_for_session(session_id: &str) -> Result<PathBuf> {
    Ok(dir_path()?.join(format!("{}.json", session_id)))
}

pub fn load(session_id: &str) -> Result<Vec<SoftInterruptMessage>> {
    let path = path_for_session(session_id)?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    let persisted: Vec<PersistedSoftInterrupt> = crate::storage::read_json(&path)?;
    Ok(persisted
        .into_iter()
        .map(SoftInterruptMessage::from)
        .collect())
}

pub fn take(session_id: &str) -> Result<Vec<SoftInterruptMessage>> {
    let path = path_for_session(session_id)?;
    let loaded = load(session_id)?;
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    Ok(loaded)
}

pub fn overwrite(session_id: &str, interrupts: &[SoftInterruptMessage]) -> Result<()> {
    let path = path_for_session(session_id)?;
    if interrupts.is_empty() {
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let persisted: Vec<PersistedSoftInterrupt> =
        interrupts.iter().cloned().map(Into::into).collect();
    crate::storage::write_json_fast(&path, &persisted)
}

pub fn append(session_id: &str, interrupt: SoftInterruptMessage) -> Result<()> {
    let mut current = load(session_id)?;
    current.push(interrupt);
    overwrite(session_id, &current)
}

pub fn clear(session_id: &str) -> Result<()> {
    overwrite(session_id, &[])
}

#[cfg(test)]
#[path = "soft_interrupt_store_tests.rs"]
mod soft_interrupt_store_tests;
