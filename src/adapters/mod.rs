//! Adapter registry. Add a harness: implement `Adapter`, add a module here,
//! and a match arm in `by_name_with_root` + a row in `SUPPORTED`.

pub mod claude_code;
pub mod codex;
pub mod copilot;

use crate::harness::Adapter;
use anyhow::{anyhow, Result};
use std::path::PathBuf;

/// (canonical name, can_read, can_write)
pub const SUPPORTED: &[(&str, bool, bool)] = &[
    ("claude-code", true, true),
    ("codex", true, true),
    ("copilot", true, false),
];

pub fn by_name(name: &str) -> Result<Box<dyn Adapter>> {
    by_name_with_root(name, None)
}

/// `root` overrides the home directory (used by tests with a temp dir).
pub fn by_name_with_root(name: &str, root: Option<PathBuf>) -> Result<Box<dyn Adapter>> {
    Ok(match name {
        "claude-code" | "claude" | "cc" => match root {
            Some(p) => Box::new(claude_code::ClaudeCode::with_root(p)),
            None => Box::new(claude_code::ClaudeCode::new()),
        },
        "codex" => match root {
            Some(p) => Box::new(codex::Codex::with_root(p)),
            None => Box::new(codex::Codex::new()),
        },
        "copilot" | "copilot-cli" => Box::new(copilot::Copilot::new_opt(root)),
        other => return Err(anyhow!("unknown or unsupported harness: '{other}' (see `portage list`)")),
    })
}
