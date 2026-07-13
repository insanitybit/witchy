//! A minimal Language Server for witchy. It reports parse and type errors as
//! diagnostics by reusing the compiler front-end (parser → linker → typeck), so
//! the editor surfaces exactly the errors `witchy <file>` would. Started with
//! `witchy lsp`; it speaks LSP over stdio and is driven by the Zed extension.

use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::path::PathBuf;

use lsp_server::{Connection, Message, Notification};
use serde_json::{Value, json};

use crate::{ast, parser, typeck};

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
    "own", "move", "where", "as", "capability", "region", "comptime", "from", "true", "false",
];

const BUILTINS: &[&str] = &[
    // Capability operations (authority is loud and unprefixed) + the two
    // universal staples. Pure data operations live in their modules
    // (list./string./dict./math., offered via the prelude completion below).
    "print", "now", "get_env", "read", "write", "append", "exists", "is_dir", "list", "subtree", "read_file", "write_file",
    "make_dir", "exec", "connect", "try_connect", "listen", "accept", "send_line", "send_bytes", "recv_line",
    "recv_all", "recv_bytes", "close", "send", "fail",
];

/// Completion items: keywords, builtins, this document's functions, and the
/// `pub fn`s of every prelude/imported module (offered as `module.name`).
fn completion_response(docs: &HashMap<String, String>, params: &Value) -> Value {
    let uri = params["textDocument"]["uri"].as_str();
    let Some(text) = uri.and_then(|u| docs.get(u)) else {
        return json!([]);
    };
    let mut items: Vec<Value> = Vec::new();
    for module in crate::linker::PRELUDE_MODULES {
        // Prelude completion comes from the canonical source registry, so it
        // cannot drift from the linker or be replaced by a sibling file.
        push_module_completions(&mut items, module, None, docs);
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
            push_module_completions(&mut items, module.trim(), uri, docs);
        }
        // `from X import a, b` (RFC-0042) binds `a`/`b` UNQUALIFIED and implies
        // `import X`, so offer the bare names plus the module's qualified fns.
        if let Some((module, names)) = parse_from_import(t) {
            for name in names {
                items.push(json!({ "label": name, "kind": 3 }));
            }
            push_module_completions(&mut items, &module, uri, docs);
        }
    }
    json!(items)
}

/// The `module` label plus every `module.fn` of a resolvable module — the
/// completions an `import module` (or the module half of a `from`-import) offers.
/// The module resolves as std, a sibling `<module>.witchy`, or an open buffer.
fn push_module_completions(
    items: &mut Vec<Value>,
    module: &str,
    uri: Option<&str>,
    docs: &HashMap<String, String>,
) {
    items.push(json!({ "label": module, "kind": 9 })); // Module
    if let Some(src) = module_source(module, uri, docs) {
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

/// Resolve an imported module NAME to its source: a bundled std module, a sibling
/// `<name>.witchy` on disk next to the open document, or (preferred, for unsaved
/// edits) an open editor buffer. Local resolution needs the open document's URI
/// to find the containing directory; `None` there falls back to std-only.
fn module_source(
    name: &str,
    doc_uri: Option<&str>,
    docs: &HashMap<String, String>,
) -> Option<String> {
    if let Some(src) = crate::linker::std_source(name) {
        return Some(src.to_string());
    }
    let dir = doc_uri.and_then(uri_to_path).and_then(|p| p.parent().map(PathBuf::from))?;
    let sibling = dir.join(format!("{name}.witchy"));
    // An open buffer (possibly with unsaved edits) wins over the on-disk copy.
    open_buffer(&sibling, docs).or_else(|| std::fs::read_to_string(&sibling).ok())
}

/// The contents of an open editor buffer whose URI maps to `path` — an unsaved
/// sibling module the client is currently editing.
fn open_buffer(path: &std::path::Path, docs: &HashMap<String, String>) -> Option<String> {
    docs.iter()
        .find(|(u, _)| uri_to_path(u).as_deref() == Some(path))
        .map(|(_, buf)| buf.clone())
}

/// Parse a `from X import a, b, c` line into `(module, [names])` (RFC-0042).
/// Returns `None` for any other line.
fn parse_from_import(line: &str) -> Option<(String, Vec<String>)> {
    let rest = line.trim_start().strip_prefix("from ")?;
    let (module, names) = rest.split_once(" import ")?;
    let names = names
        .split(',')
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect();
    Some((module.trim().to_string(), names))
}

// --- hover ------------------------------------------------------------------

/// Hover: the signature line and the contiguous `//` doc block above it, for a
/// function defined in this document or in an imported std module (qualified
/// `module.name` or bare).
fn hover_response(docs: &HashMap<String, String>, params: &Value) -> Value {
    let uri = params["textDocument"]["uri"].as_str();
    let Some(text) = uri.and_then(|u| docs.get(u)) else {
        return Value::Null;
    };
    let (line, character) = (
        params["position"]["line"].as_u64().unwrap_or(0) as usize,
        params["position"]["character"].as_u64().unwrap_or(0) as usize,
    );
    let Some(word) = word_at(text, line, character) else {
        return Value::Null;
    };
    // Where to look: `mod.name` looks in the named module; a receiver method
    // (`xs.push`) or a bare name looks in this document and every module the
    // document can see (prelude + imports).
    let mut sources: Vec<(String, String)> = Vec::new();
    let bare = match word.split_once('.') {
        Some((head, tail)) => {
            // `head.tail`: `head` is a module (`string.repeat`) or a receiver
            // value (`xs.push`). When it names a module, look there; otherwise
            // treat `tail`'s final segment as a method and resolve it against
            // every visible module (`xs.push` → `list.push`).
            let name = tail.rsplit_once('.').map_or(tail, |(_, n)| n).to_string();
            if let Some(src) = module_source(head, uri, docs) {
                sources.push((src, format!("{head}.")));
            } else {
                sources.extend(visible_module_sources(text, uri, docs));
            }
            name
        }
        None => {
            sources.push((text.to_string(), String::new()));
            sources.extend(visible_module_sources(text, uri, docs));
            word.clone()
        }
    };
    for (src, prefix) in sources {
        if let Some((sig, doc)) = signature_doc(&src, &bare) {
            let contents = format!("```witchy\n{}\n```\n{}", qualify_signature(&sig, &prefix), doc);
            return json!({ "contents": { "kind": "markdown", "value": contents } });
        }
    }
    Value::Null
}

/// Render a signature line qualified by `prefix` (e.g. `string.`). The qualifier
/// belongs before the FUNCTION NAME, not in front of the whole line: prepending
/// it produced the malformed `string.pub fn repeat(...)` (and `Net.pub fn tcp`).
/// Inserting it after the `fn`/`pub fn` keyword yields `pub fn string.repeat(...)`.
fn qualify_signature(sig: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return sig.to_string();
    }
    for kw in ["pub fn ", "fn "] {
        if let Some(rest) = sig.strip_prefix(kw) {
            return format!("{kw}{prefix}{rest}");
        }
    }
    format!("{prefix}{sig}")
}

/// The identifier (allowing `.` for module-qualified names) covering `character`
/// on `line`. Indexing is by CHARACTER offset, not byte offset: an LSP position's
/// `character` counts code units, so treating it as a byte index and slicing
/// `l[start..end]` lands off a UTF-8 char boundary on any multibyte line and
/// panics. Scanning over the decoded `chars` keeps every index on a boundary.
fn word_at(text: &str, line: usize, character: usize) -> Option<String> {
    let l = text.lines().nth(line)?;
    let chars: Vec<char> = l.chars().collect();
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.';
    let mut start = character.min(chars.len());
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = character.min(chars.len());
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    let word: String = chars[start..end].iter().collect();
    Some(word.trim_matches('.').to_string())
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

/// Module sources visible to this document without qualification at a use site:
/// the always-present prelude modules plus every `import`ed module (std or a
/// sibling/open-buffer local module), each paired with its `module.` display
/// prefix. Lets hover resolve a bare method (`xs.push` → `list.push`), an
/// imported function, or a `from`-imported name.
fn visible_module_sources(
    text: &str,
    uri: Option<&str>,
    docs: &HashMap<String, String>,
) -> Vec<(String, String)> {
    // Explicitly imported modules are searched BEFORE the ambient prelude, so a
    // bare name the document actually imports wins over an incidental
    // prelude-module namesake (`from string import repeat` beats `list.repeat`).
    let mut names: Vec<String> = Vec::new();
    for l in text.lines() {
        let lt = l.trim_start();
        if let Some(module) = lt.strip_prefix("import ") {
            names.push(module.trim().to_string());
        } else if let Some((module, _)) = parse_from_import(lt) {
            // `from X import Y` implies `import X`, so a bare `Y` resolves in X.
            names.push(module);
        }
    }
    names.extend(crate::linker::PRELUDE_MODULES.iter().map(|s| s.to_string()));
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for name in names {
        if seen.insert(name.clone()) {
            if let Some(src) = module_source(&name, uri, docs) {
                out.push((src, format!("{name}.")));
            }
        }
    }
    out
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
                send_diagnostics(connection, uri, compute_diagnostics(uri, text, docs));
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
                send_diagnostics(connection, uri, compute_diagnostics(uri, text, docs));
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
/// resolved from sibling files on disk (or an open buffer for unsaved edits),
/// falling back to the bundled std library — mirroring how `witchy <file>` loads
/// a program.
fn compute_diagnostics(uri: &str, text: &str, docs: &HashMap<String, String>) -> Vec<Value> {
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

    // Import-resolution problems (a neighbour that is missing or won't parse) are
    // reported against the entry buffer's import site, not swallowed. Collected
    // here and returned in place of the confusing line-0 link cascade they'd
    // otherwise provoke.
    let mut import_diags: Vec<Value> = Vec::new();
    while let Some(name) = queue.pop_front() {
        if !loaded.insert(name.clone()) {
            continue;
        }
        // Resolve like `witchy <file>`: sibling `<name>.witchy` on disk (or its
        // open buffer for unsaved edits) first, then the bundled std library.
        let sibling = dir.join(format!("{name}.witchy"));
        let src = std::fs::read_to_string(&sibling)
            .ok()
            .or_else(|| open_buffer(&sibling, docs))
            .or_else(|| crate::bundled_module(&name).map(str::to_string));
        let Some(src) = src else {
            // BUG-168: an import that resolves to nothing. Say so at the import
            // line instead of letting the linker emit a line-0
            // "module `main` imports unknown module `name`".
            let line0 = import_line_of(text, &name).unwrap_or(0);
            import_diags.push(line_diag(
                line0,
                text,
                &format!(
                    "cannot resolve import `{name}`: no `{name}.witchy` beside this file, and it is not a bundled module"
                ),
            ));
            continue;
        };
        // BUG-137: a neighbour that fails to parse used to be silently skipped,
        // so the buffer showed no error at all — surface it against the import.
        match parser::parse_module(&src) {
            Ok(m) => {
                for imp in &m.imports {
                    if !loaded.contains(imp) {
                        queue.push_back(imp.clone());
                    }
                }
                modules.push((name, m));
            }
            Err(e) => {
                let line0 = import_line_of(text, &name).unwrap_or(0);
                import_diags.push(line_diag(
                    line0,
                    text,
                    &format!(
                        "imported module `{name}` failed to parse: {} (line {})",
                        e.message, e.line
                    ),
                ));
            }
        }
    }
    if !import_diags.is_empty() {
        return import_diags;
    }

    let linked = match crate::pipeline::link(modules, &entry) {
        Ok(m) => m,
        Err(e) => {
            // BUG-162: map the link error onto the line it names (or the import it
            // blames) instead of always pinning it to line 0.
            let msg = e.to_string();
            return vec![line_diag(link_error_line(&msg, text), text, &msg)];
        }
    };
    match typeck::check(&linked) {
        Ok(()) => {
            // A file that declares `mode opt` turns the performance contract into a
            // HARD gate — the same one `witchy check` enforces via
            // `enforce_performance_modes`. Mirror it here so the editor surfaces those
            // errors instead of silently accepting a program `check` rejects (BUG-165).
            let enforce = !linked.modes.is_empty();
            let modes = linked.modes.join(", ");
            let mut diags: Vec<Value> = Vec::new();

            // Copy-cliffs: a plain note normally (Hint, rendered unobtrusively); in a
            // `mode` file the cliff is a hard error. Only the buffer's OWN functions
            // (the entry module's `main` / `{entry}.fn`) are judged; linked-in modules
            // keep their own policy.
            for (func, c) in crate::analysis::module_cliffs(&linked) {
                if !crate::is_entry_function(&func, &entry) {
                    continue;
                }
                let line0 = c.line.saturating_sub(1);
                if enforce {
                    diags.push(line_diag(
                        line0,
                        text,
                        &format!(
                            "`{}` is rebuilt by copy on every iteration of this loop (in `{func}`): it is {} — `mode {modes}` requires it stay on the in-place path",
                            c.var, c.reason
                        ),
                    ));
                } else {
                    let end = line_len(text, line0);
                    diags.push(json!({
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
                    }));
                }
            }

            // Signature contract (mode files only): every ownership-relevant parameter
            // must declare its convention (`let`/`own`/`var`), so the interprocedural
            // summaries are declared facts. A bare (default) `let` is the violation.
            if enforce {
                for item in &linked.items {
                    let ast::Item::Function(f) = item else { continue };
                    if !crate::is_entry_function(&f.name, &entry) {
                        continue;
                    }
                    for p in &f.params {
                        if p.convention == ast::Convention::Let && crate::ownership_relevant(&p.ty) {
                            let bare = f.name.rsplit('.').next().unwrap_or(&f.name);
                            let line0 = fn_decl_line(text, bare).unwrap_or(0);
                            diags.push(line_diag(
                                line0,
                                text,
                                &format!(
                                    "parameter `{}` has no ownership convention — `mode {modes}` requires an explicit `let` (read-only borrow), `own` (consumed), or `var` (mutated in place)",
                                    p.name
                                ),
                            ));
                        }
                    }
                }
            }
            diags
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

/// The 0-based line of `text` declaring function `bare` (`fn bare(` / `pub fn bare(`),
/// so a mode-opt signature-contract error can point at the offending function even
/// though the AST carries no span for it.
fn fn_decl_line(text: &str, bare: &str) -> Option<u32> {
    let needle = format!("fn {bare}(");
    text.lines().position(|l| {
        let t = l.trim_start();
        t.strip_prefix("pub ").unwrap_or(t).starts_with(&needle)
    }).map(|i| i as u32)
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

/// The 0-based line of `text` that imports `name` (`import name` or
/// `from name import …`), if any.
fn import_line_of(text: &str, name: &str) -> Option<u32> {
    text.lines().enumerate().find_map(|(i, l)| {
        let t = l.trim_start();
        let hit = t.strip_prefix("import ").map(str::trim) == Some(name)
            || parse_from_import(t).is_some_and(|(m, _)| m == name);
        hit.then_some(i as u32)
    })
}

/// The line a link error should underline: the `line N` it carries, else the
/// import line of the first module it names in backticks, else the top of file.
/// Link errors otherwise all pinned to line 0 (BUG-162).
fn link_error_line(message: &str, text: &str) -> u32 {
    if let Some(n) = extract_line(message) {
        return n.saturating_sub(1);
    }
    // The message may name several things in backticks (e.g. the importing module
    // AND the unknown one); use the first that this file actually imports.
    for name in backtick_tokens(message) {
        if let Some(line0) = import_line_of(text, &name) {
            return line0;
        }
    }
    0
}

/// The contents of every `` `…` `` span in `s`, in order.
fn backtick_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        match after.find('`') {
            Some(end) => {
                out.push(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    out
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
    // Decode `%XX` escapes byte-wise. Parsing the two hex digits from the byte
    // array (not `&s[i+1..i+3]`) is deliberate: a `%` followed by a multi-byte
    // UTF-8 char (e.g. `%€`) would make `i+3` fall mid-codepoint, and string
    // slicing there panics — crashing the LSP on every affected open/change.
    let hex = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    };
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
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
