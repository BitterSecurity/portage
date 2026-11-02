use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use portage::adapters;
use portage::harness::WriteOpts;
use portage::ir::Block;
use portage::output::{parse_format, render, Format};
use portage::{convert, find};
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "portage",
    version,
    about = "Carry an AI coding-agent session between harnesses and resume it natively."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// One output contract for every command: TOON by default (token-compact,
/// uniform structure an agent parses once), `--format json|text` to switch,
/// `PORTAGE_FORMAT` to pin a default for the environment.

#[derive(Subcommand)]
enum Cmd {
    /// List sessions in a harness store (or across all readable harnesses).
    List {
        #[arg(long)]
        from: Option<String>,
        /// Output format: toon (default), json, text.
        #[arg(long, env = "PORTAGE_FORMAT", default_value = "toon")]
        format: String,
    },
    /// Find sessions in a harness by description keywords and/or recency.
    Find {
        /// Description keywords (optional if --latest).
        query: Vec<String>,
        #[arg(long)]
        from: String,
        /// Show only the most recently modified session.
        #[arg(long)]
        latest: bool,
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// Output format: toon (default), json, text.
        #[arg(long, env = "PORTAGE_FORMAT", default_value = "toon")]
        format: String,
    },
    /// Show the parsed structure of one session.
    Inspect {
        session: String,
        #[arg(long)]
        from: String,
        /// Output format: toon (default), json, text.
        #[arg(long, env = "PORTAGE_FORMAT", default_value = "toon")]
        format: String,
    },
    /// Convert a session from one harness into another and register it.
    ///
    /// Identify the source session by path, by `--match <keywords>`, or
    /// `--latest`.
    Convert {
        /// Session locator/path (omit if using --match or --latest).
        session: Option<String>,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        /// Find the source session by description keywords instead of a path.
        #[arg(long = "match")]
        match_query: Option<String>,
        /// Use the most recent source session.
        #[arg(long)]
        latest: bool,
        /// handoff = compact briefing + recent tail (default); replay = full 1:1.
        #[arg(long, default_value = "handoff")]
        mode: String,
        /// Recent messages kept verbatim in handoff mode.
        #[arg(long, default_value_t = 8)]
        tail: usize,
        /// Override the cwd the session is registered under (Claude Code needs it).
        #[arg(long)]
        cwd: Option<String>,
        /// Replay mode only: do not inject the resume preamble.
        #[arg(long)]
        no_handshake: bool,
        /// Write even if the target store schema is unrecognized.
        #[arg(long)]
        force: bool,
        /// Output format: toon (default), json, text.
        #[arg(long, env = "PORTAGE_FORMAT", default_value = "toon")]
        format: String,
    },
    /// Run the MCP server over stdio (exposes list/inspect/convert as tools).
    Mcp,
    /// Print supported harnesses and their read/write capability.
    Harnesses,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::List { from, format } => list(from, parse_format(&format)?),
        Cmd::Find { query, from, latest, limit, format } => {
            find_cmd(&from, &query.join(" "), latest, limit, parse_format(&format)?)
        }
        Cmd::Inspect { session, from, format } => inspect(&from, &session, parse_format(&format)?),
        Cmd::Convert { session, from, to, mode, tail, match_query, latest, cwd, no_handshake, force, format } => {
            let src = adapters::by_name(&from)?;
            let dst = adapters::by_name(&to)?;
            let locator = resolve_locator(src.as_ref(), session, match_query, latest)?;
            eprintln!("source session: {locator}");
            let mode = match mode.as_str() {
                "handoff" => convert::Mode::Handoff,
                "replay" => convert::Mode::Replay,
                other => anyhow::bail!("unknown --mode '{other}' (use handoff or replay)"),
            };
            let opts = convert::ConvertOpts {
                mode,
                tail,
                write: WriteOpts { handshake: !no_handshake, cwd_override: cwd, force },
            };
            let (outcome, report) = convert::run(src.as_ref(), dst.as_ref(), &locator, &opts)?;
            for n in &report.notes {
                eprintln!("• {n}");
            }
            let payload = json!({
                "source": locator,
                "target": to,
                "target_session_id": outcome.target_session_id,
                "resume": outcome.resume_hint,
                "artifacts": outcome.artifacts.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>(),
                "notes": report.notes,
            });
            print!("{}", render(&payload, parse_format(&format)?));
            Ok(())
        }
        Cmd::Mcp => portage::mcp::serve_stdio(),
        Cmd::Harnesses => {
            println!("{:<14} read  write", "harness");
            for (name, r, w) in adapters::SUPPORTED {
                println!("{:<14} {:<5} {}", name, yn(*r), yn(*w));
            }
            Ok(())
        }
    }
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// Payload for `list`: one table across harnesses (each row keeps its harness).
fn list_rows(name: &str, refs: &[portage::harness::SessionRef]) -> Vec<serde_json::Value> {
    refs.iter()
        .map(|r| {
            json!({
                "harness": name,
                "title": r.title.clone().unwrap_or_default(),
                "cwd": r.cwd.clone().unwrap_or_default(),
                "modified": r.modified.clone().unwrap_or_default(),
                "locator": r.locator,
            })
        })
        .collect()
}

fn list(from: Option<String>, fmt: Format) -> Result<()> {
    let names: Vec<&str> = match &from {
        Some(f) => vec![f.as_str()],
        None => adapters::SUPPORTED.iter().filter(|(_, r, _)| *r).map(|(n, _, _)| *n).collect(),
    };
    let mut all: Vec<serde_json::Value> = Vec::new();
    for name in names {
        let a = adapters::by_name(name)?;
        let mut refs = a.list()?;
        // newest first by modified (fall back to locator mtime)
        refs.sort_by(|x, y| ref_mtime(y).cmp(&ref_mtime(x)));
        all.extend(list_rows(a.name(), &refs));
    }
    // Newest first across the merged table.
    all.sort_by(|x, y| {
        let k = |v: &serde_json::Value| v["modified"].as_str().unwrap_or("").to_string();
        k(y).cmp(&k(x))
    });
    print!("{}", render(&serde_json::json!({ "sessions": all }), fmt));
    Ok(())
}

fn find_payload(from: &str, query: &str, latest: bool, hits: &[find::Hit]) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = hits
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
    serde_json::json!({
        "from": from,
        "query": query,
        "latest": latest,
        "hits": rows,
    })
}

fn find_cmd(from: &str, query: &str, latest: bool, limit: usize, fmt: Format) -> Result<()> {
    let a = adapters::by_name(from)?;
    let (payload, fallback_note) = if latest {
        match find::latest(a.as_ref())? {
            Some(r) => (
                find_payload(from, "", true, &[find::Hit {
                    score: 0,
                    terms_matched: 0,
                    summary: None,
                    reference: r,
                }]),
                None,
            ),
            None => (serde_json::json!({ "from": from, "latest": true, "hits": [] }), None),
        }
    } else {
        if query.trim().is_empty() {
            bail!("give description keywords, or pass --latest");
        }
        let hits = find::search(a.as_ref(), query, limit)?;
        if hits.is_empty() {
            (
                find_payload(from, query, false, &find::browse(a.as_ref(), limit)?),
                Some(format!("no keyword match for \"{query}\" — recent sessions follow")),
            )
        } else {
            (find_payload(from, query, false, &hits), None)
        }
    };
    if let Some(note) = fallback_note {
        eprintln!("• {note}");
    }
    print!("{}", render(&payload, fmt));
    Ok(())
}

/// Resolve the source session from an explicit path, `--match`, or `--latest`.
fn resolve_locator(
    src: &dyn portage::harness::Adapter,
    session: Option<String>,
    match_query: Option<String>,
    latest: bool,
) -> Result<String> {
    if let Some(s) = session {
        return Ok(s);
    }
    if latest {
        return find::latest(src)?
            .map(|r| r.locator)
            .ok_or_else(|| anyhow::anyhow!("no sessions found in {}", src.name()));
    }
    if let Some(q) = match_query {
        let hits = find::search(src, &q, 5)?;
        let top = hits
            .first()
            .ok_or_else(|| anyhow::anyhow!("no {} session matched \"{}\"", src.name(), q))?;
        if hits.len() > 1 {
            eprintln!(
                "note: {} candidates matched; picked the strongest. Use `portage find` to review.",
                hits.len()
            );
        }
        return Ok(top.reference.locator.clone());
    }
    bail!("specify a session path, --match <keywords>, or --latest")
}

fn ref_mtime(r: &portage::harness::SessionRef) -> Option<std::time::SystemTime> {
    std::fs::metadata(&r.locator).ok().and_then(|m| m.modified().ok())
}

fn inspect(from: &str, locator: &str, fmt: Format) -> Result<()> {
    let a = adapters::by_name(from)?;
    let s = a.read(locator)?;
    let (mut text, mut reasoning, mut calls, mut results) = (0, 0, 0, 0);
    for m in &s.messages {
        for b in &m.blocks {
            match b {
                Block::Text { .. } => text += 1,
                Block::Reasoning { .. } => reasoning += 1,
                Block::ToolCall { .. } => calls += 1,
                Block::ToolResult { .. } => results += 1,
            }
        }
    }
    let payload = serde_json::json!({
        "source": s.source_harness,
        "locator": locator,
        "cwd": s.cwd.clone().unwrap_or_default(),
        "created": s.created.clone().unwrap_or_default(),
        "messages": s.messages.len(),
        "blocks": { "text": text, "reasoning": reasoning, "tool_calls": calls, "tool_results": results },
    });
    print!("{}", render(&payload, fmt));
    Ok(())
}
