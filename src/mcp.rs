//! Minimal MCP server over stdio (JSON-RPC 2.0, newline-delimited messages).
//! Exposes `list`, `inspect`, and `convert` as tools so an agent running inside
//! a harness can, e.g., "import my last Codex session". This is a deliberately
//! small implementation of the parts of MCP that matter here; it is not a full
//! protocol implementation.

use crate::adapters;
use crate::convert;
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
            "description": "Convert a session from one harness into another and register it so the target can resume it.",
            "inputSchema": {
                "type": "object",
                "required": ["from", "to", "session"],
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" },
                    "session": { "type": "string" },
                    "cwd": { "type": "string" }
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
            let refs = a.list()?;
            Ok(refs.into_iter().map(|r| r.locator).collect::<Vec<_>>().join("\n"))
        }
        "inspect" => {
            let from = get("from").ok_or_else(|| anyhow::anyhow!("'from' required"))?;
            let session = get("session").ok_or_else(|| anyhow::anyhow!("'session' required"))?;
            let s = adapters::by_name(&from)?.read(&session)?;
            Ok(format!("{} messages from {}", s.messages.len(), s.source_harness))
        }
        "convert" => {
            let from = get("from").ok_or_else(|| anyhow::anyhow!("'from' required"))?;
            let to = get("to").ok_or_else(|| anyhow::anyhow!("'to' required"))?;
            let session = get("session").ok_or_else(|| anyhow::anyhow!("'session' required"))?;
            let src = adapters::by_name(&from)?;
            let dst = adapters::by_name(&to)?;
            let opts = WriteOpts { handshake: true, cwd_override: get("cwd"), force: false };
            let (outcome, report) = convert::run(src.as_ref(), dst.as_ref(), &session, &opts)?;
            Ok(format!("{}\nresume: {}", report.notes.join("; "), outcome.resume_hint))
        }
        other => anyhow::bail!("unknown tool: {other}"),
    }
}
