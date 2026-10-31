//! Find a session in a harness by description or recency, so the user can say
//! "continue my codex session about asset/engagement structure" or "continue
//! the most recent codex session" instead of hunting for a file path.

use crate::harness::{Adapter, SessionRef};
use crate::ir::Session;
use anyhow::Result;
use std::time::SystemTime;

pub struct Hit {
    pub score: usize,
    pub terms_matched: usize,
    pub reference: SessionRef,
}

/// Rank sessions by how well their content/title/cwd match the query terms.
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
            // distinct terms dominate; occurrence count breaks ties.
            hits.push(Hit { score: terms_matched * 1_000_000 + total, terms_matched, reference: r });
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score));
    hits.truncate(limit);
    Ok(hits)
}

/// The single most recently modified session.
pub fn latest(adapter: &dyn Adapter) -> Result<Option<SessionRef>> {
    let mut timed: Vec<(Option<SystemTime>, SessionRef)> = adapter
        .list()?
        .into_iter()
        .map(|r| (mtime(&r), r))
        .collect();
    // None sorts last; Some(newest) first. If none are files, list order holds
    // (adapters that can't stat return their store's own newest-first order).
    if timed.iter().any(|(t, _)| t.is_some()) {
        timed.sort_by(|a, b| b.0.cmp(&a.0));
    }
    Ok(timed.into_iter().next().map(|(_, r)| r))
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
    // Content: read a bounded prefix when the locator is a path (the session's
    // opening describes the work), else go through the adapter (e.g. opencode
    // session id -> DB).
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
