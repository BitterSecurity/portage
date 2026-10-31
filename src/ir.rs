//! Intermediate representation (IR): the hub every harness reads into and
//! writes out of. It is the *union* of what supported harnesses can express,
//! so a conversion is `reader -> IR -> writer`. Anything a harness cannot
//! express on write is reported (see `convert::ConversionReport`), never
//! silently dropped.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Portage's own stable id for this session (not a harness id).
    pub id: String,
    /// Which harness this IR was read from.
    pub source_harness: String,
    /// Working directory the session belonged to, if known. Resume on several
    /// harnesses (notably Claude Code) depends on this matching the launch dir.
    pub cwd: Option<String>,
    pub title: Option<String>,
    /// RFC3339 timestamp of session creation, if known.
    pub created: Option<String>,
    pub messages: Vec<Message>,
    /// Open tool calls / queued input carried forward into a resume preamble.
    #[serde(default)]
    pub pending: Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<Block>,
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

/// Typed content block. This set is the lowest common denominator that still
/// preserves the conversation core and tool call<->result linkage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    /// Reasoning survives only as summary text across harnesses (signatures,
    /// opaque payloads are lost).
    Reasoning {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        /// Links back to `ToolCall.id`.
        call_id: String,
        output: serde_json::Value,
        #[serde(default)]
        is_error: bool,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pending {
    pub open_tool_calls: Vec<String>,
    pub queued_input: Vec<String>,
}

impl Session {
    pub fn new(source_harness: impl Into<String>) -> Self {
        Session {
            id: uuid::Uuid::new_v4().to_string(),
            source_harness: source_harness.into(),
            cwd: None,
            title: None,
            created: None,
            messages: Vec::new(),
            pending: Pending::default(),
        }
    }

    /// Convenience: plain-text content of a message joined with newlines.
    pub fn text_of(msg: &Message) -> String {
        msg.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
