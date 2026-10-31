# portage

Carry an AI coding-agent session from one harness to another and **resume it
natively** on the other side. Hit a quota, want a different model, or a teammate
on a different tool? Move the live session instead of re-explaining it.

> The name is the act of carrying a canoe overland between two rivers.

## What it does

`portage` reads a session from a **source** harness, converts it through a
common intermediate representation (IR), and writes it into a **target**
harness's store so the target's own `--resume` picker lists and continues it.

```
source store ──reader──▶ IR ──writer──▶ target store ──▶ target --resume
                                        + registrar (SQLite index)
```

## Status

| harness      | source | target | verification |
|--------------|:------:|:------:|--------------|
| Claude Code  | ✅ | ✅ | **real files** (read validated against 292 live sessions) |
| Codex CLI    | ✅ | ✅ | **real files** (read validated incl. function/custom tool calls, reasoning) |
| opencode     | ✅ | — | **real DB** (read validated against a 57k-part live session) |
| Qwen Code    | ✅ | ✅ | synthetic round-trip only |
| Kimi Code    | ✅ | ✅ | synthetic round-trip only |
| Copilot CLI  | ✅ | — | tolerant reader, unverified |

**Verification levels.** "real files/DB" = the reader was run against real
sessions on a live install and its parsed block counts matched ground truth.
"synthetic" = internally consistent round-trip against fixtures in the expected
format, but not yet confirmed against a real install. On-disk formats are
vendor-private and reverse-engineered — always confirm a real resume with
`portage inspect` against your installed version.

**Not yet implemented** (no reliable way to verify without the tool present, and
writing speculative rows into a live store risks corruption): Goose, Hermes,
OpenClaw (empty stores locally). Antigravity's local form is task markdown +
media, not a turn transcript, so it is not a clean source. These are specified
in `docs/superpowers/specs/` and plug in as `reader`/`writer`/`registrar`
adapters once a real install is available to validate against.

Writing into opencode is deliberately omitted: its event-sourced multi-table
store (session/message/part + project linkage) is read as a source only.

## Install

```sh
cargo install --path .
```

Single static binary, no runtime dependency.

## Use

```sh
portage harnesses                      # what's supported
portage list --from codex              # find sessions
portage inspect <path> --from codex    # see parsed structure
portage convert <path> --from codex --to claude-code --cwd "$PWD"
# → prints:  cd <cwd> && claude --resume <id>
```

`--cwd` matters for Claude Code: resume only finds the session when the
encoded-cwd directory matches where you launch `claude`.

### Modes

- `--mode handoff` (default): a compact, action-preserving briefing (task,
  files touched, commands run, tool activity) + the recent exchange, then asks
  the target to self-orient. Reasoning and stale tool output are dropped;
  secrets are redacted. Best for *continuing work* in another tool.
- `--mode replay`: faithful 1:1 of every message, for archival/debugging.

### Platforms

Home-relative stores (Codex, Claude Code, Qwen, Kimi) resolve the same on
Linux, WSL, macOS, and Windows via the OS home dir. opencode's store uses the
OS data dir (`~/.local/share`, `~/Library/Application Support`, or `%APPDATA%`);
set `PORTAGE_OPENCODE_DB` to override. Verified on WSL/Linux; macOS and
native-Windows paths are resolved per-OS but not yet confirmed on-platform.

### Finding a session (even vaguely)

```sh
portage find --from codex "login page password encryption"   # ranked + summaries
portage find --from codex --latest                            # most recent
```

`find` reads each candidate's content and prints a one-line summary (task ·
files · tools). You don't need exact keywords — a miss falls back to recent
sessions to browse. Then convert by description:

```sh
portage convert --from codex --to claude-code --match "login encryption" --cwd "$PWD"
portage convert --from codex --to claude-code --latest --cwd "$PWD"
```

### Thin wrappers: MCP + skill

Everything above is the CLI. Two wrappers make it one step from inside a harness:

**MCP** — `portage mcp` is a stdio MCP server exposing `find`, `inspect`, and
`convert`. Point your harness's MCP config at it:

```json
{ "mcpServers": { "portage": { "command": "portage", "args": ["mcp"] } } }
```

Then, from inside harness A, just say *"continue my Codex session about the
login encryption work"* — the agent runs `find` (doing the fuzzy matching
itself), picks the session, and `convert`s it.

**Skill** — `skill/portage/SKILL.md` teaches an agent the find → convert flow.
Copy it into your harness's skills directory (e.g. `~/.claude/skills/portage/`).

## What does not transfer

Reported, never silently dropped (see `ConversionReport`): per-turn model
switches, sandbox posture, thread branching (flattened), reasoning (kept as
summary text only). Open tool calls and queued input are carried into an
injected resume preamble.

Cloud-authoritative (Amp) and resume-less (Aider) harnesses can only ever be
read sources, never resume targets.

## Design

`docs/superpowers/specs/2026-10-31-portage-design.md`.

## License

MIT OR Apache-2.0.
