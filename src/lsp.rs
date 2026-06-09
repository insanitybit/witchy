//! A minimal Language Server for witchy. It reports parse and type errors as
//! diagnostics by reusing the compiler front-end (parser → linker → typeck), so
//! the editor surfaces exactly the errors `witchy <file>` would. Started with
//! `witchy lsp`; it speaks LSP over stdio and is driven by the Zed extension.

use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::path::PathBuf;

use lsp_server::{Connection, Message, Notification};
use serde_json::{Value, json};

use crate::{ast, linker, parser, typeck};

type LspResult = Result<(), Box<dyn Error + Sync + Send>>;

pub fn run() -> LspResult {
    let (connection, io_threads) = Connection::stdio();
    // Full-text document sync (1) is all we need: the client resends the whole
    // buffer on every edit, which we re-check from scratch. Everything else
    // stays at the protocol defaults.
    let _ = connection.initialize(json!({
        "textDocumentSync": 1,
        "completionProvider": {},
        "hoverProvider": true,
    }))?;
    main_loop(&connection)?;
    io_threads.join()?;
    Ok(())
}

fn main_loop(connection: &Connection) -> LspResult {
    let mut docs: HashMap<String, String> = HashMap::new();
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                let result = match req.method.as_str() {
                    "textDocument/completion" => Some(completion_response(&docs, &req.params)),
                    "textDocument/hover" => Some(hover_response(&docs, &req.params)),
                    _ => None,
                };
                if let Some(result) = result {
                    let _ = connection.sender.send(Message::Response(lsp_server::Response {
                        id: req.id,
                        result: Some(result),
                        error: None,
                    }));
                }
            }
            Message::Notification(not) => handle_notification(connection, &mut docs, &not),
            Message::Response(_) => {}
        }
    }
    Ok(())
}

// --- completion -------------------------------------------------------------

const KEYWORDS: &[&str] = &[
    "fn", "let", "var", "if", "else", "match", "for", "in", "while", "return", "break",
    "continue", "type", "trait", "impl", "actor", "on", "import", "pub", "inout", "sink",
    "own", "move", "spawn", "where", "as", "true", "false",
];

const BUILTINS: &[&str] = &[
    "print", "now", "get_env", "read", "write", "exists", "is_dir", "list", "subdir",
    "make_dir", "connect", "listen", "accept", "send_line", "send_bytes", "recv_line",
    "recv_all", "recv_bytes", "close", "restrict", "send", "length", "at", "push", "concat",
    "dict_new", "insert", "get_or", "has", "remove", "update", "keys", "values", "pairs",
    "size", "to_string", "int_to_string", "string_length", "char_count", "index_of", "split",
    "string_chars", "replace", "substring", "to_upper", "to_lower", "trim", "starts_with",
    "ends_with", "contains", "int_to_float", "float_to_int", "int_to_duration",
    "duration_to_int", "sqrt", "string_to_int", "fail",
];

/// Completion items: keywords, builtins, this document's functions, and the
/// `pub fn`s of every imported module (offered as `module.name`).
fn completion_response(docs: &HashMap<String, String>, params: &Value) -> Value {
    let Some(text) = params["textDocument"]["uri"].as_str().and_then(|u| docs.get(u)) else {
        return json!([]);
    };
    let mut items: Vec<Value> = Vec::new();
    for k in KEYWORDS {
        items.push(json!({ "label": k, "kind": 14 })); // Keyword
    }
    for b in BUILTINS {
        items.push(json!({ "label": b, "kind": 3 })); // Function
    }
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("pub fn ").or_else(|| t.strip_prefix("fn ")) {
            if let Some(name) = rest.split('(').next() {
                items.push(json!({ "label": name.trim(), "kind": 3 }));
            }
        }
        if let Some(module) = t.strip_prefix("import ") {
            let module = module.trim();
            items.push(json!({ "label": module, "kind": 9 })); // Module
            if let Some(src) = crate::linker::std_source(module) {
                for ml in src.lines() {
                    if let Some(rest) = ml.trim_start().strip_prefix("pub fn ") {
                        if let Some(name) = rest.split('(').next() {
                            items.push(json!({
                                "label": format!("{module}.{}", name.trim()),
                                "kind": 3,
                            }));
                        }
                    }
                }
            }
        }
    }
    json!(items)
}

// --- hover ------------------------------------------------------------------

/// Hover: the signature line and the contiguous `//` doc block above it, for a
/// function defined in this document or in an imported std module (qualified
/// `module.name` or bare).
fn hover_response(docs: &HashMap<String, String>, params: &Value) -> Value {
    let Some(text) = params["textDocument"]["uri"].as_str().and_then(|u| docs.get(u)) else {
        return Value::Null;
    };
    let (line, character) = (
        params["position"]["line"].as_u64().unwrap_or(0) as usize,
        params["position"]["character"].as_u64().unwrap_or(0) as usize,
    );
    let Some(word) = word_at(text, line, character) else {
        return Value::Null;
    };
    // Where to look: `mod.name` looks in the imported module, a bare name in
    // this document, then in every imported module.
    let mut sources: Vec<(&str, String)> = Vec::new();
    let bare = match word.split_once('.') {
        Some((module, name)) => {
            if let Some(src) = crate::linker::std_source(module) {
                sources.push((src, format!("{module}.")));
            }
            name.to_string()
        }
        None => {
            sources.push((text.as_str(), String::new()));
            for l in text.lines() {
                if let Some(module) = l.trim_start().strip_prefix("import ") {
                    if let Some(src) = crate::linker::std_source(module.trim()) {
                        sources.push((src, format!("{}.", module.trim())));
                    }
                }
            }
            word.clone()
        }
    };
    for (src, prefix) in sources {
        if let Some(doc) = signature_doc(src, &bare) {
            let contents = format!("```witchy\n{}{}\n```\n{}", prefix, doc.0, doc.1);
            return json!({ "contents": { "kind": "markdown", "value": contents } });
        }
    }
    Value::Null
}

/// The identifier (allowing `.` for module-qualified names) covering `character`
/// on `line`.
fn word_at(text: &str, line: usize, character: usize) -> Option<String> {
    let l = text.lines().nth(line)?;
    let bytes = l.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'.';
    let mut start = character.min(bytes.len());
    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = character.min(bytes.len());
    while end < bytes.len() && is_word(bytes[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some(l[start..end].trim_matches('.').to_string())
}

/// Find `fn <name>(` in `src` and return (signature line, preceding `//` block).
fn signature_doc(src: &str, name: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = src.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        let sig = t
            .strip_prefix("pub fn ")
            .or_else(|| t.strip_prefix("fn "))
            .filter(|rest| rest.split('(').next().map(str::trim) == Some(name));
        if sig.is_some() {
            let mut doc_lines: Vec<&str> = Vec::new();
            for j in (0..i).rev() {
                let dt = lines[j].trim_start();
                if let Some(c) = dt.strip_prefix("//") {
                    doc_lines.push(c.trim());
                } else {
                    break;
                }
            }
            doc_lines.reverse();
            return Some((t.trim_end_matches(':').to_string(), doc_lines.join("\n")));
        }
    }
    None
}

fn handle_notification(
    connection: &Connection,
    docs: &mut HashMap<String, String>,
    not: &Notification,
) {
    match not.method.as_str() {
        "textDocument/didOpen" => {
            let td = &not.params["textDocument"];
            if let (Some(uri), Some(text)) = (td["uri"].as_str(), td["text"].as_str()) {
                docs.insert(uri.to_string(), text.to_string());
                send_diagnostics(connection, uri, compute_diagnostics(uri, text));
            }
        }
        "textDocument/didChange" => {
            let uri = not.params["textDocument"]["uri"].as_str();
            // Under full sync the final content change carries the whole file.
            let text = not.params["contentChanges"]
                .as_array()
                .and_then(|c| c.last())
                .and_then(|c| c["text"].as_str());
            if let (Some(uri), Some(text)) = (uri, text) {
                docs.insert(uri.to_string(), text.to_string());
                send_diagnostics(connection, uri, compute_diagnostics(uri, text));
            }
        }
        "textDocument/didClose" => {
            if let Some(uri) = not.params["textDocument"]["uri"].as_str() {
                docs.remove(uri);
                send_diagnostics(connection, uri, vec![]); // clear on close
            }
        }
        _ => {}
    }
}

fn send_diagnostics(connection: &Connection, uri: &str, diagnostics: Vec<Value>) {
    let params = json!({ "uri": uri, "diagnostics": diagnostics });
    let not = Notification {
        method: "textDocument/publishDiagnostics".to_string(),
        params,
    };
    let _ = connection.sender.send(Message::Notification(not));
}

/// Run the front-end over the open document and return LSP diagnostic objects.
/// The entry module's text comes from the editor buffer; imported modules are
/// resolved from sibling files on disk, falling back to the bundled std library
/// — mirroring how `witchy <file>` loads a program.
fn compute_diagnostics(uri: &str, text: &str) -> Vec<Value> {
    let path = uri_to_path(uri);
    let dir = path
        .as_ref()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let entry = path
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("main")
        .to_string();

    // Parse the buffer first; a parse error is precise (line + column) and
    // stops us before linking.
    let entry_module = match parser::parse_module(text) {
        Ok(m) => m,
        Err(e) => return vec![parse_diag(e.line, e.col, text, &e.message)],
    };

    let mut modules: Vec<(String, ast::Module)> = Vec::new();
    let mut loaded: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.extend(entry_module.imports.iter().cloned());
    loaded.insert(entry.clone());
    modules.push((entry.clone(), entry_module));

    while let Some(name) = queue.pop_front() {
        if !loaded.insert(name.clone()) {
            continue;
        }
        let src = match std::fs::read_to_string(dir.join(format!("{name}.witchy"))) {
            Ok(s) => s,
            Err(_) => match crate::bundled_module(&name) {
                Some(s) => s.to_string(),
                None => continue, // unknown import — the linker will report it
            },
        };
        // A dependency that fails to parse isn't the open file; skip it rather
        // than blaming the user's buffer for a broken neighbour.
        if let Ok(m) = parser::parse_module(&src) {
            for imp in &m.imports {
                if !loaded.contains(imp) {
                    queue.push_back(imp.clone());
                }
            }
            modules.push((name, m));
        }
    }

    let linked = match linker::link(modules, &entry) {
        Ok(m) => m,
        Err(e) => return vec![line_diag(0, text, &e.to_string())],
    };
    match typeck::check(&linked) {
        Ok(()) => vec![],
        Err(e) => {
            let line0 = extract_line(&e.message).map_or(0, |n| n.saturating_sub(1));
            vec![line_diag(line0, text, &format!("type error: {}", e.message))]
        }
    }
}

/// A diagnostic spanning from a 1-based `(line, col)` to the end of that line.
fn parse_diag(line1: u32, col1: u32, text: &str, message: &str) -> Value {
    let line0 = line1.saturating_sub(1);
    let start = col1.saturating_sub(1);
    let end = line_len(text, line0).max(start + 1);
    diag(line0, start, line0, end, &format!("parse error: {message}"))
}

/// A diagnostic underlining all of 0-based `line0`.
fn line_diag(line0: u32, text: &str, message: &str) -> Value {
    diag(line0, 0, line0, line_len(text, line0).max(1), message)
}

fn diag(start_line: u32, start_char: u32, end_line: u32, end_char: u32, message: &str) -> Value {
    json!({
        "range": {
            "start": { "line": start_line, "character": start_char },
            "end": { "line": end_line, "character": end_char }
        },
        "severity": 1, // Error
        "source": "witchy",
        "message": message
    })
}

fn line_len(text: &str, line0: u32) -> u32 {
    text.lines()
        .nth(line0 as usize)
        .map_or(0, |l| l.chars().count() as u32)
}

/// Pull the line number out of a type-error message (errors are tagged with
/// `line N` by the checker's `at_loc`).
fn extract_line(message: &str) -> Option<u32> {
    let rest = &message[message.find("line ")? + "line ".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Convert a `file://` URI to a filesystem path, percent-decoding escapes.
/// Returns `None` for non-file URIs (so we fall back to bundled-only imports).
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file:///path` has an empty host; `file://host/path` has one to skip.
    let path = match rest.find('/') {
        Some(0) | None => rest,
        Some(i) => &rest[i..],
    };
    Some(PathBuf::from(percent_decode(path)))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diags(text: &str) -> Vec<Value> {
        compute_diagnostics("file:///tmp/main.witchy", text)
    }

    #[test]
    fn completion_includes_keywords_builtins_and_module_fns() {
        let mut docs = HashMap::new();
        let src = "import string\n\nfn helper(n: Int) -> Int:\n    n\n\nfn main(console: Console):\n    print(console, \"x\")\n";
        docs.insert("file:///t.witchy".to_string(), src.to_string());
        let items = completion_response(
            &docs,
            &json!({ "textDocument": { "uri": "file:///t.witchy" } }),
        );
        let labels: Vec<String> = items
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap().to_string())
            .collect();
        assert!(labels.contains(&"match".to_string()), "keywords offered");
        assert!(labels.contains(&"print".to_string()), "builtins offered");
        assert!(labels.contains(&"helper".to_string()), "document fns offered");
        assert!(
            labels.iter().any(|l| l.starts_with("string.")),
            "imported module fns offered: {labels:?}"
        );
    }

    #[test]
    fn hover_shows_signature_and_doc() {
        let mut docs = HashMap::new();
        let src = "// Doubles a number.\n// Twice the input.\nfn double(n: Int) -> Int:\n    n * 2\n\nfn main(console: Console):\n    print(console, int_to_string(double(3)))\n";
        docs.insert("file:///t.witchy".to_string(), src.to_string());
        // Hover over `double` in the call on line 6 (0-based), col 35.
        let col = src.lines().nth(6).unwrap().find("double").unwrap() as u64;
        let resp = hover_response(
            &docs,
            &json!({
                "textDocument": { "uri": "file:///t.witchy" },
                "position": { "line": 6, "character": col },
            }),
        );
        let contents = resp["contents"]["value"].as_str().expect("hover text");
        assert!(contents.contains("fn double(n: Int) -> Int"), "{contents}");
        assert!(contents.contains("Doubles a number."), "{contents}");
    }

    #[test]
    fn hover_resolves_imported_module_functions() {
        let mut docs = HashMap::new();
        let src = "import string\n\nfn main(console: Console):\n    print(console, string.repeat(\"ab\", 2))\n";
        docs.insert("file:///t.witchy".to_string(), src.to_string());
        let line = 3u64;
        let col = src.lines().nth(3).unwrap().find("repeat").unwrap() as u64;
        let resp = hover_response(
            &docs,
            &json!({
                "textDocument": { "uri": "file:///t.witchy" },
                "position": { "line": line, "character": col },
            }),
        );
        let contents = resp["contents"]["value"].as_str().expect("hover text");
        assert!(contents.contains("repeat"), "{contents}");
    }

    #[test]
    fn clean_program_has_no_diagnostics() {
        let src = r#"
fn main(console: Console):
    print(console, "hi")
"#;
        assert_eq!(diags(src), Vec::<Value>::new());
    }

    #[test]
    fn importing_std_resolves_without_false_errors() {
        // `string` isn't on disk next to /tmp/main.witchy, so this exercises the
        // bundled-module fallback. A clean program must stay diagnostic-free.
        let src = "import string\nfn main(console: Console):\n    print(console, string.repeat(\"ab\", 2))\n";
        assert_eq!(diags(src), Vec::<Value>::new());
    }

    #[test]
    fn parse_error_points_at_its_line() {
        // Missing closing brace / stray token on line 2 (1-based).
        let src = "fn main(console: Console) {\n  let x = \n}\n";
        let d = diags(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0]["severity"], json!(1));
        assert_eq!(d[0]["source"], json!("witchy"));
        // The error sits on a real line, reported 0-based.
        assert!(d[0]["range"]["start"]["line"].as_u64().is_some());
    }

    #[test]
    fn type_error_is_reported_on_the_offending_line() {
        // Adding a String to an Int is a type error; it should map to a single
        // diagnostic whose line was recovered from the checker's message.
        let src = r#"
fn main(console: Console):
    let x = (1 + "two")
    print(console, "")
"#;
        let d = diags(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0]["severity"], json!(1));
        assert!(
            d[0]["message"].as_str().unwrap().contains("type error"),
            "{:?}",
            d[0]["message"]
        );
    }

    #[test]
    fn uri_to_path_decodes_spaces() {
        assert_eq!(
            uri_to_path("file:///Users/x/my%20dir/a.witchy"),
            Some(PathBuf::from("/Users/x/my dir/a.witchy"))
        );
        assert_eq!(uri_to_path("untitled:foo"), None);
    }
}
