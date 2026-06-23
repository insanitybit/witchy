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
    "fn", "gen", "yield", "async", "await", "let", "var", "if", "else", "match", "for", "in",
    "while", "return", "break", "continue", "type", "trait", "impl", "import", "pub",
    "own", "move", "where", "as", "retain", "without", "region", "comptime", "true", "false",
];

const BUILTINS: &[&str] = &[
    // Capability operations (authority is loud and unprefixed) + the two
    // universal staples. Pure data operations live in their modules
    // (list./string./dict./math., offered via the prelude completion below).
    "print", "now", "get_env", "read", "write", "append", "exists", "is_dir", "list", "subdir",
    "make_dir", "exec", "connect", "try_connect", "listen", "accept", "send_line", "send_bytes", "recv_line",
    "recv_all", "recv_bytes", "close", "restrict", "send", "fail",
];

/// The prelude's module-qualified core operations, completed without an
/// import line (the linker always bundles these modules).
const PRELUDE_FNS: &[&str] = &[
    "list.push", "list.at", "list.length", "list.concat", "list.map", "list.filter",
    "list.fold", "list.sort_by", "list.contains", "list.index_of",
    "string.split", "string.trim", "string.length", "string.char_count", "string.chars",
    "string.contains", "string.starts_with", "string.ends_with", "string.replace",
    "string.substring", "string.index_of", "string.to_upper", "string.to_lower",
    "string.to_int", "string.parse_int", "string.join",
    "dict.new", "dict.insert", "dict.get_or", "dict.get", "dict.has", "dict.remove",
    "dict.update", "dict.keys", "dict.values", "dict.pairs", "dict.size",
    "math.to_float", "math.to_int", "math.sqrt", "math.min", "math.max", "math.abs",
];

/// Completion items: keywords, builtins, this document's functions, and the
/// `pub fn`s of every imported module (offered as `module.name`).
fn completion_response(docs: &HashMap<String, String>, params: &Value) -> Value {
    let Some(text) = params["textDocument"]["uri"].as_str().and_then(|u| docs.get(u)) else {
        return json!([]);
    };
    let mut items: Vec<Value> = Vec::new();
    for f in PRELUDE_FNS {
        items.push(json!({ "label": f, "kind": 3 }));
    }
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
        Ok(()) => {
            // Performance notes (severity Hint -> rendered unobtrusively):
            // accumulation that reverts to the copying path inside a loop.
            crate::analysis::module_cliffs(&linked)
                .into_iter()
                // Only the user's own functions: linked-in module functions
                // carry qualified (`module.fn`) names — their cliffs belong
                // to that module's author, not this buffer.
                .filter(|(func, _)| !func.contains('.'))
                .map(|(func, c)| {
                    let line0 = c.line.saturating_sub(1);
                    let end = line_len(text, line0);
                    json!({
                        "range": {
                            "start": { "line": line0, "character": 0 },
                            "end": { "line": line0, "character": end }
                        },
                        "severity": 4, // Hint
                        "source": "witchy",
                        "message": format!(
                            "`{}` is rebuilt by copy on every iteration (in `{func}`): it is {} — O(n²)",
                            c.var, c.reason
                        )
                    })
                })
                .collect()
        }
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
#[path = "lsp_tests.rs"]
mod tests;
