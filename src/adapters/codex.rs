//! Codex CLI adapter. Store: JSONL "rollout" files at
//! `<root>/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`, plus a
//! SQLite index at `<root>/.codex/state_5.sqlite` that the resume picker reads.
//! Resume: `codex resume`.
//!
//! The rollout file is written in full. The SQLite index schema is vendor-
//! private and version-specific, so `register_index` is CONSERVATIVE: it writes
//! only when it recognizes the table, and otherwise skips with a note (the
//! rollout file is still on disk; depending on Codex version `codex resume` may
//! still pick it up by scanning). It never guesses a schema and corrupts the DB.

use crate::harness::{Adapter, SessionRef, WriteOpts, WriteOutcome};
use crate::ir::{Block, Message, Role, Session};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub struct Codex {
    root: PathBuf,
}

impl Codex {
    pub fn new() -> Self {
        Codex { root: dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")) }
    }
    pub fn with_root(root: PathBuf) -> Self {
        Codex { root }
    }
    fn sessions_dir(&self) -> PathBuf {
        self.root.join(".codex").join("sessions")
    }
    fn index_path(&self) -> PathBuf {
        self.root.join(".codex").join("state_5.sqlite")
    }
}

fn args_to_value(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or(Value::String(s.to_string()))
}

impl Adapter for Codex {
    fn name(&self) -> &'static str { "codex" }
    fn can_read(&self) -> bool { true }
    fn can_write(&self) -> bool { true }

    fn list(&self) -> Result<Vec<SessionRef>> {
        let base = self.sessions_dir();
        let mut out = Vec::new();
        if !base.exists() {
            return Ok(out);
        }
        // sessions/YYYY/MM/DD/rollout-*.jsonl
        for a in fs::read_dir(&base)? {
            let y = a?.path();
            if !y.is_dir() { continue; }
            for b in fs::read_dir(&y)? {
                let m = b?.path();
                if !m.is_dir() { continue; }
                for c in fs::read_dir(&m)? {
                    let d = c?.path();
                    if !d.is_dir() { continue; }
                    for f in fs::read_dir(&d)? {
                        let p = f?.path();
                        let is_rollout = p
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
                            .unwrap_or(false);
                        if is_rollout {
                            let (cwd, title) = codex_meta(&p);
                            out.push(SessionRef {
                                locator: p.to_string_lossy().into_owned(),
                                title,
                                cwd,
                                modified: crate::harness::mtime_str(&p),
                            });
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    fn read(&self, locator: &str) -> Result<Session> {
        let text = fs::read_to_string(locator)
            .with_context(|| format!("reading codex rollout {locator}"))?;
        let mut session = Session::new(self.name());
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line)?;
            let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
            let payload = v.get("payload").cloned().unwrap_or(Value::Null);
            match kind {
                "session_meta" => {
                    session.cwd = payload.get("cwd").and_then(Value::as_str).map(str::to_string);
                    session.created =
                        payload.get("timestamp").and_then(Value::as_str).map(str::to_string);
                }
                "response_item" => {
                    if let Some(msg) = response_item_to_message(&payload) {
                        session.messages.push(msg);
                    }
                }
                _ => {}
            }
        }
        Ok(session)
    }

    fn write(&self, session: &Session, _opts: &WriteOpts) -> Result<WriteOutcome> {
        let now = OffsetDateTime::now_utc();
        let dir = self
            .sessions_dir()
            .join(format!("{:04}", now.year()))
            .join(format!("{:02}", now.month() as u8))
            .join(format!("{:02}", now.day()));
        fs::create_dir_all(&dir)?;
        let sid = uuid::Uuid::new_v4().to_string();
        let ts = now.format(&Rfc3339).unwrap_or_default();
        let path = dir.join(format!("rollout-{}-{sid}.jsonl", ts.replace([':', '.'], "-")));

        let mut lines = String::new();
        let meta = json!({
            "type": "session_meta",
            "payload": { "id": sid, "timestamp": ts, "cwd": session.cwd },
        });
        lines.push_str(&serde_json::to_string(&meta)?);
        lines.push('\n');

        for m in &session.messages {
            for item in message_to_response_items(m) {
                lines.push_str(&serde_json::to_string(&json!({
                    "type": "response_item", "payload": item
                }))?);
                lines.push('\n');
            }
        }
        fs::write(&path, lines)?;

        let index_note = register_index(&self.index_path(), &sid, &session.cwd, &ts)?;

        Ok(WriteOutcome {
            artifacts: vec![path],
            target_session_id: sid.clone(),
            resume_hint: format!("codex resume {sid}{}", index_note),
        })
    }
}

/// Text from a Codex content field that may be a string or an array of
/// `{type, text}` items (input_text / output_text).
fn extract_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Cheap metadata for `list`/`find`: cwd from session_meta, title from the
/// first real user prompt (skipping injected `<...>` context), reading only a
/// prefix of the rollout.
fn codex_meta(path: &std::path::Path) -> (Option<String>, Option<String>) {
    let text = match crate::harness::read_prefix(path, 32 * 1024) {
        Some(t) => t,
        None => return (None, None),
    };
    let mut cwd = None;
    let mut title = None;
    for line in text.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let payload = v.get("payload");
        if cwd.is_none() {
            cwd = payload
                .and_then(|p| p.get("cwd"))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if title.is_none() {
            if let Some(p) = payload {
                if p.get("type").and_then(Value::as_str) == Some("message")
                    && p.get("role").and_then(Value::as_str) == Some("user")
                {
                    let t = extract_text(p.get("content").unwrap_or(&Value::Null));
                    let t = t.trim();
                    if !t.is_empty() && !t.starts_with('<') {
                        title = Some(t.chars().take(70).collect::<String>());
                    }
                }
            }
        }
        if cwd.is_some() && title.is_some() {
            break;
        }
    }
    (cwd, title)
}

fn response_item_to_message(p: &Value) -> Option<Message> {
    match p.get("type").and_then(Value::as_str) {
        // Real Codex rollouts use `message` (with roles user/assistant/system/
        // developer) and, in some versions, `agent_message` for model output.
        Some("message") => {
            let role = match p.get("role").and_then(Value::as_str) {
                Some("assistant") => Role::Assistant,
                Some("system") | Some("developer") => Role::System,
                _ => Role::User,
            };
            let text = extract_text(p.get("content").unwrap_or(&Value::Null));
            Some(Message { role, blocks: vec![Block::Text { text }], timestamp: None })
        }
        Some("agent_message") => Some(Message {
            role: Role::Assistant,
            blocks: vec![Block::Text { text: extract_text(p.get("content").unwrap_or(&Value::Null)) }],
            timestamp: None,
        }),
        // `function_call` (arguments: json string) and `custom_tool_call`
        // (input: value) are both tool invocations.
        Some("function_call") | Some("custom_tool_call") => Some(Message {
            role: Role::Assistant,
            blocks: vec![Block::ToolCall {
                id: p.get("call_id").and_then(Value::as_str).unwrap_or("").to_string(),
                name: p.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                input: p
                    .get("arguments")
                    .and_then(Value::as_str)
                    .map(args_to_value)
                    .or_else(|| p.get("input").cloned())
                    .unwrap_or(Value::Null),
            }],
            timestamp: None,
        }),
        Some("function_call_output") | Some("custom_tool_call_output") => Some(Message {
            role: Role::User,
            blocks: vec![Block::ToolResult {
                call_id: p.get("call_id").and_then(Value::as_str).unwrap_or("").to_string(),
                output: p.get("output").cloned().unwrap_or(Value::Null),
                is_error: false,
            }],
            timestamp: None,
        }),
        Some("reasoning") => {
            let text = p
                .get("summary")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| c.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            Some(Message { role: Role::Assistant, blocks: vec![Block::Reasoning { text }], timestamp: None })
        }
        _ => None,
    }
}

fn message_to_response_items(m: &Message) -> Vec<Value> {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    };
    let text_type = if m.role == Role::Assistant { "output_text" } else { "input_text" };
    let mut items = Vec::new();
    for b in &m.blocks {
        match b {
            Block::Text { text } => items.push(json!({
                "type": "message", "role": role,
                "content": [{ "type": text_type, "text": text }],
            })),
            Block::Reasoning { text } => items.push(json!({
                "type": "reasoning", "summary": [{ "type": "summary_text", "text": text }],
            })),
            Block::ToolCall { id, name, input } => items.push(json!({
                "type": "function_call", "call_id": id, "name": name,
                "arguments": serde_json::to_string(input).unwrap_or_default(),
            })),
            Block::ToolResult { call_id, output, .. } => items.push(json!({
                "type": "function_call_output", "call_id": call_id,
                "output": output,
            })),
        }
    }
    items
}

/// Best-effort index registration. Returns a human note appended to the resume
/// hint. Never corrupts an unrecognized schema.
fn register_index(
    index_path: &std::path::Path,
    session_id: &str,
    cwd: &Option<String>,
    created: &str,
) -> Result<String> {
    if !index_path.exists() {
        return Ok(String::from(
            "  (note: no state_5.sqlite index found; resume relies on file scan)",
        ));
    }
    let conn = rusqlite::Connection::open(index_path)?;
    // Recognize the table before touching it.
    let has_sessions: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='sessions'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !has_sessions {
        return Ok(String::from(
            "  (note: state_5.sqlite schema not recognized; left untouched — run `portage inspect` to verify)",
        ));
    }
    // We recognize a `sessions(id, cwd, created)` shape. If the real columns
    // differ this insert fails and we report rather than guess.
    match conn.execute(
        "INSERT INTO sessions (id, cwd, created) VALUES (?1, ?2, ?3)",
        rusqlite::params![session_id, cwd.clone().unwrap_or_default(), created],
    ) {
        Ok(_) => Ok(String::new()),
        Err(e) => Ok(format!(
            "  (note: index insert skipped: {e}; rollout file written, verify schema)"
        )),
    }
}
