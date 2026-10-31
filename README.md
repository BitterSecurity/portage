# portage

**Carry an AI coding-agent session from one harness to another — and resume it
natively on the other side.**

You hit a wall in one tool: a quota limit, a bug the other model debugs better,
a teammate on a different agent. Today you re-explain everything. `portage`
finds the session you mean, converts it, and writes it into the target tool's
own store so its `--resume` lists and continues it.

> The name is the act of carrying a canoe overland between two rivers.

```sh
# From Claude Code, continue a Codex session you described from memory:
portage find    --from codex "login page password encryption"   # ranked + summaries
portage convert --from codex --to claude-code --match "login encryption" --cwd "$PWD"
# → cd <cwd> && claude --resume <id>
```

Single static binary. Local-first — no network, no API keys, nothing leaves
your machine.

---

## Why it's not just a file copy

Two different problems, and portage handles both:

1. **Format** — every harness stores sessions in its own private, versioned,
   usually-SQLite-or-JSONL shape. The hard part is writing a session the target
   will actually *resume* (e.g. Claude Code needs a `uuid`/`parentUuid` chain
   and the right `encoded-cwd` directory; Codex needs a SQLite index row).
2. **Context** — blindly replaying hundreds of messages is the wrong way to
   *continue work*: it is bulky, carries stale tool output as if current, and
   injects another model's reasoning. portage's default `handoff` mode distills
   an **action-preserving briefing** (task · files touched · commands run ·
   recent exchange) and lets the target agent re-read the live files itself.

Both are backed by research and real-install validation — see
[`docs/superpowers/specs/`](docs/superpowers/specs/).

## Status

| harness      | source | target | verification |
|--------------|:------:|:------:|--------------|
| Claude Code  | ✅ | ✅ | **real** — read + `claude --resume` round-trip verified |
| Codex CLI    | ✅ | ✅ | **real** — read verified incl. function/custom tool calls, reasoning |
| opencode     | ✅ | —  | **real DB** — read verified on a 57k-part live session |
| Qwen Code    | ✅ | ✅ | synthetic round-trip |
| Kimi Code    | ✅ | ✅ | synthetic round-trip |
| Copilot CLI  | ✅ | —  | tolerant reader, unverified |

"real" = run against actual sessions on a live install, parsed block counts
matched ground truth, and (Codex→Claude Code) `claude --resume` loaded the
converted session and restated the task. On-disk formats are vendor-private and
reverse-engineered; confirm a real resume with `portage inspect` against your
installed version.

Not yet implemented (no local install to validate against, and speculative
writes into a live store risk corruption): Goose, Hermes, OpenClaw. Amp
(cloud-authoritative) and Aider (no session/resume) can only ever be read
sources. Adding a harness is one `reader` + `writer` (+ `registrar`).

## Install

```sh
cargo install --path .
```

## Usage

```sh
portage harnesses                      # what's supported, and read/write caps
portage list    --from codex           # sessions, newest first, with summaries
portage find    --from codex "<words>" # ranked candidates for a description
portage find    --from codex --latest  # the most recent session
portage inspect <path>   --from codex  # parsed structure of one session
portage convert <path>   --from codex --to claude-code --cwd "$PWD"
```

### Finding a session (even vaguely)

You rarely remember the file path. Describe the session instead — `find` reads
each candidate and prints a one-line summary (task · files · tools), ranked by
match. You don't need exact keywords; a miss falls back to recent sessions to
browse. Then convert by description, or `--latest`:

```sh
portage convert --from codex --to claude-code --match "login encryption" --cwd "$PWD"
portage convert --from codex --to claude-code --latest --cwd "$PWD"
```

`--cwd` matters for Claude Code: resume only finds the session when the
encoded-cwd directory matches where you launch `claude`.

### Modes

- `--mode handoff` (default) — compact action-preserving briefing + recent
  turns. Reasoning and stale tool output dropped; secrets redacted. Best for
  *continuing work*.
- `--mode replay` — faithful 1:1 of every message, for archival/debugging.

## Thin wrappers: MCP + skill

The CLI is the whole product; these make it one step from inside an agent.

**MCP** — `portage mcp` is a stdio MCP server exposing `find`, `inspect`, and
`convert`. Register it with your harness:

```json
{ "mcpServers": { "portage": { "command": "portage", "args": ["mcp"] } } }
```

Then just say *"continue my Codex session about the login encryption work"* —
the agent runs `find` (doing the fuzzy, cross-language matching itself from the
summaries), picks the session, and `convert`s it. portage stays model-free; the
model that is already there does the semantic judgment.

**Skill** — copy [`skill/portage/SKILL.md`](skill/portage/SKILL.md) into your
harness's skills directory (e.g. `~/.claude/skills/portage/`) to teach the
find → convert flow.

## Platforms

Home-relative stores (Codex, Claude Code, Qwen, Kimi) resolve the same on Linux,
WSL, macOS, and Windows via the OS home dir. opencode uses the OS data dir
(`~/.local/share`, `~/Library/Application Support`, or `%APPDATA%`); set
`PORTAGE_OPENCODE_DB` to override. Verified on WSL/Linux; macOS and
native-Windows paths resolve per-OS but are not yet confirmed on-platform.

## What doesn't transfer

Reported, never silently dropped: per-turn model switches, sandbox posture,
thread branching (flattened), reasoning (summary text only; dropped entirely in
handoff). Open tool calls and queued input are carried into the resume brief.

## Development

```sh
cargo test        # synthetic fixtures + a real-schema opencode read + find ranking
```

Design and research notes live in
[`docs/superpowers/specs/`](docs/superpowers/specs/).

## License

MIT OR Apache-2.0.
