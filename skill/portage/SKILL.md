---
name: portage
description: Use when the user wants to continue/resume work from a DIFFERENT coding agent's session in the current one — e.g. "continue my Codex session about the login page and password encryption", "pick up that opencode session from yesterday", "resume my most recent codex work here". Finds the session in the other harness (even from a vague description) and converts it so the current tool can resume it.
---

# portage

`portage` carries a coding-agent session from another harness into the current
one and makes it resumable here. It is a single binary (`portage`); this skill
is a thin wrapper over it.

Supported sources/targets: Claude Code, Codex, Qwen, Kimi, opencode
(read+write); Copilot (read-only source). opencode-compatible forks that keep
their own data dir work with `PORTAGE_OPENCODE_DB=<path to their db>`.

## Flow

1. **Find the session the user means.** The user may describe it vaguely or from
   memory. YOU do the semantic matching — do not expect exact keywords.
   - Extract candidate keywords, including synonyms and both English and the
     user's own language (e.g. login / auth / 암호화 / encryption / bcrypt).
   - `portage find --from <source> "<keywords>"` — ranked candidates in TOON
     (tab-delimited tables; `[N]` = row count, `{fields}` = columns). Each hit
     carries a one-line summary (task · files · tools). Run it more than once
     with different keywords if unsure. Output looks like:

     ```
     hits[2	]{terms	modified	title	cwd	summary	locator}:
       2	2026-11-02T10:37:00Z	login page	/home/u/proj	fix login password…	/home/u/.claude/projects/…/a.jsonl
     ```

   - `portage find --from <source> --latest` for "the most recent one".
   - A keyword miss falls back to recent sessions to browse (stderr notes it).
   - Read the summaries and pick the best fit. If genuinely ambiguous, show the
     user the top 2–3 summaries and ask which.

2. **Convert into the current harness.**
   `portage convert --from <source> --to <current> --match "<keywords>" --cwd "$PWD"`
   (or `--session <path>` once you know it, or `--latest`).
   - `--cwd` matters: it registers the session under the directory the user
     will launch the target from (required for Claude Code resume), and drives
     the workspace snapshot (repo, branch, uncommitted files) embedded in the
     handoff brief.
   - Default `--mode handoff` carries a compact briefing + recent turns (best
     for continuing work). `--mode replay` is a full 1:1 copy.

3. **Report the printed `resume:` line verbatim** (e.g.
   `cd <cwd> && claude --resume <id>`, in TOON it is the `resume:` field), plus
   any `•` conversion notes. Never invent a resume id.

## Notes

- Reasoning and stale tool output are dropped; the files are on disk, so the
  resumed agent re-reads them. Secrets are redacted from the briefing.
- On-disk formats are reverse-engineered. If `convert` warns the target schema
  was unrecognized, tell the user to verify before relying on resume.
