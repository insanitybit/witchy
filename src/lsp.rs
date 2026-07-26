//! A minimal Language Server for witchy. It reports parse and type errors as
//! diagnostics by reusing the compiler front-end (parser → linker → typeck), so
//! the editor surfaces exactly the errors `witchy <file>` would. Started with
//! `witchy lsp`; it speaks LSP over stdio and is driven by the Zed extension.

use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::path::PathBuf;

use lsp_server::{Connection, Message, Notification};
use serde_json::{Value, json};

use witchy_syntax::{ast, parser};

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
        "documentSymbolProvider": true,
        "definitionProvider": true,
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
                    "textDocument/documentSymbol" => {
                        Some(document_symbol_response(&docs, &req.params))
                    }
                    "textDocument/definition" => {
                        Some(definition_response(&docs, &req.params))
                    }
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

// --- document symbols ------------------------------------------------------

/// Index handwritten entry declarations plus declarations produced by
/// compile-time expansion. Generated symbols carry their typed origin contract
/// in `data`; their visible range is the invocation span in the source buffer.
fn document_symbol_response(docs: &HashMap<String, String>, params: &Value) -> Value {
    let Some(uri) = params["textDocument"]["uri"].as_str() else {
        return json!([]);
    };
    let Some(text) = docs.get(uri) else {
        return json!([]);
    };
    let Ok(parsed) = parser::parse_module(text) else {
        return json!([]);
    };

    let mut symbols = Vec::new();
    for (index, item) in parsed.items.iter().enumerate() {
        let Some((name, kind)) = item_symbol(item) else { continue };
        let source_line = parsed.item_lines.get(index).copied().unwrap_or(1);
        if source_line == u32::MAX {
            continue;
        }
        let line = source_line.saturating_sub(1);
        let data = item_has_dynamic_dispatch(item)
            .then(|| json!({ "dynamicDispatch": true }));
        symbols.push(document_symbol(name, kind, line, text, data));
    }

    let Some((entry, _, linked)) = link_document_with_origins(uri, text, docs) else {
        return json!(symbols);
    };

    for generated in linked.origins.nodes() {
        if generated.origin.invocation.module != entry
            || !generated.node.path.is_empty()
            || generated.node.category != witchy_syntax::origin::SyntaxCategory::Item
        {
            continue;
        }
        let Some(item) = linked.module.items.get(generated.node.item as usize) else { continue };
        let Some((name, kind)) = item_symbol(item) else { continue };
        let prefix = format!("{entry}.");
        let name = name.strip_prefix(&prefix).unwrap_or(name);
        if name.starts_with("__") {
            continue;
        }
        let line = generated.origin.invocation.start.line.saturating_sub(1);
        symbols.push(document_symbol(
            name,
            kind,
            line,
            text,
            Some(json!({
                "generated": true,
                "dynamicDispatch": item_has_dynamic_dispatch(item),
                "id": {
                    "module": &generated.id.module,
                    "ordinal": generated.id.ordinal,
                },
                "origin": &generated.origin,
            })),
        ));
    }
    json!(symbols)
}

fn document_symbol(name: &str, kind: u32, line0: u32, text: &str, data: Option<Value>) -> Value {
    let end = line_len(text, line0).max(1);
    let mut symbol = json!({
        "name": name,
        "kind": kind,
        "range": {
            "start": { "line": line0, "character": 0 },
            "end": { "line": line0, "character": end },
        },
        "selectionRange": {
            "start": { "line": line0, "character": 0 },
            "end": { "line": line0, "character": end },
        },
    });
    if let Some(data) = data {
        let generated = data["generated"].as_bool().unwrap_or(false);
        let dynamic = data["dynamicDispatch"].as_bool().unwrap_or(false);
        if let Some(detail) = match (generated, dynamic) {
            (true, true) => Some("generated, dynamic dispatch"),
            (true, false) => Some("generated"),
            (false, true) => Some("dynamic dispatch (@dynamic)"),
            (false, false) => None,
        } {
            symbol["detail"] = json!(detail);
        }
        symbol["data"] = data;
    }
    symbol
}

fn item_symbol(item: &ast::Item) -> Option<(&str, u32)> {
    match item {
        ast::Item::Function(function) => Some((&function.name, 12)),
        ast::Item::Type(definition) => Some((&definition.name, 23)),
        ast::Item::Trait(definition) => Some((&definition.name, 11)),
        ast::Item::Const { name, .. } => Some((name, 14)),
        ast::Item::TypeAlias { name, .. } => Some((name, 5)),
        ast::Item::Impl(_) | ast::Item::Comptime(_) => None,
    }
}

fn item_has_dynamic_dispatch(item: &ast::Item) -> bool {
    matches!(
        item,
        ast::Item::Function(function)
            if function.attributes.iter().any(|attribute| attribute == "dynamic")
    )
}

// --- generated definitions ------------------------------------------------

/// Resolve a generated declaration represented at its invocation line to both
/// sides of its expansion boundary. The invocation is useful when a symbol came
/// from a document-symbol result; the definition location opens the macro body.
fn definition_response(docs: &HashMap<String, String>, params: &Value) -> Value {
    let Some(uri) = params["textDocument"]["uri"].as_str() else {
        return Value::Null;
    };
    let Some(text) = docs.get(uri) else {
        return Value::Null;
    };
    let Some(line0) = params["position"]["line"]
        .as_u64()
        .and_then(|line| u32::try_from(line).ok())
    else {
        return Value::Null;
    };
    let Some((entry, _, linked)) = link_document_with_origins(uri, text, docs) else {
        return Value::Null;
    };

    let mut seen = HashSet::new();
    let mut locations = Vec::new();
    for generated in linked.origins.nodes() {
        if generated.node.category != witchy_syntax::origin::SyntaxCategory::Item
            || !generated.node.path.is_empty()
            || generated.origin.invocation.module != entry
            || generated.origin.invocation.start.line.saturating_sub(1) != line0
        {
            continue;
        }
        for span in [&generated.origin.invocation, &generated.origin.definition] {
            let Some(target_uri) = module_uri(uri, &entry, &span.module, docs) else {
                continue;
            };
            let start_line = span.start.line.saturating_sub(1);
            let start_character = span.start.column.saturating_sub(1);
            let end_line = span.end.line.saturating_sub(1);
            let end_character = span.end.column.saturating_sub(1).max(start_character + 1);
            if seen.insert((
                target_uri.clone(),
                start_line,
                start_character,
                end_line,
                end_character,
            )) {
                locations.push(json!({
                    "uri": target_uri,
                    "range": {
                        "start": { "line": start_line, "character": start_character },
                        "end": { "line": end_line, "character": end_character },
                    }
                }));
            }
        }
    }
    if locations.is_empty() { Value::Null } else { json!(locations) }
}

fn module_uri(
    entry_uri: &str,
    entry: &str,
    module: &str,
    docs: &HashMap<String, String>,
) -> Option<String> {
    if module == entry {
        return Some(entry_uri.to_string());
    }
    let entry_path = uri_to_path(entry_uri)?;
    let sibling = entry_path.parent()?.join(format!("{module}.witchy"));
    if let Some(uri) = docs.keys().find(|uri| uri_to_path(uri).as_ref() == Some(&sibling)) {
        return Some(uri.clone());
    }
    sibling.exists().then(|| format!("file://{}", sibling.to_string_lossy()))
}

fn link_document_with_origins(
    uri: &str,
    text: &str,
    docs: &HashMap<String, String>,
) -> Option<(String, ast::Module, witchy_syntax::linker::LinkedModule)> {
    let path = uri_to_path(uri);
    let dir = path.as_ref()?.parent().map(PathBuf::from)?;
    let entry = path.as_ref()?.file_stem()?.to_str()?.to_string();
    let parsed = parser::parse_module(text).ok()?;
    let mut modules = vec![(entry.clone(), parsed.clone())];
    let mut loaded = HashSet::from([entry.clone()]);
    let mut queue: VecDeque<String> = parsed.imports.iter().cloned().collect();
    queue.extend(parsed.from_imports.iter().map(|(name, _)| name.clone()));
    while let Some(name) = queue.pop_front() {
        if !loaded.insert(name.clone()) {
            continue;
        }
        let sibling = dir.join(format!("{name}.witchy"));
        let source = open_buffer(&sibling, docs)
            .or_else(|| std::fs::read_to_string(&sibling).ok())
            .or_else(|| crate::bundled_module(&name).map(str::to_string))?;
        let module = parser::parse_module(&source).ok()?;
        queue.extend(module.imports.iter().filter(|name| !loaded.contains(*name)).cloned());
        queue.extend(
            module
                .from_imports
                .iter()
                .map(|(name, _)| name)
                .filter(|name| !loaded.contains(*name))
                .cloned(),
        );
        modules.push((name, module));
    }
    let linked = witchy_interp::pipeline::link_with_origins(modules, &entry).ok()?;
    Some((entry, parsed, linked))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FunctionDeclaration<'a> {
    signature: &'a str,
    name: &'a str,
    name_start: usize,
    public: bool,
}

/// Parse one source declaration header. Keeping this small indexer shared makes
/// completion, hover, qualification, and diagnostic lookup agree on every
/// function kind even when the rest of the buffer is temporarily invalid.
fn function_declaration(line: &str) -> Option<FunctionDeclaration<'_>> {
    let signature = line.trim_start().trim_end();
    let signature = signature.strip_suffix(':').unwrap_or(signature).trim_end();
    let (public, rest) = signature
        .strip_prefix("pub ")
        .map_or((false, signature), |rest| (true, rest));
    let rest = rest
        .strip_prefix("async fn ")
        .or_else(|| rest.strip_prefix("gen fn "))
        .or_else(|| rest.strip_prefix("fn "))?;
    let name_rest = rest.trim_start();
    let name = name_rest.split_once('(')?.0.trim();
    if name.is_empty() {
        return None;
    }
    Some(FunctionDeclaration {
        signature,
        name,
        name_start: signature.len() - name_rest.len(),
        public,
    })
}

/// Completion items: keywords, builtins, this document's functions, and the
/// `pub fn`s of every prelude/imported module (offered as `module.name`).
fn completion_response(docs: &HashMap<String, String>, params: &Value) -> Value {
    let uri = params["textDocument"]["uri"].as_str();
    let Some(text) = uri.and_then(|u| docs.get(u)) else {
        return json!([]);
    };
    let mut items: Vec<Value> = Vec::new();
    for module in witchy_syntax::linker::PRELUDE_MODULES {
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
        if let Some(declaration) = function_declaration(t) {
            items.push(json!({ "label": declaration.name, "kind": 3 }));
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
            if let Some(declaration) =
                function_declaration(ml).filter(|declaration| declaration.public)
            {
                items.push(json!({
                    "label": format!("{module}.{}", declaration.name),
                    "kind": 3,
                }));
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
    if let Some(src) = witchy_syntax::linker::bundled_source(name) {
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
    let mut associated_owner: Option<String> = None;
    let mut module_qualified = false;
    let mut bare_name = false;
    let bare = match word.split_once('.') {
        Some((head, tail)) => {
            // `head.tail`: `head` is a module (`string.repeat`) or a receiver
            // value (`xs.push`). When it names a module, look there; otherwise
            // treat `tail`'s final segment as a method and resolve it against
            // every visible module (`xs.push` → `list.push`).
            let name = tail.rsplit_once('.').map_or(tail, |(_, n)| n).to_string();
            if let Some(src) = module_source(head, uri, docs) {
                module_qualified = true;
                sources.push((src, format!("{head}.")));
            } else {
                // An uppercase head is a type-owned associated function, not a
                // receiver value. Search the current module and visible modules
                // by AST ownership; never relabel an incidental free function
                // such as policy.tcp as Net.tcp (BUG-161).
                if head.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    associated_owner = Some(head.to_string());
                    sources.push((text.to_string(), String::new()));
                }
                sources.extend(visible_module_sources(text, uri, docs));
            }
            name
        }
        None => {
            bare_name = true;
            sources.push((text.to_string(), String::new()));
            sources.extend(visible_module_sources(text, uri, docs));
            word.clone()
        }
    };
    if let Some(owner) = &associated_owner {
        for (src, _prefix) in &sources {
            if let Ok(Some(symbol)) = witchy_syntax::doc::associated_function(src, owner, &bare) {
                let contents = format!(
                    "```witchy\n{}\n```\n{}",
                    symbol.signature, symbol.docs
                );
                return json!({ "contents": { "kind": "markdown", "value": contents } });
            }
        }
        return Value::Null;
    }
    // A bare name the DOCUMENT ITSELF defines wins outright: a user's own
    // `fn count(...)` must not be shadowed by a std impl-method namesake
    // (String.count). Note a literal receiver (`"ab".repeat`) also reads as a
    // bare word — the quote stops the word scan — so this check must be
    // document-only, leaving std methods to the pass below.
    if bare_name {
        if let Some((src, prefix)) = sources.first() {
            if let Some((sig, doc)) = type_alias_signature_doc(src, &bare) {
                let contents = format!("```witchy\n{sig}\n```\n{doc}");
                return json!({ "contents": { "kind": "markdown", "value": contents } });
            }
            if let Some((sig, doc)) = signature_doc(src, &bare) {
                let contents =
                    format!("```witchy\n{}\n```\n{}", qualify_signature(&sig, prefix), doc);
                return json!({ "contents": { "kind": "markdown", "value": contents } });
            }
        }
    }
    // Methods-first (RFC-0099): an impl instance method owns the operation, so
    // a receiver spelling (`"ab".repeat`, `xs.push`) reports the Type-qualified
    // method before any incidental free-function namesake in another module. An
    // explicit module qualifier (`list.repeat`) still means the module function.
    if !module_qualified {
        for (src, _prefix) in &sources {
            if let Ok(Some(symbol)) = witchy_syntax::doc::instance_method(src, None, &bare) {
                let contents = format!(
                    "```witchy\n{}\n```\n{}",
                    symbol.signature, symbol.docs
                );
                return json!({ "contents": { "kind": "markdown", "value": contents } });
            }
        }
    }
    for (src, prefix) in sources {
        if let Some((sig, doc)) = signature_doc(&src, &bare) {
            let contents = format!("```witchy\n{}\n```\n{}", qualify_signature(&sig, &prefix), doc);
            return json!({ "contents": { "kind": "markdown", "value": contents } });
        }
    }
    Value::Null
}

/// Find a one-line structural alias declaration and its contiguous `//` docs.
/// Type-position record composition is intentionally surfaced in source form:
/// hovering `Detailed` should show `.{..Summary, revision: Int}`, while the
/// compiler's normalized exact anonymous identity remains an implementation
/// detail available through type quotes and diagnostics.
fn type_alias_signature_doc(src: &str, name: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = src.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let signature = line.trim();
        let Some(rest) = signature.strip_prefix("type ") else { continue };
        let Some(declared) = rest
            .split(|character: char| character == '(' || character == '=' || character.is_whitespace())
            .next()
        else {
            continue;
        };
        if declared != name || !rest.contains('=') {
            continue;
        }
        let mut docs = Vec::new();
        for previous in (0..index).rev() {
            let line = lines[previous].trim_start();
            let Some(comment) = line.strip_prefix("//") else { break };
            docs.push(comment.trim());
        }
        docs.reverse();
        return Some((signature.to_string(), docs.join("\n")));
    }
    None
}

/// Render a signature line qualified by `prefix` (e.g. `string.`). The qualifier
/// belongs before the FUNCTION NAME, not in front of the whole line: prepending
/// it produced the malformed `string.pub fn repeat(...)` (and `Net.pub fn tcp`).
/// Inserting it at the parsed name offset preserves ordinary/async/gen modifiers.
fn qualify_signature(sig: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return sig.to_string();
    }
    if let Some(declaration) = function_declaration(sig) {
        return format!(
            "{}{}{}",
            &declaration.signature[..declaration.name_start],
            prefix,
            &declaration.signature[declaration.name_start..]
        );
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

/// Find a function declaration in `src` and return its signature plus preceding
/// contiguous `//` doc block.
fn signature_doc(src: &str, name: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = src.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if let Some(declaration) =
            function_declaration(l).filter(|declaration| declaration.name == name)
        {
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
            let dynamic = source_function_is_dynamic(src, name);
            let signature = if dynamic {
                format!("@dynamic\n{}", declaration.signature)
            } else {
                declaration.signature.to_string()
            };
            let docs = doc_lines.join("\n");
            let docs = if dynamic {
                let note = "**Dynamic dispatch:** registered for closed-world checked invocation.";
                if docs.is_empty() {
                    note.to_string()
                } else {
                    format!("{note}\n\n{docs}")
                }
            } else {
                docs
            };
            return Some((signature, docs));
        }
    }
    None
}

fn source_function_is_dynamic(src: &str, name: &str) -> bool {
    parser::parse_module(src).is_ok_and(|module| {
        module.items.iter().any(|item| {
            matches!(
                item,
                ast::Item::Function(function)
                    if function.name.rsplit('.').next() == Some(name)
                        && function
                            .attributes
                            .iter()
                            .any(|attribute| attribute == "dynamic")
            )
        })
    })
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
    names.extend(witchy_syntax::linker::PRELUDE_MODULES.iter().map(|s| s.to_string()));
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

    let checked = match witchy_interp::pipeline::link_checked(modules, &entry) {
        Ok(checked) => checked,
        Err(witchy_interp::pipeline::PipelineError::Link(error)) => {
            // BUG-162: map the link error onto the line it names (or the import it
            // blames) instead of always pinning it to line 0.
            let message = error.to_string();
            return vec![line_diag(
                link_error_line(&error, text, &entry),
                text,
                &message,
            )];
        }
        Err(witchy_interp::pipeline::PipelineError::Type(error)) => {
            let line0 = extract_line(&error.message).map_or(0, |n| n.saturating_sub(1));
            return vec![line_diag(
                line0,
                text,
                &format!("type error: {}", error.message),
            )];
        }
        Err(error) => {
            let message = error.to_string();
            let line0 = extract_line(&message).map_or(0, |n| n.saturating_sub(1));
            return vec![line_diag(line0, text, &message)];
        }
    };
    let linked = checked.module();
            // A file that declares `mode opt` turns the performance contract into a
            // HARD gate — the same one `witchy check` enforces via
            // `enforce_performance_modes`. Mirror it here so the editor surfaces those
            // errors instead of silently accepting a program `check` rejects (BUG-165).
            let source_modes: Vec<&str> = linked
                .modes
                .iter()
                .filter(|mode| !mode.starts_with('@'))
                .map(String::as_str)
                .collect();
            let enforce = !source_modes.is_empty();
            let modes = source_modes.join(", ");
            let mut diags: Vec<Value> = Vec::new();

            // Copy-cliffs: a plain note normally (Hint, rendered unobtrusively); in a
            // `mode` file the cliff is a hard error. Only the buffer's OWN functions
            // (the entry module's `main` / `{entry}.fn`) are judged; linked-in modules
            // keep their own policy.
            for (func, c) in witchy_lower::analysis::module_cliffs(linked) {
                if !witchy::is_entry_function(&func, &entry) {
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

            if enforce {
                for miss in witchy_lower::analysis::module_no_copy_misses(linked) {
                    if !witchy::is_entry_function(&miss.function, &entry) {
                        continue;
                    }
                    let line0 = miss.line.saturating_sub(1);
                    let advice = if miss.reason.contains("first-class call ABI") {
                        "call it directly so the capacity token stays in the compiled ABI, or use normal mode"
                    } else {
                        "keep the receiver uniquely owned and free of aliases or active loans"
                    };
                    diags.push(line_diag(
                        line0,
                        text,
                        &format!(
                            "`{}` cannot satisfy the no-copy `var` contract of `{}`: {} — `mode {modes}` requires a `unique` or `local unique` ownership proof; {advice}",
                            miss.var, miss.callee, miss.reason,
                        ),
                    ));
                }
                for miss in witchy_lower::analysis::module_fip_misses(linked) {
                    if !witchy::is_entry_function(&miss.function, &entry) {
                        continue;
                    }
                    diags.push(line_diag(
                        miss.line.saturating_sub(1),
                        text,
                        &format!(
                            "functional-in-place contract failed in `{}`: {} — `mode {modes}` requires tail recursion that forwards and returns the `own unique` value",
                            miss.function, miss.reason,
                        ),
                    ));
                }
            }

            // Signature contract (mode files only): every ownership-relevant parameter
            // must declare its convention (`let`/`own`/`var`), so the interprocedural
            // summaries are declared facts. A bare (default) `let` is the violation.
            if enforce {
                for item in &linked.items {
                    let ast::Item::Function(f) = item else { continue };
                    if !witchy::is_entry_function(&f.name, &entry) {
                        continue;
                    }
                    for p in &f.params {
                        if p.convention == ast::Convention::Let && witchy::ownership_relevant(&p.ty) {
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

/// The 0-based line of `text` declaring function `bare`, so a mode-opt
/// signature-contract error can point at ordinary/async/gen functions even
/// though the AST carries no span for declarations.
fn fn_decl_line(text: &str, bare: &str) -> Option<u32> {
    text.lines()
        .position(|line| {
            function_declaration(line).is_some_and(|declaration| declaration.name == bare)
        })
        .map(|i| i as u32)
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
/// relevant import, else the top of file. Link errors otherwise all pinned to
/// line 0 (BUG-162).
fn link_error_line(error: &witchy_syntax::linker::LinkError, text: &str, entry: &str) -> u32 {
    if let Some(location) = &error.location {
        if location.module == entry {
            return location.line.saturating_sub(1);
        }
        if let Some(line0) = import_line_of(text, &location.module) {
            return line0;
        }
    }
    if let Some(n) = extract_line(&error.message) {
        return n.saturating_sub(1);
    }
    // The message may name several things in backticks (e.g. the importing module
    // AND the unknown one); use the first that this file actually imports.
    for name in backtick_tokens(&error.message) {
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
