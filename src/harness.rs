//! Harness identity and the adapter contract.
//!
//! Adding a harness = implement `Adapter` and register it in `adapters::all()`.
//! A reader turns a harness store into IR; a writer turns IR into the harness
//! store *and* registers it so the harness's own `--resume` lists it.

use crate::ir::Session;
use anyhow::Result;
use std::path::PathBuf;

/// A reference to a session found in a harness store, as `list` returns it.
#[derive(Debug, Clone)]
pub struct SessionRef {
    /// Locator passed back to `read` (path, id, or harness-specific key).
    pub locator: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub modified: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WriteOpts {
    /// Inject a resume-preamble first message describing what did not transfer
    /// and carrying pending state forward.
    pub handshake: bool,
    /// Override the cwd the session is registered under (Claude Code needs the
    /// encoded-cwd dir to match where the user launches `claude`).
    pub cwd_override: Option<String>,
    /// Write even if the target's store schema version is not in the verified set.
    pub force: bool,
}

impl Default for WriteOpts {
    fn default() -> Self {
        WriteOpts { handshake: true, cwd_override: None, force: false }
    }
}

/// What the target produced and how to resume it there.
#[derive(Debug, Clone)]
pub struct WriteOutcome {
    /// Files/rows written (for reporting / undo).
    pub artifacts: Vec<PathBuf>,
    /// The id the target will list this session under.
    pub target_session_id: String,
    /// Exact command the user runs to resume in the target.
    pub resume_hint: String,
}

pub trait Adapter: Sync {
    fn name(&self) -> &'static str;
    fn can_read(&self) -> bool;
    fn can_write(&self) -> bool;

    /// Locate sessions in this harness's store. Read-capable adapters implement
    /// this; others may return an empty list.
    fn list(&self) -> Result<Vec<SessionRef>>;

    /// Parse one session into IR.
    fn read(&self, locator: &str) -> Result<Session>;

    /// Render + register a session into this harness. Must fail loudly (not
    /// corrupt the store) when it cannot guarantee the session is resumable.
    fn write(&self, session: &Session, opts: &WriteOpts) -> Result<WriteOutcome>;
}
