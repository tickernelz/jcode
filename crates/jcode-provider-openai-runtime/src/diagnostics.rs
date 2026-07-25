use super::*;

impl OpenAIProvider {
    fn diagnostic_persistent_ws_summary(&self) -> String {
        match self.persistent_ws.try_lock() {
            Ok(guard) => guard
                .as_ref()
                .map(|state| state.diag_snapshot().log_fields())
                .unwrap_or_else(|| PersistentWsDiagSnapshot::absent().log_fields()),
            Err(_) => "persistent_ws=busy".to_string(),
        }
    }

    pub fn diagnostic_state_summary(&self) -> String {
        let transport_mode = self
            .transport_mode
            .try_read()
            .map(|mode| mode.as_str().to_string())
            .unwrap_or_else(|_| "busy".to_string());
        format!(
            "transport_mode={} {}",
            transport_mode,
            self.diagnostic_persistent_ws_summary()
        )
    }
}
