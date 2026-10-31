//! Kimi Code adapter. Store: `<root>/.kimi-code/sessions/<project-key>/<id>/`
//! holding `context.jsonl` (one message per line) + `state.json`. Resume:
//! `kimi --session <id>`.
//!
//! context.jsonl lines use an OpenAI-chat-ish shape (role + content, with
//! `tool_calls`, `reasoning_content`, and `role:"tool"` results) — Moonshot's
//! wire style. Exact fields are vendor-private; exercised by synthetic
//! round-trip fixtures. Verify a real resume with `portage inspect`.

use crate::harness::{Adapter, SessionRef, WriteOpts, WriteOutcome};
use crate::ir::{Block, Message, Role, Session};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

use super::proj_key;

pub struct Kimi {
    root: PathBuf,
}

impl Kimi {
    pub fn new_opt(root: Option<PathBuf>) -> Self {
        Kimi { root: root.unwrap_or_else(|| dirs::home_dir().unwrap_or_default()) }
    }
    fn sessions_dir(&self) -> PathBuf {
        self.root.join(".kimi-code").join("sessions")
    }
}

fn val_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn line_to_message(v: &Value) -> Option<Message> {
    let role_s = v.get("role").and_then(Value::as_str)?;
    // Tool result line.
    if role_s == "tool" {
        return Some(Message {
            role: Role::User,
            blocks: vec![Block::ToolResult {
                call_id: v.get("tool_call_id").and_then(Value::as_str).unwrap_or("").to_string(),
                output: v.get("content").cloned().unwrap_or(Value::Null),
                is_error: false,
            }],
            timestamp: None,
        });
    }
    let role = match role_s {
        "assistant" => Role::Assistant,
        "system" => Role::System,
        _ => Role::User,
    };
    let mut blocks = Vec::new();
    if let Some(r) = v.get("reasoning_content").and_then(Value::as_str) {
        if !r.is_empty() {
            blocks.push(Block::Reasoning { text: r.to_string() });
        }
    }
    if let Some(c) = v.get("content").and_then(Value::as_str) {
        if !c.is_empty() {
            blocks.push(Block::Text { text: c.to_string() });
        }
    }
    if let Some(calls) = v.get("tool_calls").and_then(Value::as_array) {
        for c in calls {
            let f = c.get("function").cloned().unwrap_or(Value::Null);
            blocks.push(Block::ToolCall {
                id: c.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                name: f.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                input: f
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or(Value::Null),
            });
        }
    }
    if blocks.is_empty() {
        return None;
    }
    Some(Message { role, blocks, timestamp: None })
}

/// One IR message -> one or more context.jsonl lines. Tool results become their
/// own `role:"tool"` lines; everything else is one combined line.
fn message_to_lines(m: &Message) -> Vec<Value> {
    let role = match m.role {
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::User => "user",
    };
    let mut lines = Vec::new();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    for b in &m.blocks {
        match b {
            Block::Text { text } => content.push_str(text),
            Block::Reasoning { text } => reasoning.push_str(text),
            Block::ToolCall { id, name, input } => tool_calls.push(json!({
                "id": id, "type": "function",
                "function": { "name": name, "arguments": serde_json::to_string(input).unwrap_or_default() }
            })),
            Block::ToolResult { call_id, output, .. } => lines.push(json!({
                "role": "tool", "tool_call_id": call_id, "content": val_to_string(output)
            })),
        }
    }
    let has_main = !content.is_empty() || !reasoning.is_empty() || !tool_calls.is_empty();
    if has_main {
        let mut obj = json!({ "role": role, "content": content });
        if !reasoning.is_empty() {
            obj["reasoning_content"] = json!(reasoning);
        }
        if !tool_calls.is_empty() {
            obj["tool_calls"] = Value::Array(tool_calls);
        }
        // Keep the main line before any tool-result lines for natural ordering.
        lines.insert(0, obj);
    }
    lines
}

impl Adapter for Kimi {
    fn name(&self) -> &'static str { "kimi" }
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
            for sess in fs::read_dir(&proj)? {
                let sdir = sess?.path();
                let ctx = sdir.join("context.jsonl");
                if ctx.exists() {
                    out.push(SessionRef {
                        locator: ctx.to_string_lossy().into_owned(),
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
        let text = fs::read_to_string(locator)
            .with_context(|| format!("reading kimi context {locator}"))?;
        let mut session = Session::new(self.name());
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line)?;
            if let Some(m) = line_to_message(&v) {
                session.messages.push(m);
            }
        }
        Ok(session)
    }

    fn write(&self, session: &Session, opts: &WriteOpts) -> Result<WriteOutcome> {
        let cwd = opts.cwd_override.clone().or_else(|| session.cwd.clone());
        let key = proj_key(cwd.as_deref().unwrap_or("unknown"));
        let sid = uuid::Uuid::new_v4().to_string();
        let dir = self.sessions_dir().join(key).join(&sid);
        fs::create_dir_all(&dir)?;

        let mut ctx = String::new();
        for m in &session.messages {
            for line in message_to_lines(m) {
                ctx.push_str(&serde_json::to_string(&line)?);
                ctx.push('\n');
            }
        }
        let context_path = dir.join("context.jsonl");
        fs::write(&context_path, ctx)?;
        // Minimal state.json so the session is well-formed; real resume may need
        // more fields (verify against installed Kimi version).
        let state_path = dir.join("state.json");
        fs::write(&state_path, serde_json::to_string(&json!({ "id": sid, "cwd": cwd }))?)?;

        Ok(WriteOutcome {
            artifacts: vec![context_path, state_path],
            target_session_id: sid.clone(),
            resume_hint: format!("kimi --session {sid}"),
        })
    }
}
