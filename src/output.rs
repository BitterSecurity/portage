//! Output formats for CLI (and MCP) results, tuned for the primary consumer:
//! an LLM agent reading stdout. Three formats per command:
//!
//! - `toon` — Token-Oriented Object Notation (tab delimiter). The research
//!   case (github.com/toon-format/toon, 244-question retrieval benchmark over
//!   4 models): for uniform tables — exactly what `list`/`find` return — TOON
//!   spends ~30–55% fewer tokens than JSON at equal or better retrieval
//!   accuracy, and the `[N]`/`{fields}` headers let a model catch truncation.
//! - `json` — lossless structured form for scripts.
//! - `text` — human-oriented lines (the old default), kept via `--format text`.
//!
//! Payloads are plain `serde_json` values; only the rendering differs. TOON
//! here is an output-side subset of the spec (scalars, one nesting level,
//! uniform tables, tab delimiter) — portage never decodes TOON.

use anyhow::{bail, Result};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Toon,
    Json,
    Text,
}

pub fn parse_format(s: &str) -> Result<Format> {
    match s {
        "toon" => Ok(Format::Toon),
        "json" => Ok(Format::Json),
        "text" => Ok(Format::Text),
        other => bail!("unknown --format '{other}' (use toon, json, or text)"),
    }
}

/// Render a payload in the given format.
pub fn render(v: &Value, fmt: Format) -> String {
    match fmt {
        Format::Json => serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into()),
        Format::Toon => toon(v, 0),
        Format::Text => text(v),
    }
}

/// TOON encoding with tab delimiter (fewer tokens than comma, and titles
/// rarely contain tabs — less escaping). Output-side subset: scalars, one
/// level of nested objects, uniform object tables, scalar arrays.
fn toon(v: &Value, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    let obj = match v {
        Value::Object(m) => m,
        _ => return format!("{indent}{}\n", toon_field(v)),
    };
    let mut out = String::new();
    for (k, val) in obj {
        match val {
            Value::Object(_) => {
                out.push_str(&format!("{indent}{k}:\n"));
                out.push_str(&toon(val, depth + 1));
            }
            Value::Array(items) => out.push_str(&toon_array(k, items, &indent)),
            scalar => out.push_str(&format!("{indent}{k}: {}\n", toon_field(scalar))),
        }
    }
    out
}

fn toon_array(key: &str, items: &[Value], indent: &str) -> String {
    if items.is_empty() {
        return format!("{indent}{key}[0]:\n");
    }
    // Uniform tables: every element an object with the same field order —
    // the shape portage's listings always have.
    let fields: Vec<String> = match items.first() {
        Some(Value::Object(first)) => first.keys().cloned().collect(),
        _ => vec![],
    };
    let uniform = !fields.is_empty()
        && items.iter().all(|it| {
            it.as_object()
                .map(|o| o.len() == fields.len() && fields.iter().all(|f| o.contains_key(f)))
                .unwrap_or(false)
        });
    if uniform {
        let header_fields: Vec<&str> = fields.iter().map(String::as_str).collect();
        let mut out = format!("{indent}{key}[{}\t]{{{}}}:\n", items.len(), header_fields.join("\t"));
        for it in items {
            let row: Vec<String> = fields
                .iter()
                .map(|f| toon_field(it.get(f).unwrap_or(&Value::Null)))
                .collect();
            out.push_str(&format!("{indent}  {}\n", row.join("\t")));
        }
        return out;
    }
    // Scalar array: inline on the header line.
    if items.iter().all(|it| !it.is_object() && !it.is_array()) {
        let row: Vec<String> = items.iter().map(toon_field).collect();
        return format!("{indent}{key}[{}]: {}\n", items.len(), row.join("\t"));
    }
    // Mixed shapes fall back to one JSON line per element (still lossless).
    let mut out = format!("{indent}{key}[{}]:\n", items.len());
    for it in items {
        out.push_str(&format!("{indent}  - {}\n", serde_json::to_string(it).unwrap_or_default()));
    }
    out
}

fn toon_field(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => quote_if_needed(s),
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}

/// TOON string rule: quote anything that could mis-parse as another type or
/// contain structure (tab/newline/quotes/edge whitespace).
fn quote_if_needed(s: &str) -> String {
    let needs = s.is_empty()
        || s.starts_with(' ')
        || s.ends_with(' ')
        || s.contains(['\t', '\n', '\r', '"', '\\'])
        || s == "null"
        || s == "true"
        || s == "false"
        || s.parse::<i64>().is_ok()
        || s.parse::<f64>().is_ok();
    if !needs {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Human text form: one line per record, same fields as the TOON table (no
/// decoration), scalars as `key: value`.
fn text(v: &Value) -> String {
    fn scalar(v: &Value) -> Option<String> {
        match v {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                Some(match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
            }
            _ => None,
        }
    }
    match v {
        Value::Object(m) => {
            let mut out = String::new();
            for (k, val) in m {
                match val {
                    Value::Array(items) => {
                        out.push_str(&format!("{k}[{}]:\n", items.len()));
                        for it in items {
                            match scalar(it) {
                                Some(s) => out.push_str(&format!("  {s}\n")),
                                None => out.push_str(&text(it)),
                            }
                        }
                    }
                    Value::Object(_) => out.push_str(&text(val)),
                    _ => {
                        if let Some(s) = scalar(val) {
                            out.push_str(&format!("{k}: {s}\n"));
                        }
                    }
                }
            }
            out
        }
        _ => format!("{}\n", scalar(v).unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn toon_uniform_table_uses_tabular_form() {
        let v = json!({
            "query": "login encryption",
            "hits": [
                {"terms": 2, "title": "login page", "cwd": "/home/u/p", "locator": "/s/a.jsonl"},
                {"terms": 1, "title": "auth, refactor", "cwd": "/home/u/p", "locator": "/s/b.jsonl"}
            ]
        });
        let out = render(&v, Format::Toon);
        assert_eq!(
            out,
            "query: login encryption\n\
             hits[2\t]{terms\ttitle\tcwd\tlocator}:\n  \
             2\tlogin page\t/home/u/p\t/s/a.jsonl\n  \
             1\tauth, refactor\t/home/u/p\t/s/b.jsonl\n"
        );
        // Structural guardrails a model can validate: length + field list.
        assert!(out.contains("hits[2\t]"));
    }

    #[test]
    fn toon_quotes_ambiguous_and_structural_strings() {
        let v = json!({"note": "a\tb", "count": "42", "flag": "true", "empty": "", "plain": "ok"});
        let out = render(&v, Format::Toon);
        assert!(out.contains(r#"note: "a\tb""#), "{out}");
        assert!(out.contains(r#"count: "42""#), "{out}");
        assert!(out.contains(r#"flag: "true""#), "{out}");
        assert!(out.contains(r#"empty: """#), "{out}");
        assert!(out.contains("plain: ok\n"), "{out}");
    }

    #[test]
    fn toon_scalar_array_is_inline() {
        let v = json!({"artifacts": ["/a/db", "/b/c"], "notes": []});
        let out = render(&v, Format::Toon);
        assert_eq!(out, "artifacts[2]: /a/db\t/b/c\nnotes[0]:\n");
    }

    #[test]
    fn json_and_text_rendering() {
        let v = json!({"resume": "cd /x && opencode --session s1", "hits": [1, 2]});
        assert!(render(&v, Format::Json).contains("\"resume\":"));
        let t = render(&v, Format::Text);
        assert!(t.contains("resume: cd /x && opencode --session s1"));
        assert!(t.contains("hits[2]:\n  1\n  2"));
    }
}
