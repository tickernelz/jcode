use super::*;

impl App {
    pub(in super::super) fn provider_session_id_for_next_request(&self) -> Option<String> {
        self.provider_session_id.clone()
    }

    pub(in super::super) fn append_current_turn_system_reminder(
        &self,
        split: &mut crate::prompt::SplitSystemPrompt,
    ) {
        let Some(reminder) = self
            .current_turn_system_reminder
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        else {
            return;
        };

        if !split.dynamic_part.is_empty() {
            split.dynamic_part.push_str("\n\n");
        }
        split.dynamic_part.push_str("# System Reminder\n\n");
        split.dynamic_part.push_str(reminder);
    }
}
