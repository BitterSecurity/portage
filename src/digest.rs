//! Handoff digest: turn a full session IR into a compact, action-preserving
//! briefing for continuing the work in another harness.
//!
//! Design (grounded in external research — see docs/superpowers/specs/):
//! - The target is a coding agent on the SAME filesystem, so files on disk are
//!   the durable state. We carry *paths + what was done*, never stale file
//!   contents (InfiAgent; Anthropic "reference files by path").
//! - Action-preserving: keep tool *calls* as one-liners, drop their bulky
//!   *outputs* (CoACT; observation masking — "The Complexity Trap").
//! - Deterministic extraction only; no re-summarization chains (CliffCompaction,
//!   Slipstream). LLM summary is a future opt-in, not the default.
//! - Drop reasoning (another model's chain-of-thought misleads the target).
//! - Keep the recent exchange verbatim (recency), as TEXT only to avoid
//!   orphaned tool_call/tool_result pairs breaking the target's resume.
//! - Redact obvious secrets (real sessions contain tokens).
//! - Let the target self-orient (Anthropic sub-agent handoff pattern).

use crate::ir::{Block, Message, Role, Session};

const MAX_GOAL: usize = 600;
const MAX_CMD: usize = 140;
const MAX_FILES: usize = 40;

/// Produce a trimmed session: one briefing message + the recent text tail.
pub fn build_handoff_session(session: &Session, source: &str, target: &str, tail: usize) -> Session {
    let mut out = Session::new(source);
    out.cwd = session.cwd.clone();
    out.created = session.created.clone();

    let brief = build_brief(session, source, target);
    out.messages.push(Message {
        role: Role::User,
        blocks: vec![Block::Text { text: brief }],
        timestamp: None,
    });

    // Recent tail: last `tail` text-bearing messages, text blocks only.
    let mut tail_msgs: Vec<Message> = Vec::new();
    for m in session.messages.iter().rev() {
        if tail_msgs.len() >= tail {
            break;
        }
        let texts: Vec<Block> = m
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { text } if !text.trim().is_empty() => {
                    Some(Block::Text { text: redact(text) })
                }
                _ => None,
            })
            .collect();
        if !texts.is_empty() {
            tail_msgs.push(Message { role: m.role, blocks: texts, timestamp: None });
        }
    }
    tail_msgs.reverse();
    out.messages.extend(tail_msgs);
    out
}

/// One-line summary for session discovery: the task + files touched + the
/// busiest tools. Lets a human or the calling agent judge a vague description
/// against a candidate without opening it.
pub fn quick_summary(session: &Session) -> String {
    let goal = extract_goal(session)
        .map(|g| g.lines().next().unwrap_or("").chars().take(90).collect::<String>())
        .filter(|g| !g.trim().is_empty())
        .unwrap_or_else(|| "(no explicit prompt)".into());
    let (files, _cmds, activity) = extract_actions(session);
    let mut parts = vec![goal];
    if !files.is_empty() {
        let names: Vec<String> = files
            .iter()
            .take(5)
            .map(|(p, _)| p.rsplit(['/', '\\']).next().unwrap_or(p).to_string())
            .collect();
        parts.push(format!("files: {}", names.join(", ")));
    }
    if !activity.is_empty() {
        let tools: Vec<String> =
            activity.iter().take(3).map(|(n, c)| format!("{n}×{c}")).collect();
        parts.push(format!("tools: {}", tools.join(", ")));
    }
    parts.join(" · ")
}

fn build_brief(session: &Session, source: &str, target: &str) -> String {
    let goal = extract_goal(session)
        .unwrap_or_else(|| "(no substantive user prompt found — see recent exchange)".into());

    let (files, commands, activity) = extract_actions(session);

    let mut s = String::new();
    s.push_str(&format!("# Session handoff (carried from {source} by portage)\n\n"));
    s.push_str("## Original task\n");
    s.push_str(&goal);
    s.push_str("\n\n");

    s.push_str(&format!("## Files touched ({})\n", files.len()));
    if files.is_empty() {
        s.push_str("(none detected)\n");
    } else {
        for (path, action) in files.iter().take(MAX_FILES) {
            s.push_str(&format!("- `{path}` — last op: {action}\n"));
        }
    }
    s.push('\n');

    s.push_str(&format!("## Commands run ({})\n", commands.len()));
    if commands.is_empty() {
        s.push_str("(none detected)\n");
    } else {
        const MAX_CMDS: usize = 25;
        if commands.len() > MAX_CMDS {
            s.push_str(&format!("(most recent {MAX_CMDS} of {})\n", commands.len()));
        }
        for (cmd, ok) in commands.iter().rev().take(MAX_CMDS).rev() {
            s.push_str(&format!("- {} `{}`\n", if *ok { "✓" } else { "✗" }, cmd));
        }
    }
    s.push('\n');

    if !activity.is_empty() {
        s.push_str("## Tool activity (all calls)\n");
        let shown: Vec<String> =
            activity.iter().take(12).map(|(n, c)| format!("{n}×{c}")).collect();
        s.push_str(&shown.join(", "));
        if activity.len() > 12 {
            s.push_str(&format!(", … (+{} more tool types)", activity.len() - 12));
        }
        s.push_str("\n\n");
    }

    s.push_str("## Your job\n");
    s.push_str(&format!(
        "You are continuing this session in {target}. The files above live on disk in this \
         workspace — re-read them as you need them rather than trusting any snapshot. Reasoning \
         and old tool output from the source were intentionally dropped. Read the recent exchange \
         below, briefly restate in your own words the current state and the single next concrete \
         step, then continue the work.\n"
    ));
    s
}

/// First user message that carries a real prompt (boilerplate context blocks
/// such as Codex's `<environment_context>` are stripped). Real sessions open
/// with injected XML context, so the naive "first user message" is wrong.
fn extract_goal(session: &Session) -> Option<String> {
    for m in &session.messages {
        if m.role != Role::User {
            continue;
        }
        let cleaned = strip_boilerplate(&Session::text_of(m));
        let c = cleaned.trim();
        if c.is_empty() || is_injected_doc(c) {
            continue; // environment context / AGENTS.md / instruction dumps
        }
        return Some(truncate(&redact(&cleaned), MAX_GOAL));
    }
    None
}

/// Leading user turns are often injected project docs, not the real prompt.
fn is_injected_doc(c: &str) -> bool {
    let head = c.lines().next().unwrap_or("").to_lowercase();
    head.starts_with("# agents") || (head.starts_with('#') && head.contains("instructions"))
}

/// Drop paragraphs that are wrapped tag blocks (`<environment_context>…`,
/// `<user_instructions>…`), keeping real prose.
fn strip_boilerplate(t: &str) -> String {
    t.split("\n\n")
        .filter(|p| !p.trim_start().starts_with('<'))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Returns (files: path -> last op, commands: (cmd, ok), activity: name×count
/// over ALL tool calls so nothing is hidden, sorted by count desc).
fn extract_actions(session: &Session) -> (Vec<(String, String)>, Vec<(String, bool)>, Vec<(String, usize)>) {
    use std::collections::BTreeMap;
    let mut errors: BTreeMap<String, bool> = BTreeMap::new();
    for m in &session.messages {
        for b in &m.blocks {
            if let Block::ToolResult { call_id, is_error, .. } = b {
                errors.insert(call_id.clone(), *is_error);
            }
        }
    }

    let mut files: Vec<(String, String)> = Vec::new();
    let mut commands: Vec<(String, bool)> = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();

    for m in &session.messages {
        for b in &m.blocks {
            if let Block::ToolCall { id, name, input } = b {
                *counts.entry(name.clone()).or_insert(0) += 1; // every call counted
                let lname = name.to_lowercase();
                if is_command_tool(&lname) {
                    if let Some(cmd) = command_of(input) {
                        let ok = !errors.get(id).copied().unwrap_or(false);
                        commands.push((truncate(&redact(&cmd), MAX_CMD), ok));
                    }
                }
                if is_file_tool(&lname) {
                    if let Some(path) = path_of(input) {
                        if let Some(e) = files.iter_mut().find(|(p, _)| *p == path) {
                            e.1 = name.clone();
                        } else {
                            files.push((path, name.clone()));
                        }
                    }
                }
            }
        }
    }

    let mut activity: Vec<(String, usize)> = counts.into_iter().collect();
    activity.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    (files, commands, activity)
}

fn is_command_tool(l: &str) -> bool {
    ["shell", "bash", "exec", "run", "command", "terminal"].iter().any(|k| l.contains(k))
}
fn is_file_tool(l: &str) -> bool {
    ["read", "write", "edit", "create", "patch", "view", "str_replace", "multiedit", "notebook"]
        .iter()
        .any(|k| l.contains(k))
}

fn command_of(input: &serde_json::Value) -> Option<String> {
    // Some tools (e.g. Codex `exec`) pass the command as a bare string.
    if let serde_json::Value::String(s) = input {
        return Some(s.clone());
    }
    for k in ["command", "cmd", "script", "input"] {
        if let Some(v) = input.get(k) {
            return Some(match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(a) => a
                    .iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                other => other.to_string(),
            });
        }
    }
    None
}

fn path_of(input: &serde_json::Value) -> Option<String> {
    for k in ["path", "file_path", "filePath", "file", "filename", "target_file", "notebook_path"] {
        if let Some(s) = input.get(k).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

/// Mask obvious secrets so a handoff brief is safe to carry/share.
pub fn redact(s: &str) -> String {
    s.split_inclusive(char::is_whitespace)
        .map(|tok| {
            let t = tok.trim();
            if looks_secret(t) {
                tok.replace(t, "[redacted]")
            } else {
                tok.to_string()
            }
        })
        .collect()
}

fn looks_secret(t: &str) -> bool {
    const PREFIXES: &[&str] =
        &["sk-", "ghp_", "gho_", "ghs_", "github_pat_", "xox", "AKIA", "AIza", "glpat-", "Bearer"];
    if t.len() >= 20 && PREFIXES.iter().any(|p| t.starts_with(p)) {
        return true;
    }
    // Long high-entropy-looking token.
    if t.len() >= 40 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        let digits = t.chars().filter(|c| c.is_ascii_digit()).count();
        let upper = t.chars().filter(|c| c.is_ascii_uppercase()).count();
        return digits >= 2 && upper >= 2;
    }
    false
}
