use super::*;

/// Serde default for boolean fields that should default to `true` when absent,
/// so older clients that omit the field keep their previous (unconditional)
/// behavior.
fn default_true() -> bool {
    true
}

/// Wire spec for a task-DAG node submitted by an agent (seed/expand/inject).
/// Mirrors `jcode_plan::dag::NodeSpec` but kept as an explicit wire type so the
/// protocol stays self-describing and serde-stable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskGraphNodeSpec {
    pub id: String,
    pub content: String,
    /// "explore" | "implement" | "verify" | "fix" | "synthesize". Defaults to
    /// "explore" when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub priority: u8,
}

fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

mod request;
mod server_event;

pub use request::Request;
pub use server_event::ServerEvent;
