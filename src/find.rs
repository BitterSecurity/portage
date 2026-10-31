//! Find a session in a harness by description or recency.
//!
//! Vague/paraphrased queries ("that session where I did the login page and the
//! password encryption") are handled by returning good candidates with a
//! one-line summary each, and letting the caller — usually the LLM agent that
//! invoked portage — do the semantic judgment. portage stays offline and
//! model-free; the model that is already there picks. Keyword miss falls back
//! to recent sessions so the user can still browse from memory.

use crate::harness::{Adapter, SessionRef};
use crate::ir::Session;
use anyhow::Result;
use std::time::SystemTime;

pub struct Hit {
    pub score: usize,
    pub terms_matched: usize,
    pub summary: Option<String>,
    pub reference: SessionRef,
}

/// Rank sessions by how well their content/title/cwd match the query terms,
/// then attach a one-line summary to each returned hit.
pub fn search(adapter: &dyn Adapter, query: &str, limit: usize) -> Result<Vec<Hit>> {
    let terms: Vec<String> =
        query.split_whitespace().map(|t| t.to_lowercase()).filter(|t| !t.is_empty()).collect();
    if terms.is_empty() {
        return Ok(vec![]);
    }
    let mut hits = Vec::new();
    for r in adapter.list()? {
        let hay = haystack(adapter, &r).to_lowercase();
        let mut terms_matched = 0;
        let mut total = 0;
        for t in &terms {
            let c = hay.matches(t.as_str()).count();
            if c > 0 {
                terms_matched += 1;
                total += c;
            }
        }
        if terms_matched > 0 {
            hits.push(Hit {
                score: terms_matched * 1_000_000 + total,
                terms_matched,
                summary: None,
                reference: r,
            });
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score));
    hits.truncate(limit);
    for h in &mut hits {
        h.summary = summarize(adapter, &h.reference);
    }
    Ok(hits)
}

/// The most recent sessions, summarized — the fallback when a query matches
/// nothing (the user half-remembers it) and the basis for `--latest`.
pub fn browse(adapter: &dyn Adapter, limit: usize) -> Result<Vec<Hit>> {
    let mut refs = adapter.list()?;
    refs.sort_by(|a, b| mtime(b).cmp(&mtime(a)));
    refs.truncate(limit);
    Ok(refs
        .into_iter()
        .map(|r| Hit { score: 0, terms_matched: 0, summary: summarize(adapter, &r), reference: r })
        .collect())
}

/// The single most recently modified session.
pub fn latest(adapter: &dyn Adapter) -> Result<Option<SessionRef>> {
    Ok(browse(adapter, 1)?.into_iter().next().map(|h| h.reference))
}

fn summarize(adapter: &dyn Adapter, r: &SessionRef) -> Option<String> {
    adapter.read(&r.locator).ok().map(|s| crate::digest::quick_summary(&s))
}

fn mtime(r: &SessionRef) -> Option<SystemTime> {
    std::fs::metadata(&r.locator).ok().and_then(|m| m.modified().ok())
}

fn haystack(adapter: &dyn Adapter, r: &SessionRef) -> String {
    let mut s = String::new();
    if let Some(t) = &r.title {
        s.push_str(t);
        s.push(' ');
    }
    if let Some(c) = &r.cwd {
        s.push_str(c);
        s.push(' ');
    }
    s.push_str(&r.locator);
    s.push(' ');
    // Content: bounded prefix when the locator is a path (the opening describes
    // the work), else via the adapter (e.g. opencode session id -> DB).
    if let Some(txt) = crate::harness::read_prefix(std::path::Path::new(&r.locator), 512 * 1024) {
        s.push_str(&txt);
    } else if let Ok(sess) = adapter.read(&r.locator) {
        for m in &sess.messages {
            s.push_str(&Session::text_of(m));
            s.push(' ');
        }
    }
    s
}
