//! Qwen Code adapter. Qwen Code is a Gemini-CLI fork, so sessions follow the
//! Gemini `Content` shape: `role` ("user" / "model") + `parts[]`
//! (`{text}`, `{text, thought:true}`, `{functionCall}`, `{functionResponse}`).
//! Store: one JSON file per session at
//! `<root>/.qwen/sessions/<project-key>/<id>.json`. Resume: `qwen --resume <id>`.
//!
//! FIDELITY: the project-key hashing scheme and exact envelope are vendor-
//! private; this uses a stable local key and is exercised by synthetic
//! round-trip fixtures. Verify a real resume with `portage inspect`.

use crate::harness::{Adapter, SessionRef, WriteOpts, WriteOutcome};
use crate::ir::{Block, Message, Role, Session};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

use super::proj_key;

pub struct Qwen {
    root: PathBuf,
}

impl Qwen {
    pub fn new_opt(root: Option<PathBuf>) -> Self {
        Qwen { root: root.unwrap_or_else(|| dirs::home_dir().unwrap_or_default()) }
    }
    fn sessions_dir(&self) -> PathBuf {
        self.root.join(".qwen").join("sessions")
    }
}

fn parts_to_blocks(parts: &Value) -> Vec<Block> {
    let arr = match parts.as_array() {
        Some(a) => a,
        None => return vec![],
    };
    let mut blocks = Vec::new();
    for p in arr {
        if let Some(fc) = p.get("functionCall") {
            blocks.push(Block::ToolCall {
                id: fc.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                name: fc.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                input: fc.get("args").cloned().unwrap_or(Value::Null),
            });
        } else if let Some(fr) = p.get("functionResponse") {
            blocks.push(Block::ToolResult {
                call_id: fr.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                output: fr.get("response").cloned().unwrap_or(Value::Null),
                is_error: false,
            });
        } else if let Some(text) = p.get("text").and_then(Value::as_str) {
            if p.get("thought").and_then(Value::as_bool).unwrap_or(false) {
                blocks.push(Block::Reasoning { text: text.to_string() });
            } else {
                blocks.push(Block::Text { text: text.to_string() });
            }
        }
    }
    blocks
}

fn blocks_to_parts(blocks: &[Block]) -> Value {
    let mut parts = Vec::new();
    for b in blocks {
        match b {
            Block::Text { text } => parts.push(json!({ "text": text })),
            Block::Reasoning { text } => parts.push(json!({ "text": text, "thought": true })),
            Block::ToolCall { id, name, input } => {
                parts.push(json!({ "functionCall": { "id": id, "name": name, "args": input } }))
            }
            Block::ToolResult { call_id, output, .. } => parts.push(
                json!({ "functionResponse": { "id": call_id, "name": "", "response": output } }),
            ),
        }
    }
    Value::Array(parts)
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::Assistant => "model",
        Role::System => "system",
        Role::User => "user",
    }
}
fn parse_role(s: &str) -> Role {
    match s {
        "model" => Role::Assistant,
        "system" => Role::System,
        _ => Role::User,
    }
}

impl Adapter for Qwen {
    fn name(&self) -> &'static str { "qwen" }
    fn can_read(&self) -> bool { true }
    fn can_write(&self) -> bool { true }

    fn list(&self) -> Result<Vec<SessionRef>> {
        let base = self.sessions_dir();
        let mut out = Vec::new();
        if !base.exists() {
            return Ok(out);
        }
        for proj in fs::read_dir(&base)? {
            let proj = proj?.path();
            if !proj.is_dir() {
                continue;
            }
            for f in fs::read_dir(&proj)? {
                let p = f?.path();
                if p.extension().and_then(|e| e.to_str()) == Some("json") {
                    out.push(SessionRef {
                        locator: p.to_string_lossy().into_owned(),
                        title: None,
                        cwd: None,
                        modified: None,
                    });
                }
            }
        }
        Ok(out)
    }

    fn read(&self, locator: &str) -> Result<Session> {
        let v: Value = serde_json::from_str(
            &fs::read_to_string(locator).with_context(|| format!("reading qwen session {locator}"))?,
        )?;
        let mut session = Session::new(self.name());
        session.cwd = v.get("cwd").and_then(Value::as_str).map(str::to_string);
        session.created = v.get("created").and_then(Value::as_str).map(str::to_string);
        if let Some(msgs) = v.get("messages").and_then(Value::as_array) {
            for m in msgs {
                let role = parse_role(m.get("role").and_then(Value::as_str).unwrap_or("user"));
                let blocks = parts_to_blocks(m.get("parts").unwrap_or(&Value::Null));
                if !blocks.is_empty() {
                    session.messages.push(Message { role, blocks, timestamp: None });
                }
            }
        }
        Ok(session)
    }

    fn write(&self, session: &Session, opts: &WriteOpts) -> Result<WriteOutcome> {
        let cwd = opts.cwd_override.clone().or_else(|| session.cwd.clone());
        let key = proj_key(cwd.as_deref().unwrap_or("unknown"));
        let dir = self.sessions_dir().join(key);
        fs::create_dir_all(&dir)?;
        let sid = uuid::Uuid::new_v4().to_string();
        let path = dir.join(format!("{sid}.json"));

        let messages: Vec<Value> = session
            .messages
            .iter()
            .map(|m| json!({ "role": role_str(m.role), "parts": blocks_to_parts(&m.blocks) }))
            .collect();
        let doc = json!({ "sessionId": sid, "cwd": cwd, "created": session.created, "messages": messages });
        fs::write(&path, serde_json::to_string_pretty(&doc)?)
            .map_err(|e| anyhow!("writing qwen session: {e}"))?;

        Ok(WriteOutcome {
            artifacts: vec![path],
            target_session_id: sid.clone(),
            resume_hint: format!("qwen --resume {sid}"),
        })
    }
}
