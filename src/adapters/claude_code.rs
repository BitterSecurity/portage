//! Claude Code adapter. Store: JSONL, one event per line, at
//! `<root>/.claude/projects/<encoded-cwd>/<uuid>.jsonl`.
//! Resume: `claude --resume` — which only finds the session when the
//! encoded-cwd directory matches the dir `claude` is launched from.
//!
//! The writer emits the real resume-relevant schema verified against live
//! Claude Code stores: a `uuid`/`parentUuid` linked chain plus `sessionId`,
//! `cwd`, `version`, `gitBranch`, `userType`, `isSidechain`, `entrypoint` on
//! every user/assistant line. Without the uuid chain the resume picker will not
//! list the session.

use crate::harness::{mtime_str, Adapter, SessionRef, WriteOpts, WriteOutcome};
use crate::ir::{Block, Message, Role, Session};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

/// Claude Code version stamped on written lines (resume is tolerant of this).
const CC_VERSION: &str = "2.1.316";

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

/// Encode a cwd into Claude Code's project directory name.
/// Unix: `/home/catow/GIT/portage` -> `-home-catow-GIT-portage`.
/// Windows: `C:\Users\x` -> `C--Users-x` (drive colon also becomes `-`).
/// The Unix/WSL form is verified against real stores; the native-Windows form
/// is a best guess — confirm with `portage inspect` on Windows.
pub fn encode_cwd(cwd: &str) -> String {
    cwd.replace(['/', '\\', ':'], "-")
}

fn content_to_blocks(content: &Value) -> Vec<Block> {
    let items = match content {
        Value::Array(a) => a.clone(),
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

/// Line `type`. System is folded into a user line (rare; handoff never emits it).
fn line_type(r: Role) -> &'static str {
    match r {
        Role::Assistant => "assistant",
        _ => "user",
    }
}
fn parse_role(s: &str) -> Role {
    match s {
        "assistant" => Role::Assistant,
        "system" => Role::System,
        _ => Role::User,
    }
}

fn git_branch(cwd: &str) -> String {
    Command::new("git")
        .args(["-C", cwd, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
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
                if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let (cwd, title) = cc_meta(&p);
                out.push(SessionRef {
                    locator: p.to_string_lossy().into_owned(),
                    title,
                    cwd,
                    modified: mtime_str(&p),
                });
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
                None => continue,
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
        let branch = git_branch(&cwd);
        let now = OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_default();

        let mut lines = String::new();
        let mut parent: Option<String> = None;
        for m in &session.messages {
            let uuid = uuid::Uuid::new_v4().to_string();
            let ts = m.timestamp.clone().unwrap_or_else(|| now.clone());
            let message = if m.role == Role::Assistant {
                json!({
                    "role": "assistant", "type": "message", "id": format!("msg_{uuid}"),
                    "model": "claude-sonnet-4-5", "content": blocks_to_content(&m.blocks),
                    "stop_reason": "end_turn", "usage": {"input_tokens": 0, "output_tokens": 0},
                })
            } else {
                json!({ "role": "user", "content": blocks_to_content(&m.blocks) })
            };
            let line = json!({
                "type": line_type(m.role),
                "uuid": uuid,
                "parentUuid": parent,
                "sessionId": sid,
                "timestamp": ts,
                "cwd": cwd,
                "version": CC_VERSION,
                "gitBranch": branch,
                "userType": "external",
                "isSidechain": false,
                "entrypoint": "sdk-cli",
                "message": message,
            });
            lines.push_str(&serde_json::to_string(&line)?);
            lines.push('\n');
            parent = Some(uuid);
        }
        fs::write(&path, lines)?;

        Ok(WriteOutcome {
            artifacts: vec![path],
            target_session_id: sid.clone(),
            resume_hint: format!("cd {cwd} && claude --resume {sid}"),
        })
    }
}

/// Cheap metadata for `list`: cwd from the first line carrying one, title from
/// an `ai-title` line or the first user text snippet.
fn cc_meta(path: &std::path::Path) -> (Option<String>, Option<String>) {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (None, None),
    };
    let mut cwd = None;
    let mut title = None;
    let mut first_user = None;
    for line in text.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if cwd.is_none() {
            cwd = v.get("cwd").and_then(Value::as_str).map(str::to_string);
        }
        if title.is_none() {
            title = v.get("aiTitle").and_then(Value::as_str).map(str::to_string);
        }
        if first_user.is_none() && v.get("type").and_then(Value::as_str) == Some("user") {
            if let Some(m) = v.get("message") {
                let t = match m.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(a)) => a
                        .iter()
                        .filter_map(|c| c.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => String::new(),
                };
                if !t.trim().is_empty() {
                    first_user = Some(t.chars().take(70).collect::<String>());
                }
            }
        }
        if cwd.is_some() && title.is_some() {
            break;
        }
    }
    (cwd, title.or(first_user))
}
