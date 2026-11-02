//! opencode adapter.
//!
//! Store (verified against a real install): SQLite at
//! `<root>/.local/share/opencode/opencode.db` with tables `session`,
//! `message` (data JSON holds `role`), and `part` (data JSON, `type` ∈
//! text / reasoning / tool / step-start / step-finish / patch / compaction).
//! A `tool` part carries both the call and its result in `state`
//! (`state.input`, `state.output`). Resume: `opencode --session <id>`.
//!
//! Writing registers DB rows the same way the app itself does: one `session`
//! row, one `message` row per turn (assistant rows chained via `parentID`),
//! one `part` row per content block. `tool` parts hold the call and its
//! result together in `state`; results stored in a later message (how Claude
//! Code represents them) are merged into the part of their call. No event
//! rows are needed — the materialized tables are the store's source of truth;
//! the event log is a runtime pub/sub concern. The whole write is one
//! transaction: a failure leaves the store untouched.
//!
//! Identifiers embed the creation timestamp (`{prefix}_{ms:013x}{k:04x}`), so
//! the store's `ORDER BY time_created, id` / `ORDER BY id` reads reconstruct
//! conversation and block order exactly as written. Forks of opencode keep
//! their own data dir (`PORTAGE_OPENCODE_DB`); the resume hint follows the
//! store's directory name so a redirected store suggests the right binary.

use crate::harness::{Adapter, SessionRef, WriteOpts, WriteOutcome};
use crate::ir::{Block, Message, Role, Session};
use anyhow::{bail, Context, Result};
use rusqlite::{params_from_iter, Connection};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub struct OpenCode {
    /// Test/explicit home override; None => per-OS data dir.
    root: Option<PathBuf>,
}

impl OpenCode {
    pub fn new_opt(root: Option<PathBuf>) -> Self {
        OpenCode { root }
    }
    /// opencode stores its DB in the OS data dir, which differs per platform:
    /// Linux/WSL `~/.local/share`, macOS `~/Library/Application Support`,
    /// Windows `%APPDATA%`. `dirs::data_dir()` resolves the right one.
    /// `PORTAGE_OPENCODE_DB` overrides the full path for nonstandard installs
    /// (opencode-compatible forks that keep their own data dir).
    fn db_path(&self) -> PathBuf {
        if let Ok(p) = std::env::var("PORTAGE_OPENCODE_DB") {
            return PathBuf::from(p);
        }
        let base = match &self.root {
            Some(r) => r.join(".local").join("share"),
            None => dirs::data_dir()
                .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local").join("share")),
        };
        base.join("opencode").join("opencode.db")
    }
    fn open(&self) -> Result<Connection> {
        let p = self.db_path();
        Connection::open(&p).with_context(|| format!("opening opencode db at {}", p.display()))
    }
    /// Binary to suggest in the resume hint. Upstream lives in `opencode/`;
    /// a store redirected elsewhere usually belongs to a fork named after its
    /// data dir.
    fn resume_bin(&self) -> String {
        if std::env::var("PORTAGE_OPENCODE_DB").is_ok() {
            if let Some(name) = self.db_path().parent().and_then(Path::file_name) {
                return name.to_string_lossy().into_owned();
            }
        }
        "opencode".to_string()
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
    fn can_write(&self) -> bool { true }

    fn list(&self) -> Result<Vec<SessionRef>> {
        if !self.db_path().exists() {
            return Ok(vec![]);
        }
        let conn = self.open()?;
        let mut stmt = conn.prepare(
            "SELECT id, directory, title, time_updated FROM session ORDER BY time_updated DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let updated: Option<i64> = r.get(3)?;
            Ok(SessionRef {
                locator: r.get::<_, String>(0)?,
                title: r.get::<_, Option<String>>(2)?,
                cwd: r.get::<_, Option<String>>(1)?,
                modified: updated.and_then(ms_to_rfc3339),
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

    fn write(&self, session: &Session, opts: &WriteOpts) -> Result<WriteOutcome> {
        let cwd = opts
            .cwd_override
            .clone()
            .or_else(|| session.cwd.clone())
            .ok_or_else(|| anyhow::anyhow!("opencode resume needs a cwd; pass --cwd"))?;
        let db = self.db_path();
        if !db.exists() {
            bail!(
                "opencode store not found at {} — run opencode once so it creates its store",
                db.display()
            );
        }
        let mut conn = self.open()?;
        let tx = conn.transaction()?;

        let model = resolve_model(&tx)?;
        let project_id = resolve_project(&tx, &cwd)?;
        let now = now_ms();
        let version: String = tx
            .query_row(
                "SELECT version FROM session ORDER BY time_updated DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "0.0.0".into());

        // Monotonic id counter: every row gets base + k, so `ORDER BY id`
        // anywhere in the store replays creation order.
        let base = now;
        let next_id = |k: i64, prefix: &str| format!("{prefix}_{:013x}", base + k);
        let mut k: i64 = 0;

        let sid = next_id(k, "ses");
        k += 1;
        let slug = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let created = session
            .created
            .as_deref()
            .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
            .map(|d| (d.unix_timestamp_nanos() / 1_000_000) as i64)
            .unwrap_or(now);
        let title =
            session.title.clone().unwrap_or_else(|| "(ported session)".into());

        insert_row(
            &tx,
            "session",
            &[
                ("id", Value::from(sid.clone())),
                ("project_id", Value::from(project_id)),
                ("slug", Value::from(slug)),
                ("directory", Value::from(cwd.clone())),
                ("title", Value::from(title)),
                ("version", Value::from(version)),
                ("path", Value::from(cwd.trim_start_matches('/'))),
                ("agent", Value::from("build")),
                ("model", Value::from(model.session_json())),
                ("cost", Value::from(0.0)),
                ("tokens_input", Value::from(0)),
                ("tokens_output", Value::from(0)),
                ("tokens_reasoning", Value::from(0)),
                ("tokens_cache_read", Value::from(0)),
                ("tokens_cache_write", Value::from(0)),
                ("summary_additions", Value::from(0)),
                ("summary_deletions", Value::from(0)),
                ("summary_files", Value::from(0)),
                ("time_created", Value::from(created)),
                ("time_updated", Value::from(now)),
            ],
        )?;

        // Build every message and its parts in memory first: tool results can
        // live in a different message than their call (Claude Code puts them
        // in the next user turn), so merging must see all calls before any
        // row is inserted.
        struct Draft {
            id: String,
            ms: i64,
            data: Value,
            parts: Vec<(String, Value)>,
        }
        // call_id -> (draft index, part index) for pending tool parts.
        let mut call_part: std::collections::HashMap<String, (usize, usize)> =
            std::collections::HashMap::new();
        // ToolResult blocks waiting for their call's part.
        let mut results: Vec<(String, Value, bool, usize)> = Vec::new();
        let mut drafts: Vec<Draft> = Vec::new();
        let mut prev_msg: Option<String> = None;

        for m in &session.messages {
            if m.role == Role::System {
                // opencode has no system-message rows (the system prompt lives
                // outside the store); this is the one thing a replay drops.
                eprintln!("• opencode write: system message skipped (no such row type)");
                continue;
            }
            if m.blocks.is_empty() {
                continue;
            }
            let mid = next_id(k, "msg");
            k += 1;
            let ms = m
                .timestamp
                .as_deref()
                .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
                .map(|d| (d.unix_timestamp_nanos() / 1_000_000) as i64)
                .unwrap_or(created);
            let data = message_data(m, &model, prev_msg.as_deref(), &cwd, ms);
            let mut parts: Vec<(String, Value)> = Vec::new();
            for b in &m.blocks {
                match b {
                    Block::Text { text } => parts.push((
                        next_id(k, "prt"),
                        json!({"type": "text", "text": text}),
                    )),
                    Block::Reasoning { text } => parts.push((
                        next_id(k, "prt"),
                        json!({"type": "reasoning", "text": text, "time": {"start": ms, "end": ms}}),
                    )),
                    Block::ToolCall { id, name, input } => {
                        let tool_name = if name.is_empty() { "(unknown)" } else { name.as_str() };
                        call_part.insert(id.clone(), (drafts.len(), parts.len()));
                        parts.push((
                            next_id(k, "prt"),
                            json!({
                                "type": "tool", "tool": tool_name, "callID": id,
                                "state": {"status": "pending", "input": input, "metadata": {},
                                          "title": tool_name, "time": {"start": ms, "end": ms}},
                            }),
                        ));
                    }
                    Block::ToolResult { call_id, output, is_error } => {
                        results.push((call_id.clone(), output.clone(), *is_error, drafts.len()));
                    }
                }
                k += 1;
            }
            prev_msg = Some(mid.clone());
            drafts.push(Draft { id: mid, ms, data, parts });
        }

        // Merge results into their call's part; a result with no call anywhere
        // becomes its own part so nothing is silently dropped.
        for (call_id, output, is_error, msg_i) in results {
            let status = if is_error { "error" } else { "completed" };
            match call_part.get(&call_id).copied() {
                Some((di, pi)) => {
                    let end_ms = drafts[di].ms;
                    let st = drafts[di].parts[pi].1.get_mut("state").unwrap();
                    st["status"] = json!(status);
                    st["output"] = json!(output_to_text(&output));
                    st["time"]["end"] = json!(end_ms);
                }
                None => {
                    let ms = drafts[msg_i].ms;
                    let pid = next_id(k, "prt");
                    k += 1;
                    drafts[msg_i].parts.push((
                        pid,
                        json!({
                            "type": "tool", "tool": "(unknown)", "callID": call_id,
                            "state": {"status": status, "input": {}, "output": output_to_text(&output),
                                      "metadata": {}, "title": "(unknown)",
                                      "time": {"start": ms, "end": ms}},
                        }),
                    ));
                    eprintln!(
                        "• opencode write: tool result {call_id} had no matching call — written as its own part"
                    );
                }
            }
        }

        for d in &drafts {
            insert_row(
                &tx,
                "message",
                &[
                    ("id", Value::from(d.id.clone())),
                    ("session_id", Value::from(sid.clone())),
                    ("time_created", Value::from(d.ms)),
                    ("time_updated", Value::from(d.ms)),
                    ("data", Value::from(serde_json::to_string(&d.data)?)),
                ],
            )?;
            for (pid, data) in &d.parts {
                insert_row(
                    &tx,
                    "part",
                    &[
                        ("id", Value::from(pid.clone())),
                        ("message_id", Value::from(d.id.clone())),
                        ("session_id", Value::from(sid.clone())),
                        ("time_created", Value::from(d.ms)),
                        ("time_updated", Value::from(d.ms)),
                        ("data", Value::from(serde_json::to_string(data)?)),
                    ],
                )?;
            }
        }

        tx.commit()?;

        let bin = self.resume_bin();
        Ok(WriteOutcome {
            artifacts: vec![db],
            target_session_id: sid.clone(),
            resume_hint: format!("cd {cwd} && {bin} --session {sid}"),
        })
    }
}

/// Column-aware INSERT: builds the statement from the columns that actually
/// exist in this store, so opencode schema drift (and opencode-compatible
/// forks) neither breaks the write nor drops required fields silently.
fn insert_row(conn: &Connection, table: &str, cols: &[(&str, Value)]) -> Result<()> {
    let present: Vec<String> = {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut names = Vec::new();
    let mut values = Vec::new();
    let mut missing_core = Vec::new();
    for (name, val) in cols {
        if present.iter().any(|c| c == name) {
            names.push(format!("\"{name}\""));
            values.push(to_sql(val.clone()));
        } else if matches!(*name, "id" | "session_id" | "message_id" | "data" | "time_created" | "time_updated") {
            missing_core.push(*name); // linkage/timing columns must exist or the row is orphaned
        }
    }
    if !missing_core.is_empty() {
        bail!("opencode store table '{table}' is missing required columns {missing_core:?} — unsupported schema");
    }
    let placeholders = vec!["?"; names.len()].join(", ");
    let sql = format!("INSERT INTO {table} ({}) VALUES ({})", names.join(", "), placeholders);
    conn.execute(&sql, params_from_iter(values.iter()))?;
    Ok(())
}

/// JSON value -> SQLite binding. Objects/arrays are stored as JSON text, the
/// way the store itself persists them.
fn to_sql(v: Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as Sv;
    match v {
        Value::Null => Sv::Null,
        Value::Bool(b) => Sv::Integer(b as i64),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Sv::Integer(i),
            None => Sv::Real(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Sv::Text(s),
        other => Sv::Text(other.to_string()),
    }
}

struct Model {
    provider: String,
    id: String,
}
impl Model {
    /// The session's default model, in the store's own shape.
    fn session_json(&self) -> String {
        json!({"id": self.id, "providerID": self.provider}).to_string()
    }
}

/// Where the resumed session's model comes from, in priority order:
/// 1. `PORTAGE_OPENCODE_MODEL=<provider>/<model>` (explicit override)
/// 2. the store's own most recent real model (reuse what this install uses)
/// Failing both is a loud error: resuming with an unresolvable model would
/// only fail later, on the first message.
fn resolve_model(conn: &Connection) -> Result<Model> {
    if let Ok(m) = std::env::var("PORTAGE_OPENCODE_MODEL") {
        let m = m.trim();
        if let Some((provider, id)) = m.split_once('/') {
            if !provider.is_empty() && !id.is_empty() {
                return Ok(Model { provider: provider.into(), id: id.into() });
            }
        }
        bail!("PORTAGE_OPENCODE_MODEL must be <provider>/<model>, got '{m}'");
    }
    let mut stmt = conn.prepare(
        "SELECT model FROM session WHERE model IS NOT NULL AND model <> '' ORDER BY time_updated DESC",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    for row in rows {
        let row = row?;
        if let Ok(v) = serde_json::from_str::<Value>(&row) {
            let id = v
                .get("id")
                .or_else(|| v.get("modelID"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let provider = v.get("providerID").and_then(Value::as_str).unwrap_or("");
            // `<synthetic>` models are internal bookkeeping (title generation
            // etc.), not something a user can resume with.
            if !id.is_empty() && !provider.is_empty() && !id.starts_with('<') {
                return Ok(Model { provider: provider.into(), id: id.into() });
            }
        }
    }
    bail!(
        "no usable model found in the opencode store — set PORTAGE_OPENCODE_MODEL=<provider>/<model> \
         (e.g. from `opencode models`)"
    )
}

/// Project linkage for the session: exact worktree match, then the root
/// project, then any existing row; a fresh store gets the conventional root
/// project (`global` at `/`) created on demand.
fn resolve_project(conn: &Connection, cwd: &str) -> Result<String> {
    for (sql, param) in [
        ("SELECT id FROM project WHERE worktree = ?1 LIMIT 1", Some(cwd)),
        ("SELECT id FROM project WHERE worktree = '/' LIMIT 1", None),
        ("SELECT id FROM project ORDER BY time_created LIMIT 1", None),
    ] {
        let found = match param {
            Some(p) => conn.query_row(sql, [p], |r| r.get::<_, String>(0)).ok(),
            None => conn.query_row(sql, [], |r| r.get::<_, String>(0)).ok(),
        };
        if let Some(id) = found {
            return Ok(id);
        }
    }
    let now = now_ms();
    insert_row(
        conn,
        "project",
        &[
            ("id", Value::from("global")),
            ("worktree", Value::from("/")),
            ("time_created", Value::from(now)),
            ("time_updated", Value::from(now)),
            ("sandboxes", Value::from("{}")),
        ],
    )?;
    Ok("global".into())
}

/// Message info JSON, in the shape the store itself writes: flat model fields
/// plus the message chain on assistant messages, a nested model on user
/// messages.
fn message_data(m: &Message, model: &Model, parent: Option<&str>, cwd: &str, ms: i64) -> Value {
    let role = match m.role {
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::User => "user",
    };
    let mut v = json!({
        "role": role,
        "time": { "created": ms },
        "agent": "build",
    });
    if m.role == Role::Assistant {
        let obj = v.as_object_mut().unwrap();
        obj.insert("modelID".into(), json!(model.id));
        obj.insert("providerID".into(), json!(model.provider));
        obj.insert("mode".into(), json!("build"));
        obj.insert(
            "parentID".into(),
            json!(parent.map(str::to_string).unwrap_or_else(|| format!("msg_{:013x}", ms - 1))),
        );
        obj.insert("path".into(), json!({"cwd": cwd, "root": cwd}));
        obj.insert("cost".into(), json!(0));
        obj.insert(
            "tokens".into(),
            json!({"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}}),
        );
    } else {
        v.as_object_mut().unwrap().insert(
            "model".into(),
            json!({"providerID": model.provider, "modelID": model.id}),
        );
    }
    v
}

fn output_to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn now_ms() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

fn ms_to_rfc3339(ms: i64) -> Option<String> {
    let dt = OffsetDateTime::from_unix_timestamp_nanos((ms * 1_000_000) as i128).ok()?;
    dt.format(&Rfc3339).ok()
}
