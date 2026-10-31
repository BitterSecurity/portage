//! Claude Code adapter. Store: JSONL, one event per line, at
//! `<root>/.claude/projects/<encoded-cwd>/<uuid>.jsonl`.
//! Resume: `claude --resume` — which only finds the session when the
//! encoded-cwd directory matches the dir `claude` is launched from.
//!
//! NOTE ON FIDELITY: the field mapping below follows the public shape of
//! Claude Code transcripts (Anthropic message content blocks). It is exercised
//! by synthetic round-trip fixtures. Before trusting a *real* resume, verify
//! against your installed Claude Code version with `portage inspect`.

use crate::harness::{Adapter, SessionRef, WriteOpts, WriteOutcome};
use crate::ir::{Block, Message, Role, Session};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

pub struct ClaudeCode {
    root: PathBuf,
}

impl ClaudeCode {
    pub fn new() -> Self {
        ClaudeCode { root: dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")) }
    }
    pub fn with_root(root: PathBuf) -> Self {
        ClaudeCode { root }
    }
    fn projects_dir(&self) -> PathBuf {
        self.root.join(".claude").join("projects")
    }
}

/// `/home/catow/GIT/portage` -> `-home-catow-GIT-portage`
pub fn encode_cwd(cwd: &str) -> String {
    cwd.replace(['/', '\\'], "-")
}

fn content_to_blocks(content: &Value) -> Vec<Block> {
    let items = match content {
        Value::Array(a) => a.clone(),
        // A bare string is plain text.
        Value::String(s) => return vec![Block::Text { text: s.clone() }],
        _ => return vec![],
    };
    let mut blocks = Vec::new();
    for it in items {
        match it.get("type").and_then(Value::as_str) {
            Some("text") => blocks.push(Block::Text {
                text: it.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
            }),
            Some("thinking") => blocks.push(Block::Reasoning {
                text: it.get("thinking").and_then(Value::as_str).unwrap_or("").to_string(),
            }),
            Some("tool_use") => blocks.push(Block::ToolCall {
                id: it.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                name: it.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                input: it.get("input").cloned().unwrap_or(Value::Null),
            }),
            Some("tool_result") => blocks.push(Block::ToolResult {
                call_id: it.get("tool_use_id").and_then(Value::as_str).unwrap_or("").to_string(),
                output: it.get("content").cloned().unwrap_or(Value::Null),
                is_error: it.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            }),
            _ => {}
        }
    }
    blocks
}

fn blocks_to_content(blocks: &[Block]) -> Value {
    let mut items = Vec::new();
    for b in blocks {
        match b {
            Block::Text { text } => items.push(json!({"type": "text", "text": text})),
            Block::Reasoning { text } => items.push(json!({"type": "thinking", "thinking": text})),
            Block::ToolCall { id, name, input } => {
                items.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}))
            }
            Block::ToolResult { call_id, output, is_error } => items.push(
                json!({"type": "tool_result", "tool_use_id": call_id, "content": output, "is_error": is_error}),
            ),
        }
    }
    Value::Array(items)
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    }
}
fn parse_role(s: &str) -> Role {
    match s {
        "assistant" => Role::Assistant,
        "system" => Role::System,
        _ => Role::User,
    }
}

impl Adapter for ClaudeCode {
    fn name(&self) -> &'static str { "claude-code" }
    fn can_read(&self) -> bool { true }
    fn can_write(&self) -> bool { true }

    fn list(&self) -> Result<Vec<SessionRef>> {
        let dir = self.projects_dir();
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        for proj in fs::read_dir(&dir)? {
            let proj = proj?.path();
            if !proj.is_dir() {
                continue;
            }
            for f in fs::read_dir(&proj)? {
                let p = f?.path();
                if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    out.push(SessionRef {
                        locator: p.to_string_lossy().into_owned(),
                        title: None,
                        cwd: Some(proj.file_name().unwrap().to_string_lossy().into_owned()),
                        modified: None,
                    });
                }
            }
        }
        Ok(out)
    }

    fn read(&self, locator: &str) -> Result<Session> {
        let text = fs::read_to_string(locator)
            .with_context(|| format!("reading claude-code session {locator}"))?;
        let mut session = Session::new(self.name());
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line)
                .with_context(|| format!("parsing jsonl line in {locator}"))?;
            if session.cwd.is_none() {
                if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                    session.cwd = Some(c.to_string());
                }
            }
            if session.created.is_none() {
                if let Some(t) = v.get("timestamp").and_then(Value::as_str) {
                    session.created = Some(t.to_string());
                }
            }
            let msg = match v.get("message") {
                Some(m) => m,
                None => continue, // skip non-message events (summaries etc.)
            };
            let role = parse_role(msg.get("role").and_then(Value::as_str).unwrap_or("user"));
            let blocks = content_to_blocks(msg.get("content").unwrap_or(&Value::Null));
            if blocks.is_empty() {
                continue;
            }
            session.messages.push(Message {
                role,
                blocks,
                timestamp: v.get("timestamp").and_then(Value::as_str).map(str::to_string),
            });
        }
        Ok(session)
    }

    fn write(&self, session: &Session, opts: &WriteOpts) -> Result<WriteOutcome> {
        let cwd = opts
            .cwd_override
            .clone()
            .or_else(|| session.cwd.clone())
            .ok_or_else(|| anyhow!("claude-code resume needs a cwd; pass --cwd"))?;
        let dir = self.projects_dir().join(encode_cwd(&cwd));
        fs::create_dir_all(&dir)?;
        let sid = uuid::Uuid::new_v4().to_string();
        let path = dir.join(format!("{sid}.jsonl"));

        let mut lines = String::new();
        for m in &session.messages {
            let line = json!({
                "type": role_str(m.role),
                "sessionId": sid,
                "cwd": cwd,
                "timestamp": m.timestamp,
                "message": { "role": role_str(m.role), "content": blocks_to_content(&m.blocks) },
            });
            lines.push_str(&serde_json::to_string(&line)?);
            lines.push('\n');
        }
        fs::write(&path, lines)?;

        Ok(WriteOutcome {
            artifacts: vec![path],
            target_session_id: sid.clone(),
            resume_hint: format!("cd {cwd} && claude --resume {sid}"),
        })
    }
}
