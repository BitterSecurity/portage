//! The conversion pipeline: `source.read -> IR -> (handshake) -> target.write`.
//! Asymmetries that cannot cross losslessly are collected in a
//! `ConversionReport`, never silently dropped.

use crate::harness::{Adapter, WriteOpts, WriteOutcome};
use crate::ir::{Block, Message, Role, Session};
use anyhow::{bail, Result};

#[derive(Debug, Default)]
pub struct ConversionReport {
    pub notes: Vec<String>,
}

/// Run a conversion between two already-constructed adapters.
pub fn run(
    source: &dyn Adapter,
    target: &dyn Adapter,
    locator: &str,
    opts: &WriteOpts,
) -> Result<(WriteOutcome, ConversionReport)> {
    if !source.can_read() {
        bail!("{} cannot be used as a source", source.name());
    }
    if !target.can_write() {
        bail!("{} cannot be used as a target (resume not supported here)", target.name());
    }

    let mut session = source.read(locator)?;
    let mut report = ConversionReport::default();

    report.notes.push(format!(
        "mapped {} -> {} ({} messages)",
        source.name(),
        target.name(),
        session.messages.len()
    ));
    if session.messages.iter().any(|m| {
        m.blocks.iter().any(|b| matches!(b, Block::Reasoning { .. }))
    }) {
        report.notes.push(
            "reasoning preserved as summary text only (signatures/opaque payloads dropped)".into(),
        );
    }

    if opts.handshake {
        session.messages.insert(0, preamble(&session, source.name()));
        report.notes.push("injected resume preamble as first message".into());
    }

    let outcome = target.write(&session, opts)?;
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
