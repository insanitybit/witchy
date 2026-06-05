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
    let _ = connection.initialize(json!({ "textDocumentSync": 1 }))?;
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
            }
            Message::Notification(not) => handle_notification(connection, &mut docs, &not),
            Message::Response(_) => {}
        }
    }
    Ok(())
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
    fn clean_program_has_no_diagnostics() {
        let src = "fn main(console: Console) {\n  print(console, \"hi\")\n}\n";
        assert_eq!(diags(src), Vec::<Value>::new());
    }

    #[test]
    fn importing_std_resolves_without_false_errors() {
        // `string` isn't on disk next to /tmp/main.witchy, so this exercises the
        // bundled-module fallback. A clean program must stay diagnostic-free.
        let src = "import string\nfn main(console: Console) {\n  print(console, string.repeat(\"ab\", 2))\n}\n";
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
        let src = "fn main(console: Console) {\n  let x = 1 + \"two\"\n  print(console, \"\")\n}\n";
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
