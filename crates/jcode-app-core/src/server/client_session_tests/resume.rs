use super::*;
use crate::transport::WriteHalf;
use anyhow::{Result, anyhow};

struct TestRuntimeGuard {
    _sandbox: tempfile::TempDir,
    prev_runtime: Option<std::ffi::OsString>,
    prev_home: Option<std::ffi::OsString>,
}

impl Drop for TestRuntimeGuard {
    fn drop(&mut self) {
        if let Some(prev_runtime) = self.prev_runtime.take() {
            crate::env::set_var("JCODE_RUNTIME_DIR", prev_runtime);
        } else {
            crate::env::remove_var("JCODE_RUNTIME_DIR");
        }
        if let Some(prev_home) = self.prev_home.take() {
            crate::env::set_var("JCODE_HOME", prev_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }
}

fn setup_runtime_dir() -> Result<TestRuntimeGuard> {
    let sandbox = tempfile::TempDir::new().map_err(|e| anyhow!(e))?;
    let runtime = sandbox.path().join("runtime");
    let home = sandbox.path().join("home");
    std::fs::create_dir_all(&runtime)?;
    std::fs::create_dir_all(&home)?;
    let prev_runtime = std::env::var_os("JCODE_RUNTIME_DIR");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_RUNTIME_DIR", &runtime);
    crate::env::set_var("JCODE_HOME", &home);
    Ok(TestRuntimeGuard {
        _sandbox: sandbox,
        prev_runtime,
        prev_home,
    })
}

fn test_writer() -> Result<(Arc<Mutex<WriteHalf>>, crate::transport::Stream)> {
    let (stream_a, stream_b) = crate::transport::stream_pair().map_err(|e| anyhow!(e))?;
    let (_reader, writer_half) = stream_a.into_split();
    Ok((Arc::new(Mutex::new(writer_half)), stream_b))
}

include!("resume/multiple_live_attach.rs");
include!("resume/busy_existing_attach.rs");
include!("resume/reconnect_takeover_with_history.rs");
include!("resume/attach_without_local_history.rs");
include!("resume/different_client_attach.rs");
include!("resume/live_events_before_history.rs");
include!("resume/same_client_takeover.rs");
