//! Synthetic round-trip + conversion tests. These prove the IR pipeline and the
//! CC/Codex mappings are internally consistent against fixtures in the format the
//! adapters expect. They do NOT prove a converted session resumes in a real
//! install — that bar requires the actual harness (see design §3).

use portage::adapters;
use portage::convert::{self, ConvertOpts, Mode};
use portage::harness::WriteOpts;
use portage::ir::{Block, Message, Role, Session};

fn replay(cwd: &str) -> ConvertOpts {
    ConvertOpts {
        mode: Mode::Replay,
        tail: 8,
        write: WriteOpts { handshake: true, cwd_override: Some(cwd.into()), force: false },
    }
}
use serde_json::json;
use std::collections::BTreeMap;

fn sample() -> Session {
    let mut s = Session::new("test");
    s.cwd = Some("/home/u/proj".to_string());
    s.messages = vec![
        Message { role: Role::User, blocks: vec![Block::Text { text: "hello".into() }], timestamp: None },
        Message {
            role: Role::Assistant,
            blocks: vec![
                Block::Reasoning { text: "thinking".into() },
                Block::Text { text: "hi there".into() },
                Block::ToolCall { id: "c1".into(), name: "read".into(), input: json!({"path": "a.txt"}) },
            ],
            timestamp: None,
        },
        Message {
            role: Role::User,
            blocks: vec![Block::ToolResult { call_id: "c1".into(), output: json!("file body"), is_error: false }],
            timestamp: None,
        },
    ];
    s
}

fn block_kinds(s: &Session) -> BTreeMap<&'static str, usize> {
    let mut m = BTreeMap::new();
    for msg in &s.messages {
        for b in &msg.blocks {
            let k = match b {
                Block::Text { .. } => "text",
                Block::Reasoning { .. } => "reasoning",
                Block::ToolCall { .. } => "call",
                Block::ToolResult { .. } => "result",
            };
            *m.entry(k).or_insert(0) += 1;
        }
    }
    m
}

#[test]
fn claude_code_roundtrip_preserves_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let cc = adapters::by_name_with_root("claude-code", Some(dir.path().to_path_buf())).unwrap();
    let out = cc.write(&sample(), &WriteOpts::default()).unwrap();
    let path = out.artifacts[0].to_string_lossy().into_owned();
    let back = cc.read(&path).unwrap();

    assert_eq!(back.messages.len(), 3, "CC keeps one line per message");
    assert_eq!(block_kinds(&back), block_kinds(&sample()));
    assert_eq!(back.cwd.as_deref(), Some("/home/u/proj"));
}

#[test]
fn codex_roundtrip_is_stable() {
    let dir = tempfile::tempdir().unwrap();
    let codex = adapters::by_name_with_root("codex", Some(dir.path().to_path_buf())).unwrap();
    let out1 = codex.write(&sample(), &WriteOpts::default()).unwrap();
    let s2 = codex.read(&out1.artifacts[0].to_string_lossy()).unwrap();
    // Codex flattens blocks into response_items, so each block becomes a message.
    assert_eq!(block_kinds(&s2), block_kinds(&sample()));

    // Re-writing and re-reading must be stable.
    let out2 = codex.write(&s2, &WriteOpts::default()).unwrap();
    let s3 = codex.read(&out2.artifacts[0].to_string_lossy()).unwrap();
    assert_eq!(block_kinds(&s2), block_kinds(&s3));
}

#[test]
fn convert_codex_to_claude_code() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    // Seed a Codex session on disk.
    let codex = adapters::by_name_with_root("codex", Some(root.clone())).unwrap();
    let seeded = codex.write(&sample(), &WriteOpts::default()).unwrap();

    // Convert Codex -> Claude Code.
    let src = adapters::by_name_with_root("codex", Some(root.clone())).unwrap();
    let dst = adapters::by_name_with_root("claude-code", Some(root.clone())).unwrap();
    let (outcome, report) = convert::run(
        src.as_ref(),
        dst.as_ref(),
        &seeded.artifacts[0].to_string_lossy(),
        &replay("/home/u/proj"),
    )
    .unwrap();

    assert!(outcome.artifacts[0].exists());
    assert!(report.notes.iter().any(|n| n.contains("preamble")));
    assert!(outcome.resume_hint.contains("claude --resume"));

    // The converted CC session should carry the preamble + original blocks.
    let cc = adapters::by_name_with_root("claude-code", Some(root)).unwrap();
    let back = cc.read(&outcome.artifacts[0].to_string_lossy()).unwrap();
    let kinds = block_kinds(&back);
    assert_eq!(kinds.get("text"), Some(&3)); // hello + hi there + preamble
    assert_eq!(kinds.get("call"), Some(&1));
    assert_eq!(kinds.get("result"), Some(&1));
}

fn roundtrip_preserves_blocks(harness: &str) {
    let dir = tempfile::tempdir().unwrap();
    let a = adapters::by_name_with_root(harness, Some(dir.path().to_path_buf())).unwrap();
    let opts = WriteOpts { handshake: false, cwd_override: Some("/home/u/proj".into()), force: false };
    let out = a.write(&sample(), &opts).unwrap();
    let back = a.read(&out.artifacts[0].to_string_lossy()).unwrap();
    assert_eq!(block_kinds(&back), block_kinds(&sample()), "{harness} must preserve blocks");
}

#[test]
fn qwen_roundtrip_preserves_blocks() {
    roundtrip_preserves_blocks("qwen");
}

#[test]
fn kimi_roundtrip_preserves_blocks() {
    roundtrip_preserves_blocks("kimi");
}

#[test]
fn convert_codex_to_qwen_and_kimi() {
    for target in ["qwen", "kimi"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let codex = adapters::by_name_with_root("codex", Some(root.clone())).unwrap();
        let seeded = codex.write(&sample(), &WriteOpts::default()).unwrap();

        let src = adapters::by_name_with_root("codex", Some(root.clone())).unwrap();
        let dst = adapters::by_name_with_root(target, Some(root.clone())).unwrap();
        let (outcome, _) = convert::run(
            src.as_ref(),
            dst.as_ref(),
            &seeded.artifacts[0].to_string_lossy(),
            &replay("/home/u/proj"),
        )
        .unwrap();
        assert!(outcome.artifacts[0].exists());
        let back = dst.read(&outcome.artifacts[0].to_string_lossy()).unwrap();
        // original 5 blocks + 1 preamble text block.
        assert_eq!(block_kinds(&back).get("text"), Some(&3), "{target}");
        assert_eq!(block_kinds(&back).get("call"), Some(&1), "{target}");
        assert_eq!(block_kinds(&back).get("result"), Some(&1), "{target}");
    }
}

#[test]
fn handoff_mode_compresses_and_drops_reasoning() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let codex = adapters::by_name_with_root("codex", Some(root.clone())).unwrap();
    let seeded = codex.write(&sample(), &WriteOpts::default()).unwrap();

    let src = adapters::by_name_with_root("codex", Some(root.clone())).unwrap();
    let dst = adapters::by_name_with_root("claude-code", Some(root.clone())).unwrap();
    let opts = ConvertOpts {
        mode: Mode::Handoff,
        tail: 8,
        write: WriteOpts { handshake: false, cwd_override: Some("/home/u/proj".into()), force: false },
    };
    let (outcome, report) =
        convert::run(src.as_ref(), dst.as_ref(), &seeded.artifacts[0].to_string_lossy(), &opts).unwrap();
    assert!(report.notes.iter().any(|n| n.contains("handoff")));

    let cc = adapters::by_name_with_root("claude-code", Some(root)).unwrap();
    let back = cc.read(&outcome.artifacts[0].to_string_lossy()).unwrap();

    // First message is the briefing.
    let brief = Session::text_of(&back.messages[0]);
    assert!(brief.contains("Session handoff"));
    assert!(brief.contains("Files touched"));
    assert!(brief.contains("a.txt"), "file path from tool call should appear: {brief}");
    // Reasoning never survives a handoff.
    assert_eq!(block_kinds(&back).get("reasoning"), None);
    // Compact: fewer messages than the raw 5-item codex replay.
    assert!(back.messages.len() <= 4);
}

#[test]
fn find_ranks_by_description() {
    use portage::find;
    let dir = tempfile::tempdir().unwrap();
    let cc = adapters::by_name_with_root("claude-code", Some(dir.path().to_path_buf())).unwrap();

    let mut mk = |text: &str| {
        let mut s = Session::new("test");
        s.cwd = Some("/home/u/proj".into());
        s.messages = vec![Message {
            role: Role::User,
            blocks: vec![Block::Text { text: text.into() }],
            timestamp: None,
        }];
        cc.write(&s, &WriteOpts::default()).unwrap();
    };
    mk("refactor the authentication widget module");
    mk("fix the billing gadget invoice bug");

    let hits = find::search(cc.as_ref(), "authentication widget", 5).unwrap();
    assert!(!hits.is_empty());
    let top = std::fs::read_to_string(&hits[0].reference.locator).unwrap();
    assert!(top.contains("widget"), "top hit should be the widget session");
    // a query matching neither returns nothing
    assert!(find::search(cc.as_ref(), "nonexistentxyz", 5).unwrap().is_empty());
}

#[test]
fn redact_masks_secrets() {
    let r = portage::digest::redact("token sk-ABCD1234efgh5678IJKL90mnop here");
    assert!(r.contains("[redacted]"), "got: {r}");
    assert!(r.contains("token") && r.contains("here"));
    // ordinary words are untouched
    assert_eq!(portage::digest::redact("just normal words"), "just normal words");
}

#[test]
fn opencode_reader_maps_parts() {
    // Minimal real opencode schema (session/message/part with JSON `data`).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let db = root.join(".local/share/opencode/opencode.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE session(id TEXT PRIMARY KEY, directory TEXT, title TEXT, time_created INTEGER, time_updated INTEGER);
         CREATE TABLE message(id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
         CREATE TABLE part(id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, data TEXT);
         INSERT INTO session VALUES('s1','/tmp/p','t',1,1);
         INSERT INTO message VALUES('m1','s1',1,'{\"role\":\"user\"}');
         INSERT INTO part VALUES('p1','m1','s1','{\"type\":\"text\",\"text\":\"hi\"}');
         INSERT INTO message VALUES('m2','s1',2,'{\"role\":\"assistant\"}');
         INSERT INTO part VALUES('p2','m2','s1','{\"type\":\"reasoning\",\"text\":\"think\"}');
         INSERT INTO part VALUES('p3','m2','s1','{\"type\":\"text\",\"text\":\"answer\"}');
         INSERT INTO part VALUES('p4','m2','s1','{\"type\":\"tool\",\"callID\":\"c1\",\"tool\":\"read\",\"state\":{\"input\":{\"p\":\"a\"},\"output\":\"ok\",\"status\":\"completed\"}}');",
    )
    .unwrap();
    drop(conn);

    let oc = adapters::by_name_with_root("opencode", Some(root)).unwrap();
    let s = oc.read("s1").unwrap();
    assert_eq!(s.cwd.as_deref(), Some("/tmp/p"));
    assert_eq!(s.messages.len(), 2);
    let k = block_kinds(&s);
    assert_eq!(k.get("text"), Some(&2)); // hi + answer
    assert_eq!(k.get("reasoning"), Some(&1));
    assert_eq!(k.get("call"), Some(&1));
    assert_eq!(k.get("result"), Some(&1)); // tool part had output
}

#[test]
fn codex_index_registration_inserts_row() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join(".codex")).unwrap();
    let index = root.join(".codex").join("state_5.sqlite");
    let conn = rusqlite::Connection::open(&index).unwrap();
    conn.execute(
        "CREATE TABLE sessions (id TEXT, cwd TEXT, created TEXT)",
        [],
    )
    .unwrap();
    drop(conn);

    let codex = adapters::by_name_with_root("codex", Some(root.clone())).unwrap();
    codex.write(&sample(), &WriteOpts::default()).unwrap();

    let conn = rusqlite::Connection::open(&index).unwrap();
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1, "write should register one row in a recognized index");
}
