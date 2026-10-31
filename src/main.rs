use anyhow::Result;
use clap::{Parser, Subcommand};
use portage::adapters;
use portage::convert;
use portage::harness::WriteOpts;
use portage::ir::{Block, Session};

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
    /// Show the parsed structure of one session.
    Inspect {
        session: String,
        #[arg(long)]
        from: String,
    },
    /// Convert a session from one harness into another and register it.
    Convert {
        session: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        /// Override the cwd the session is registered under (Claude Code needs it).
        #[arg(long)]
        cwd: Option<String>,
        /// Do not inject the resume preamble.
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
        Cmd::Inspect { session, from } => inspect(&from, &session),
        Cmd::Convert { session, from, to, cwd, no_handshake, force } => {
            let src = adapters::by_name(&from)?;
            let dst = adapters::by_name(&to)?;
            let opts = WriteOpts { handshake: !no_handshake, cwd_override: cwd, force };
            let (outcome, report) = convert::run(src.as_ref(), dst.as_ref(), &session, &opts)?;
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
        let refs = a.list()?;
        println!("[{}] {} session(s)", a.name(), refs.len());
        for r in refs {
            println!("  {}", r.locator);
        }
    }
    Ok(())
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
