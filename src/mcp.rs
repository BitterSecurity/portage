//! Minimal MCP server over stdio (JSON-RPC 2.0, newline-delimited messages).
//! Exposes `list`, `inspect`, and `convert` as tools so an agent running inside
//! a harness can, e.g., "import my last Codex session". This is a deliberately
//! small implementation of the parts of MCP that matter here; it is not a full
//! protocol implementation.

use crate::adapters;
use crate::convert::{self, ConvertOpts, Mode};
use crate::find;
use crate::harness::WriteOpts;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2024-11-05";

pub fn serve_stdio() -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");

        let result = match method {
            "initialize" => Some(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "portage", "version": env!("CARGO_PKG_VERSION") },
            })),
            "tools/list" => Some(json!({ "tools": tool_defs() })),
            "tools/call" => Some(handle_call(req.get("params"))),
            _ => None, // notifications (no id) and unknown methods
        };

        // Only requests (those carrying an id) get a response.
        if let (Some(id), Some(result)) = (id, result) {
            let out = json!({ "jsonrpc": "2.0", "id": id, "result": result });
            writeln!(stdout, "{}", serde_json::to_string(&out)?)?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn tool_defs() -> Value {
    json!([
        {
            "name": "list",
            "description": "List sessions found in a harness store.",
            "inputSchema": {
                "type": "object",
                "properties": { "from": { "type": "string", "description": "harness name, e.g. codex" } }
            }
        },
        {
            "name": "find",
            "description": "Find sessions in a harness to continue. The user may describe one vaguely or from memory (e.g. 'the session where I did the login page and the password encryption'). YOU do the semantic matching: extract candidate keywords (include synonyms and both English and the user's language, e.g. login/auth/암호화/encryption), call this (more than once if needed), read each candidate's one-line summary, and pick the best fit — then call `convert` with that session. A keyword miss falls back to recent sessions so you can still browse.",
            "inputSchema": {
                "type": "object",
                "required": ["from"],
                "properties": {
                    "from": { "type": "string", "description": "harness to search, e.g. codex" },
                    "query": { "type": "string", "description": "candidate keywords describing the session" },
                    "latest": { "type": "boolean", "description": "return only the most recent session" }
                }
            }
        },
        {
            "name": "inspect",
            "description": "Show the parsed structure of one session.",
            "inputSchema": {
                "type": "object",
                "required": ["from", "session"],
                "properties": {
                    "from": { "type": "string" },
                    "session": { "type": "string", "description": "session locator/path" }
                }
            }
        },
        {
            "name": "convert",
            "description": "Convert a session from one harness into another and register it so the target can resume it. Identify the source by `session` (path), `match` (description keywords), or `latest`.",
            "inputSchema": {
                "type": "object",
                "required": ["from", "to"],
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" },
                    "session": { "type": "string", "description": "exact session locator/path" },
                    "match": { "type": "string", "description": "find the source session by description keywords" },
                    "latest": { "type": "boolean", "description": "use the most recent source session" },
                    "cwd": { "type": "string" },
                    "mode": { "type": "string", "enum": ["handoff", "replay"], "description": "handoff (default) = compact briefing + recent tail; replay = full 1:1" }
                }
            }
        }
    ])
}

fn handle_call(params: Option<&Value>) -> Value {
    match call_inner(params) {
        Ok(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
        Err(e) => json!({ "content": [{ "type": "text", "text": e.to_string() }], "isError": true }),
    }
}

fn call_inner(params: Option<&Value>) -> Result<String> {
    let p = params.ok_or_else(|| anyhow::anyhow!("missing params"))?;
    let name = p.get("name").and_then(Value::as_str).unwrap_or("");
    let args = p.get("arguments").cloned().unwrap_or(Value::Null);
    let get = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);

    match name {
        "list" => {
            let from = get("from").ok_or_else(|| anyhow::anyhow!("'from' required"))?;
            let a = adapters::by_name(&from)?;
            let mut refs = a.list()?;
            refs.sort_by(|x, y| {
                let k = |r: &crate::harness::SessionRef| r.modified.clone().unwrap_or_default();
                k(y).cmp(&k(x))
            });
            let rows: Vec<Value> = refs
                .iter()
                .map(|r| {
                    json!({
                        "harness": a.name(),
                        "title": r.title.clone().unwrap_or_default(),
                        "cwd": r.cwd.clone().unwrap_or_default(),
                        "modified": r.modified.clone().unwrap_or_default(),
                        "locator": r.locator,
                    })
                })
                .collect();
            Ok(crate::output::render(&json!({ "sessions": rows }), crate::output::Format::Toon))
        }
        "find" => {
            let from = get("from").ok_or_else(|| anyhow::anyhow!("'from' required"))?;
            let a = adapters::by_name(&from)?;
            let (payload, note) = if args.get("latest").and_then(Value::as_bool) == Some(true) {
                match find::latest(a.as_ref())? {
                    Some(r) => (find_payload(a.name(), "", true, &[find::Hit {
                        score: 0, terms_matched: 0, summary: None, reference: r,
                    }]), None),
                    None => (json!({ "from": a.name(), "latest": true, "hits": [] }),
                             Some("no sessions found".to_string())),
                }
            } else {
                let q = get("query").ok_or_else(|| anyhow::anyhow!("'query' or 'latest' required"))?;
                let hits = find::search(a.as_ref(), &q, 8)?;
                if hits.is_empty() {
                    (find_payload(a.name(), &q, false, &find::browse(a.as_ref(), 8)?),
                     Some(format!("no keyword match for \"{q}\" — recent sessions follow")))
                } else {
                    (find_payload(a.name(), &q, false, &hits), None)
                }
            };
            if let Some(n) = note {
                return Ok(format!("# {n}\n{}", crate::output::render(&payload, crate::output::Format::Toon)));
            }
            Ok(crate::output::render(&payload, crate::output::Format::Toon))
        }
        "inspect" => {
            let from = get("from").ok_or_else(|| anyhow::anyhow!("'from' required"))?;
            let session = get("session").ok_or_else(|| anyhow::anyhow!("'session' required"))?;
            let s = adapters::by_name(&from)?.read(&session)?;
            let (mut text, mut reasoning, mut calls, mut results) = (0, 0, 0, 0);
            for m in &s.messages {
                for b in &m.blocks {
                    match b {
                        crate::ir::Block::Text { .. } => text += 1,
                        crate::ir::Block::Reasoning { .. } => reasoning += 1,
                        crate::ir::Block::ToolCall { .. } => calls += 1,
                        crate::ir::Block::ToolResult { .. } => results += 1,
                    }
                }
            }
            Ok(crate::output::render(
                &json!({
                    "source": s.source_harness,
                    "locator": session,
                    "cwd": s.cwd.clone().unwrap_or_default(),
                    "created": s.created.clone().unwrap_or_default(),
                    "messages": s.messages.len(),
                    "blocks": { "text": text, "reasoning": reasoning, "tool_calls": calls, "tool_results": results },
                }),
                crate::output::Format::Toon,
            ))
        }
        "convert" => {
            let from = get("from").ok_or_else(|| anyhow::anyhow!("'from' required"))?;
            let to = get("to").ok_or_else(|| anyhow::anyhow!("'to' required"))?;
            let src = adapters::by_name(&from)?;
            let dst = adapters::by_name(&to)?;
            let locator = if let Some(s) = get("session") {
                s
            } else if args.get("latest").and_then(Value::as_bool) == Some(true) {
                find::latest(src.as_ref())?
                    .map(|r| r.locator)
                    .ok_or_else(|| anyhow::anyhow!("no sessions in {from}"))?
            } else if let Some(q) = get("match") {
                find::search(src.as_ref(), &q, 1)?
                    .into_iter()
                    .next()
                    .map(|h| h.reference.locator)
                    .ok_or_else(|| anyhow::anyhow!("no {from} session matched \"{q}\""))?
            } else {
                anyhow::bail!("provide 'session', 'match', or 'latest'");
            };
            let mode = match get("mode").as_deref() {
                Some("replay") => Mode::Replay,
                _ => Mode::Handoff,
            };
            let opts = ConvertOpts {
                mode,
                tail: 8,
                write: WriteOpts { handshake: true, cwd_override: get("cwd"), force: false },
            };
            let (outcome, report) = convert::run(src.as_ref(), dst.as_ref(), &locator, &opts)?;
            Ok(crate::output::render(
                &json!({
                    "source": locator,
                    "target": to,
                    "target_session_id": outcome.target_session_id,
                    "resume": outcome.resume_hint,
                    "artifacts": outcome.artifacts.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>(),
                    "notes": report.notes,
                }),
                crate::output::Format::Toon,
            ))
        }
        other => anyhow::bail!("unknown tool: {other}"),
    }
}

/// Shared with the CLI: the find result table (harness-scoped fields).
fn find_payload(from: &str, query: &str, latest: bool, hits: &[find::Hit]) -> Value {
    let rows: Vec<Value> = hits
        .iter()
        .map(|h| {
            json!({
                "terms": h.terms_matched,
                "modified": h.reference.modified.clone().unwrap_or_default(),
                "title": h.reference.title.clone().unwrap_or_default(),
                "cwd": h.reference.cwd.clone().unwrap_or_default(),
                "summary": h.summary.clone().unwrap_or_default(),
                "locator": h.reference.locator,
            })
        })
        .collect();
    json!({ "from": from, "query": query, "latest": latest, "hits": rows })
}
