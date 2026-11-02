//! Workspace (git worktree) context: a snapshot of the working tree the source
//! session operated in, captured at convert time. The target agent usually runs
//! on the same filesystem, so what carries over is orientation — which branch,
//! what is uncommitted, where HEAD is — never stale file contents (see
//! `digest.rs` for that design rule).

use std::path::Path;
use std::process::Command;

pub struct Workspace {
    pub origin: Option<String>,
    pub dir: String,
    pub branch: String,
    pub head: String,
    pub dirty: Vec<(char, String)>,
    pub commits: Vec<String>,
}

fn git(cwd: &str, args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(["-C", cwd])
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        // trim_end only: `status --porcelain` lines BEGIN with a status column
        // (a leading space is meaningful — " M" = modified in worktree).
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
}

/// Collect git state for `cwd`; `None` when it is not inside a work tree (or
/// git is unavailable / the directory is gone). Every probe is best-effort —
/// a missing piece degrades the section, never the conversion.
pub fn collect(cwd: &str) -> Option<Workspace> {
    if !Path::new(cwd).exists() {
        return None;
    }
    if git(cwd, &["rev-parse", "--is-inside-work-tree"])?.trim() != "true" {
        return None;
    }
    let dir = Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string());
    let origin = git(cwd, &["remote", "get-url", "origin"])
        .filter(|s| !s.is_empty())
        .map(strip_credentials);
    Some(Workspace {
        origin,
        dir,
        branch: git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default(),
        head: git(cwd, &["rev-parse", "--short", "HEAD"]).unwrap_or_default(),
        dirty: git(cwd, &["status", "--porcelain"])
            .map(|out| {
                out.lines()
                    .filter(|l| l.len() > 3)
                    .map(|l| {
                        let b = l.as_bytes();
                        let st = if b[0] != b' ' { b[0] as char } else { b[1] as char };
                        (st, l[3..].trim().to_string())
                    })
                    .collect()
            })
            .unwrap_or_default(),
        commits: git(cwd, &["log", "--max-count=5", "--pretty=%h %s"])
            .map(|out| out.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect())
            .unwrap_or_default(),
    })
}

/// Markdown section for the handoff brief / resume preamble. `None` outside a
/// git work tree.
pub fn section(cwd: &str) -> Option<String> {
    let w = collect(cwd)?;
    let mut s = String::from("## Workspace snapshot (captured at handoff)\n");
    let repo = w.origin.clone().unwrap_or_else(|| w.dir.clone());
    s.push_str(&format!("- repo: {repo}\n"));
    s.push_str(&format!("- branch: {} @ {}\n", w.branch, w.head));
    if w.dirty.is_empty() {
        s.push_str("- working tree: clean\n");
    } else {
        s.push_str(&format!(
            "- working tree: {} uncommitted file(s)\n",
            w.dirty.len()
        ));
        const MAX_DIRTY: usize = 8;
        let shown: Vec<String> = w
            .dirty
            .iter()
            .take(MAX_DIRTY)
            .map(|(st, p)| format!("{st} `{p}`"))
            .collect();
        s.push_str(&format!("  - {}\n", shown.join(", ")));
        if w.dirty.len() > MAX_DIRTY {
            s.push_str(&format!("  - … (+{} more)\n", w.dirty.len() - MAX_DIRTY));
        }
    }
    if !w.commits.is_empty() {
        s.push_str("- recent commits:\n");
        for c in &w.commits {
            s.push_str(&format!("  - {c}\n"));
        }
    }
    Some(s)
}

/// `https://user:token@host/path` -> `https://host/path` — remote URLs must be
/// safe to embed in a handoff brief.
fn strip_credentials(url: String) -> String {
    match (url.find("://"), url.find('@')) {
        (Some(i), Some(a)) if a > i => format!("{}{}", &url[..i + 3], &url[a + 1..]),
        _ => url,
    }
}
