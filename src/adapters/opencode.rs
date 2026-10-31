//! opencode adapter — READ-ONLY source.
//!
//! Store (verified against a real install): SQLite at
//! `<root>/.local/share/opencode/opencode.db` with tables `session`,
//! `message` (data JSON holds `role`), and `part` (data JSON, `type` ∈
//! text / reasoning / tool / step-start / step-finish / patch / compaction).
//! A `tool` part carries both the call and its result in `state`
//! (`state.input`, `state.output`). Resume: `opencode --session <id>`.
//!
//! Writing is intentionally NOT implemented: opencode's store is a heavily
//! event-sourced multi-table DB (session/message/part + project linkage); the
//! responsible path is to read opencode as a source and resume elsewhere, not
//! to synthesize rows into a live opencode.db. See design §4.

use crate::harness::{Adapter, SessionRef, WriteOpts, WriteOutcome};
use crate::ir::{Block, Message, Role, Session};
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::Value;
use std::path::PathBuf;

pub struct OpenCode {
    root: PathBuf,
}

impl OpenCode {
    pub fn new_opt(root: Option<PathBuf>) -> Self {
        OpenCode { root: root.unwrap_or_else(|| dirs::home_dir().unwrap_or_default()) }
    }
    fn db_path(&self) -> PathBuf {
        self.root.join(".local").join("share").join("opencode").join("opencode.db")
    }
    fn open(&self) -> Result<Connection> {
        let p = self.db_path();
        Connection::open(&p).with_context(|| format!("opening opencode db at {}", p.display()))
    }
}

fn part_blocks(data: &Value) -> Vec<Block> {
    match data.get("type").and_then(Value::as_str) {
        Some("text") => data
            .get("text")
            .and_then(Value::as_str)
            .map(|t| vec![Block::Text { text: t.to_string() }])
            .unwrap_or_default(),
        Some("reasoning") => {
            let t = data
                .get("text")
                .or_else(|| data.get("reasoning"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if t.is_empty() { vec![] } else { vec![Block::Reasoning { text: t.to_string() }] }
        }
        Some("tool") => {
            let call_id = data.get("callID").and_then(Value::as_str).unwrap_or("").to_string();
            let name = data.get("tool").and_then(Value::as_str).unwrap_or("").to_string();
            let state = data.get("state").cloned().unwrap_or(Value::Null);
            let mut blocks = vec![Block::ToolCall {
                id: call_id.clone(),
                name,
                input: state.get("input").cloned().unwrap_or(Value::Null),
            }];
            // Only emit a result once the tool finished.
            if let Some(output) = state.get("output") {
                let is_error = state.get("status").and_then(Value::as_str) == Some("error");
                blocks.push(Block::ToolResult { call_id, output: output.clone(), is_error });
            }
            blocks
        }
        // step-start / step-finish / patch / compaction: control parts, skipped.
        _ => vec![],
    }
}

impl Adapter for OpenCode {
    fn name(&self) -> &'static str { "opencode" }
    fn can_read(&self) -> bool { true }
    fn can_write(&self) -> bool { false }

    fn list(&self) -> Result<Vec<SessionRef>> {
        if !self.db_path().exists() {
            return Ok(vec![]);
        }
        let conn = self.open()?;
        let mut stmt =
            conn.prepare("SELECT id, directory, title FROM session ORDER BY time_updated DESC")?;
        let rows = stmt.query_map([], |r| {
            Ok(SessionRef {
                locator: r.get::<_, String>(0)?,
                title: r.get::<_, Option<String>>(2)?,
                cwd: r.get::<_, Option<String>>(1)?,
                modified: None,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn read(&self, locator: &str) -> Result<Session> {
        let conn = self.open()?;
        let mut session = Session::new(self.name());
        session.cwd = conn
            .query_row("SELECT directory FROM session WHERE id = ?1", [locator], |r| {
                r.get::<_, Option<String>>(0)
            })
            .ok()
            .flatten();

        // Messages in order, each with its role and ordered parts.
        let mut mstmt = conn
            .prepare("SELECT id, data FROM message WHERE session_id = ?1 ORDER BY time_created, id")?;
        let msgs: Vec<(String, String)> = mstmt
            .query_map([locator], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;

        let mut pstmt =
            conn.prepare("SELECT data FROM part WHERE message_id = ?1 ORDER BY id")?;
        for (mid, mdata) in msgs {
            let role = match serde_json::from_str::<Value>(&mdata)
                .ok()
                .and_then(|v| v.get("role").and_then(Value::as_str).map(str::to_string))
                .as_deref()
            {
                Some("assistant") => Role::Assistant,
                Some("system") => Role::System,
                _ => Role::User,
            };
            let mut blocks = Vec::new();
            let parts: Vec<String> =
                pstmt.query_map([&mid], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()?;
            for pdata in parts {
                if let Ok(v) = serde_json::from_str::<Value>(&pdata) {
                    blocks.extend(part_blocks(&v));
                }
            }
            if !blocks.is_empty() {
                session.messages.push(Message { role, blocks, timestamp: None });
            }
        }
        Ok(session)
    }

    fn write(&self, _session: &Session, _opts: &WriteOpts) -> Result<WriteOutcome> {
        bail!("opencode write is not implemented: its event-sourced multi-table store is read as a source only (use opencode with --from)")
    }
}
