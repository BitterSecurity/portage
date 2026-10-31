//! Synthetic round-trip + conversion tests. These prove the IR pipeline and the
//! CC/Codex mappings are internally consistent against fixtures in the format the
//! adapters expect. They do NOT prove a converted session resumes in a real
//! install — that bar requires the actual harness (see design §3).

use portage::adapters;
use portage::convert;
use portage::harness::WriteOpts;
use portage::ir::{Block, Message, Role, Session};
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
    let opts = WriteOpts { handshake: true, cwd_override: Some("/home/u/proj".into()), force: false };
    let (outcome, report) =
        convert::run(src.as_ref(), dst.as_ref(), &seeded.artifacts[0].to_string_lossy(), &opts).unwrap();

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
