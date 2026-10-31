use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use portage::adapters;
use portage::harness::{mtime_str, WriteOpts};
use portage::ir::{Block, Session};
use portage::{convert, find};

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

#[derive(Subcommand)]
enum Cmd {
    /// List sessions in a harness store (or across all readable harnesses).
    List {
        #[arg(long)]
        from: Option<String>,
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
    },
    /// Show the parsed structure of one session.
    Inspect {
        session: String,
        #[arg(long)]
        from: String,
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
    },
    /// Run the MCP server over stdio (exposes list/inspect/convert as tools).
    Mcp,
    /// Print supported harnesses and their read/write capability.
    Harnesses,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::List { from } => list(from),
        Cmd::Find { query, from, latest, limit } => find_cmd(&from, &query.join(" "), latest, limit),
        Cmd::Inspect { session, from } => inspect(&from, &session),
        Cmd::Convert { session, from, to, mode, tail, match_query, latest, cwd, no_handshake, force } => {
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
            println!("wrote {} artifact(s):", outcome.artifacts.len());
            for a in &outcome.artifacts {
                println!("  {}", a.display());
            }
            println!("resume with:\n  {}", outcome.resume_hint);
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

fn list(from: Option<String>) -> Result<()> {
    let names: Vec<&str> = match &from {
        Some(f) => vec![f.as_str()],
        None => adapters::SUPPORTED.iter().filter(|(_, r, _)| *r).map(|(n, _, _)| *n).collect(),
    };
    for name in names {
        let a = adapters::by_name(name)?;
        let mut refs = a.list()?;
        // newest first by modified (fall back to locator mtime)
        refs.sort_by(|x, y| ref_mtime(y).cmp(&ref_mtime(x)));
        println!("[{}] {} session(s)", a.name(), refs.len());
        for r in &refs {
            print_ref(r);
        }
    }
    Ok(())
}

fn find_cmd(from: &str, query: &str, latest: bool, limit: usize) -> Result<()> {
    let a = adapters::by_name(from)?;
    if latest {
        match find::latest(a.as_ref())? {
            Some(r) => {
                println!("[{}] most recent:", a.name());
                print_ref(&r);
            }
            None => println!("[{}] no sessions found", a.name()),
        }
        return Ok(());
    }
    if query.trim().is_empty() {
        bail!("give description keywords, or pass --latest");
    }
    let hits = find::search(a.as_ref(), query, limit)?;
    if hits.is_empty() {
        println!("[{}] no session matched \"{}\"", a.name(), query);
        return Ok(());
    }
    println!("[{}] {} match(es) for \"{}\":", a.name(), hits.len(), query);
    for h in &hits {
        print!("  [{} terms] ", h.terms_matched);
        print_ref(&h.reference);
    }
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

fn print_ref(r: &portage::harness::SessionRef) {
    let when = r.modified.clone().or_else(|| mtime_str(std::path::Path::new(&r.locator)));
    let label = r.title.clone().or_else(|| r.cwd.clone()).unwrap_or_default();
    println!("  {:<20} {}", when.as_deref().unwrap_or("-"), label);
    println!("    {}", r.locator);
}

fn inspect(from: &str, locator: &str) -> Result<()> {
    let a = adapters::by_name(from)?;
    let s = a.read(locator)?;
    print_summary(&s);
    Ok(())
}

fn print_summary(s: &Session) {
    println!("source harness : {}", s.source_harness);
    println!("cwd            : {}", s.cwd.as_deref().unwrap_or("-"));
    println!("created        : {}", s.created.as_deref().unwrap_or("-"));
    println!("messages       : {}", s.messages.len());
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
    println!("blocks         : text={text} reasoning={reasoning} tool_calls={calls} tool_results={results}");
}
