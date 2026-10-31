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

Early. Fully implemented and tested today:

| harness      | source | target |
|--------------|:------:|:------:|
| Claude Code  | ✅ | ✅ |
| Codex CLI    | ✅ | ✅ |
| Qwen Code    | ✅ | ✅ |
| Kimi Code    | ✅ | ✅ |
| Copilot CLI  | ✅ | — (read-only for now) |

More harnesses (Gemini/Antigravity, opencode, Goose, Hermes, OpenClaw —
the SQLite-backed ones) are specified in `docs/superpowers/specs/` and plug in as
`reader`/`writer`/`registrar` adapters. **On-disk formats are vendor-private and
reverse-engineered** — a converted session is verified by round-trip against
synthetic fixtures; always confirm a real resume against your installed version.

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

### From inside an agent (MCP)

```sh
portage mcp        # stdio MCP server exposing list / inspect / convert
```

Point your harness's MCP config at `portage mcp`, then ask the agent to
"import my last Codex session".

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
