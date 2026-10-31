---
name: portage
description: Use when the user wants to continue/resume an AI coding-agent session from one harness in another — e.g. "import my last Codex session into Claude Code", "move this session to codex", "carry my session over". Converts and registers the session so the target's native --resume lists it.
---

# portage

`portage` moves a coding-agent session between harnesses and makes it resumable
in the target. Supported today: Claude Code and Codex (both directions),
Copilot CLI (read-only source).

## Steps

1. **Find the source session** if the user did not give a path:
   `portage list --from <harness>`
2. **Convert** into the target:
   `portage convert <path> --from <src> --to <dst> --cwd "$PWD"`
   - `--cwd` is required for `--to claude-code` and should be the directory the
     user will launch `claude` from, or resume won't find it.
   - Add `--no-handshake` to skip the injected resume-preamble message.
3. **Report** the printed resume command to the user verbatim (e.g.
   `cd <cwd> && claude --resume <id>`), plus any `•` notes about what did not
   transfer.

## Notes

- On-disk formats are reverse-engineered and version-specific. If `convert`
  prints a note that the index/schema was not recognized, tell the user to
  verify against their installed harness version before relying on resume.
- Never fabricate a resume id; use exactly what `portage` printed.
