//! The conversion pipeline: `source.read -> IR -> (handshake) -> target.write`.
//! Asymmetries that cannot cross losslessly are collected in a
//! `ConversionReport`, never silently dropped.

use crate::digest;
use crate::harness::{Adapter, WriteOpts, WriteOutcome};
use crate::ir::{Block, Message, Role, Session};
use anyhow::{bail, Result};

#[derive(Debug, Default)]
pub struct ConversionReport {
    pub notes: Vec<String>,
}

/// How much of the source session to carry over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Compact, action-preserving briefing + recent text tail (default for
    /// continuing work in another harness).
    Handoff,
    /// Faithful 1:1 replay of every message (archival / debugging).
    Replay,
}

pub struct ConvertOpts {
    pub mode: Mode,
    /// Recent messages kept verbatim in handoff mode.
    pub tail: usize,
    pub write: WriteOpts,
}

impl Default for ConvertOpts {
    fn default() -> Self {
        ConvertOpts { mode: Mode::Handoff, tail: 8, write: WriteOpts::default() }
    }
}

/// Run a conversion between two already-constructed adapters.
pub fn run(
    source: &dyn Adapter,
    target: &dyn Adapter,
    locator: &str,
    opts: &ConvertOpts,
) -> Result<(WriteOutcome, ConversionReport)> {
    if !source.can_read() {
        bail!("{} cannot be used as a source", source.name());
    }
    if !target.can_write() {
        bail!("{} cannot be used as a target (resume not supported here)", target.name());
    }

    let raw = source.read(locator)?;
    let mut report = ConversionReport::default();

    let session = match opts.mode {
        Mode::Handoff => {
            let s = digest::build_handoff_session(&raw, source.name(), target.name(), opts.tail);
            report.notes.push(format!(
                "handoff: {} source messages → briefing + last {} text turns (reasoning & stale tool output dropped)",
                raw.messages.len(),
                s.messages.len().saturating_sub(1)
            ));
            s
        }
        Mode::Replay => {
            let mut s = raw;
            report.notes.push(format!("replay: {} messages 1:1", s.messages.len()));
            if s.messages.iter().any(|m| m.blocks.iter().any(|b| matches!(b, Block::Reasoning { .. }))) {
                report.notes.push(
                    "reasoning kept as summary text only (signatures/opaque payloads dropped)".into(),
                );
            }
            if opts.write.handshake {
                let p = preamble(&s, source.name());
                s.messages.insert(0, p);
                report.notes.push("injected resume preamble".into());
            }
            s
        }
    };

    let outcome = target.write(&session, &opts.write)?;
    Ok((outcome, report))
}

fn preamble(session: &Session, source: &str) -> Message {
    let mut t = format!(
        "[portage] This session was carried over from {source}. Continue from where it left off."
    );
    if !session.pending.queued_input.is_empty() {
        t.push_str("\nQueued but unsent input: ");
        t.push_str(&session.pending.queued_input.join(" | "));
    }
    if !session.pending.open_tool_calls.is_empty() {
        t.push_str("\nTool calls that were still open: ");
        t.push_str(&session.pending.open_tool_calls.join(", "));
    }
    Message { role: Role::User, blocks: vec![Block::Text { text: t }], timestamp: None }
}
