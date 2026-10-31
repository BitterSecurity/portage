//! GitHub Copilot CLI adapter — READ-ONLY source for now.
//! Store: `<root>/.copilot/session-state/<id>/events.jsonl` (+ workspace.yaml,
//! plan.md, checkpoints/). Resume: `copilot --resume <id>`.
//!
//! This is here to prove the adapter pattern extends beyond the two fully
//! implemented harnesses. The events.jsonl schema is undocumented; the reader
//! is tolerant and best-effort. Writing is not implemented until the event
//! schema is verified against a real install (see design §4 gotchas).

use crate::harness::{Adapter, SessionRef, WriteOpts, WriteOutcome};
use crate::ir::{Block, Message, Role, Session};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

pub struct Copilot {
    root: PathBuf,
}

impl Copilot {
    pub fn new_opt(root: Option<PathBuf>) -> Self {
        Copilot { root: root.unwrap_or_else(|| dirs::home_dir().unwrap_or_default()) }
    }
    fn base(&self) -> PathBuf {
        self.root.join(".copilot").join("session-state")
    }
}

impl Adapter for Copilot {
    fn name(&self) -> &'static str { "copilot" }
    fn can_read(&self) -> bool { true }
    fn can_write(&self) -> bool { false }

    fn list(&self) -> Result<Vec<SessionRef>> {
        let base = self.base();
        let mut out = Vec::new();
        if !base.exists() {
            return Ok(out);
        }
        for e in fs::read_dir(&base)? {
            let dir = e?.path();
            let events = dir.join("events.jsonl");
            if events.exists() {
                out.push(SessionRef {
                    locator: events.to_string_lossy().into_owned(),
                    title: None,
                    cwd: None,
                    modified: None,
                });
            }
        }
        Ok(out)
    }

    fn read(&self, locator: &str) -> Result<Session> {
        let text = fs::read_to_string(locator)
            .with_context(|| format!("reading copilot events {locator}"))?;
        let mut session = Session::new(self.name());
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // Tolerant: accept {role, text|content} shaped events.
            let role = match v.get("role").and_then(Value::as_str) {
                Some("assistant") => Role::Assistant,
                Some("system") => Role::System,
                Some("user") => Role::User,
                _ => continue,
            };
            let text = v
                .get("text")
                .or_else(|| v.get("content"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                continue;
            }
            session.messages.push(Message {
                role,
                blocks: vec![Block::Text { text }],
                timestamp: v.get("timestamp").and_then(Value::as_str).map(str::to_string),
            });
        }
        Ok(session)
    }

    fn write(&self, _session: &Session, _opts: &WriteOpts) -> Result<WriteOutcome> {
        bail!("copilot write is not implemented yet (event schema needs live verification); use copilot only as a --from source")
    }
}
