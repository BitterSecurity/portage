# portage — universal agent-session converter

**Status:** design (pending approval)
**Date:** 2026-10-31
**Owner:** catower917@gmail.com

---

## 1. Intent & why

AI coding harnesses (Claude Code, Codex, Gemini/Antigravity, Copilot CLI,
opencode, Goose, Qwen, Kimi, Hermes, OpenClaw, …) each store conversation
sessions in their own private on-disk format. When you hit a wall in one
tool — a quota limit, a bug the other model debugs better, a teammate on a
different harness — there is no supported way to carry the live session
across and *resume* it on the other side. You re-explain everything.

`portage` is a CLI that reads a session from one harness, converts it, and
writes it into another harness's store **so that the target's own `--resume`
picker lists it and continues it natively**. The name is the act of carrying
a canoe overland between two rivers.

The owner's own daily need is narrow: **Codex → Claude Code**. That one path
is the dogfood target and must work end to end first. The project ships as
open source with a broader set of harnesses so others get the same escape
hatch.

### Why now
The whole ecosystem is mid-migration from flat JSONL transcripts to SQLite
session stores (Codex index, Hermes, OpenClaw, Goose, opencode, Crush,
Cursor). Readers/locators already exist across many OSS tools, but only one
tool (`agent-session-bridge`, 3 harnesses) actually makes a converted session
*resumable* in the target, and it is Python. There is room for a single-binary
Rust tool that does the hard part (resume registration) for more harnesses.

## 2. Goals / non-goals / anti-goals

### Goals
- Convert a session from a **source** harness into a **target** harness such
  that the target lists and resumes it natively.
- Preserve the conversation core losslessly where the IR allows: user/assistant
  text, reasoning summaries, tool calls, tool results, and call↔result linkage.
- Ship as a **single static binary** (Rust), plus thin **MCP server** and
  **skill** wrappers for one-command use from inside a harness.
- Architecture that makes adding a harness = one `reader` + one `writer`
  (+ `registrar` for SQLite-backed stores), not N pairwise converters.
- First release covers 10 harnesses (§4).

### Non-goals (not now, maybe later)
- Live/continuous background mirroring (a daemon). v1 is one-shot `convert`.
- A GUI / web viewer. (Replay/view is already well served by other OSS.)
- Preserving harness-specific state that has no cross-harness meaning
  (token accounting, sandbox posture, model-per-turn, thread branching).
- Writing *into* cloud-authoritative or resume-less harnesses (see anti-goals).

### Anti-goals (would make this a failure even if it "works")
- A converted session that silently drops user/assistant turns or mis-links a
  tool result to the wrong call. Correctness of the transcript beats coverage.
- A "resume" that writes a file the target never actually lists. If it does not
  show up in the target's native picker, it is not done.
- Requiring a Python/Node runtime to install (defeats the single-binary promise).
- Claiming "universal" as a feature when a harness is untested. "Universal" is
  the *architecture*; supported harnesses are an explicit, versioned list.

## 3. Architecture — IR hub

```
source store ──reader──▶  IR  ──writer──▶ target store
                          │                   │
                          │              registrar (SQLite index/DB)
                       (hub)                   ▼
                                       target --resume lists it
```

- **IR (intermediate representation):** the union of what supported harnesses
  can express — threaded messages with typed content blocks
  (`text` / `reasoning` / `tool_call` / `tool_result`), session metadata,
  tool schemas, and explicit *pending* state (open tool calls, queued input).
- **reader:** parse one harness's store → IR.
- **writer:** render IR → one harness's on-disk format.
- **registrar:** register the written session into the target's SQLite
  index/DB so its resume picker shows it. This is the genuinely hard,
  fragile part and the project's main value. A registrar **refuses to write
  if the target's schema version is unrecognized** rather than corrupt it.
- N adapters ⇒ N×(N−1) directed pairs for free via the hub.

### Lossy transfers → resume handshake
Items that cannot cross losslessly (per-turn model switches, sandbox posture,
thread branches flattened, reasoning kept as summary text only, queued input)
are not silently dropped. The converter emits a **ConversionReport** of
asymmetries and injects a short **resume preamble** as the first message of the
resumed session, carrying forward open tool calls / queued input and a note of
what did not transfer.

### Verification is part of "done"
For each supported target, a converted session must be shown to actually appear
in that harness's native resume path (live recall of a user + assistant
sentinel). Formats are reverse-engineered and vendor-private, so the design
pins **tested harness versions** and treats newer versions as unverified until
checked via `portage inspect`.

## 4. Supported harnesses (first release)

Classification: **S** = usable as source (read), **T** = usable as target
(write + resume registration).

| Harness | Store | Path | Resume | S | T |
|---|---|---|---|:--:|:--:|
| Claude Code | JSONL | `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl` | `claude --resume` | ✅ | ✅ |
| Codex CLI | JSONL + SQLite index | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` + `state_5.sqlite` | `codex resume` | ✅ | ✅ |
| Antigravity (`agy`) | JSONL | `~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/transcript.jsonl` (+ `conversation_summaries.db`) | `agy --conversation <id>` | ✅ | ✅ |
| Gemini CLI (legacy) | JSON | `~/.gemini/tmp/<hash>/chats/session-*.json` | `gemini --resume` | ✅ | ⛔ (retiring) |
| Copilot CLI | JSONL (+ index) | `~/.copilot/session-state/<id>/events.jsonl` | `copilot --resume <id>` | ✅ | ✅ |
| opencode | SQLite | `~/.local/share/opencode/opencode.db` (session→message→part) | `opencode --session <id>` | ✅ | ✅ |
| Goose | SQLite (1.10+) | `~/.local/share/goose/sessions/sessions.db` | `goose session --resume --session-id <id>` | ✅ | ✅ |
| Qwen Code | JSON | `~/.qwen/sessions/<hash>/<id>.json` | `qwen --resume <id>` | ✅ | ✅ |
| Kimi Code | JSONL | `~/.kimi-code/sessions/` (cwd-grouped) + wire log | `kimi --session <id>` | ✅ | ✅ |
| Hermes | SQLite | `~/.hermes/state.db` (legacy jsonl) | `hermes --resume` | ✅ | ✅ |
| OpenClaw | SQLite | `~/.openclaw/agents/<id>/agent/openclaw-agent.sqlite` | native | ✅ | ✅ |

**Harness-specific gotchas the builder must handle:**
- **Claude Code:** the `<encoded-cwd>` directory name must match the dir the
  user launches `claude` from, or resume won't find it.
- **Codex:** dropping the `rollout-*.jsonl` is not enough; must index into
  `state_5.sqlite` for the resume picker.
- **Hermes / OpenClaw / Goose / opencode:** SQLite-only in current builds;
  writing a file is meaningless — must register DB rows.
- **Antigravity:** map conversation to workspace via `conversation_summaries.db`.

**Deferred (post-v1), read-only sources only:**
- **Amp (Sourcegraph):** threads are cloud-authoritative (ampcode.com); local
  JSON is a legacy cache. Cannot write a resumable thread offline.
- **Aider:** Markdown history only (`.aider.chat.history.md`), no session-id or
  resume concept — can be read as a source, never a target.

## 5. Decisions already made (owner-confirmed unless marked "builder call")

- **Build fresh**, not a fork of `agent-session-bridge`. Its IR/reader/writer/
  registrar shape is the reference; code is not reused.
- **Language: Rust.** *Not for runtime speed* (sessions are KB–MB, one-shot —
  irrelevant). For (a) single static binary ⇒ trivial skill install, no runtime
  dep; (b) the best existing reference readers (`terraphim_sessions`, `lore-cli`)
  are Rust; (c) mature SQLite (`rusqlite`) and official MCP (`rmcp`) crates.
- **CLI first**, with MCP server and skill as thin wrappers over the same core.
- **Delivery:** single binary; install via skill; expose via MCP so an agent
  inside a harness can call "import my last Codex session."
- *(builder call)* **Gemini:** target **Antigravity** (current); read legacy
  Gemini JSON as a source only.
- *(builder call)* **Binary name:** `portage` (consider `port` alias).

## 6. CLI surface (initial, builder may refine)

- `portage list [--from <harness>]` — list sessions found in a harness's store.
- `portage inspect <session>` — show parsed structure + detected harness/version
  (and whether the version is in the verified set).
- `portage convert <session> --from <h> --to <h> [--no-handshake] [--force]` —
  convert + register into the target.
- `portage mcp` — run the MCP server exposing convert/list/inspect as tools.
- `portage install-skill` — install the portage skill into detected harnesses.

## 7. Constraints
- Session files may contain secrets → test fixtures are synthetic; never ship
  real transcripts in the repo.
- Vendor formats are private and drift between versions → pin verified versions,
  refuse on unknown SQLite schema, fail loudly not silently.
- Local-first: no network required for any supported (non-Amp) pair.

## 8. What's left to the builder
- Exact IR Rust types and the per-harness reader/writer/registrar impls.
- Which SQLite crate feature set / bundling; schema-version detection strategy.
- MCP tool schemas and the skill package contents.
- Test strategy: synthetic fixtures per harness + a live round-trip check that a
  converted session appears in the target's resume picker (the "done" bar).
- Implementation **sequencing** (plan phase): Codex→CC end-to-end first (dogfood),
  then CC→Codex, then remaining adapters.

## 9. Open questions for the owner (to resolve before plan)
1. **License** — MIT, Apache-2.0, or dual MIT/Apache-2.0 (Rust-ecosystem norm)?
2. **v1 completeness bar** — does first release require all 10 harnesses fully
   bidirectional-and-resume-verified, or is "all 10 readable + Codex↔CC
   resume-verified + others best-effort writer" an acceptable first tag?
3. **MCP + skill at launch**, or CLI-first release with those as a fast follow?
