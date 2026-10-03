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
        "completionProvider": {
            "triggerCharacters": [".", ":"]
        },
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

    for generated in linked.origins().nodes() {
        if generated.origin.invocation.module != entry
            || !generated.node.path.is_empty()
            || generated.node.category != witchy_syntax::origin::SyntaxCategory::Item
        {
            continue;
        }
        let Some(item) = linked.module().items.get(generated.node.item as usize) else { continue };
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
    for generated in linked.origins().nodes() {
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
) -> Option<(String, ast::Module, ToolingLink)> {
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
    let linked = ToolingLink::Plain(witchy_syntax::linker::link_with_origins(
        modules, &entry,
        witchy_syntax::linker::ComptimeExpander::with_sibling_request(
            witchy_types::pipeline::require_expansion_free, |_| false,
        ),
    ).ok()?);
    Some((entry, parsed, linked))
}

enum ToolingLink {
    Plain(witchy_syntax::linker::LinkedModule),
}

impl ToolingLink {
    fn module(&self) -> &ast::Module {
        match self { Self::Plain(linked) => &linked.module }
    }

    fn origins(&self) -> &witchy_syntax::origin::OriginTable {
        match self { Self::Plain(linked) => &linked.origins }
    }
}

// --- completion -------------------------------------------------------------

const KEYWORDS: &[&str] = &[
    "fn", "pure", "once", "async", "gen", "yield", "await", "let", "var", "if", "else", "match",
    "for", "in", "while", "return", "break", "continue", "type", "trait", "impl", "import", "from",
    "pub", "capability", "sealed", "must", "grantable", "unique", "frozen", "local", "own", "move",
    "where", "as", "comptime", "region", "true", "false",
];

const BUILTIN_TYPES: &[(&str, &str)] = &[
    ("Int", "64-bit signed integer"),
    ("Float", "64-bit IEEE 754 floating point number"),
    ("String", "UTF-8 string"),
    ("Bool", "Boolean value (true or false)"),
    ("Bytes", "Byte buffer"),
    ("()", "Unit type and value"),
    ("Duration", "Time duration (e.g. 30s, 250ms)"),
    ("List", "Dynamically sized sequence: List(T) or [T]"),
    ("Dict", "Hash map collection: Dict(K, V)"),
    ("Option", "Optional value: Option(T) = Some(T) | None"),
    ("Result", "Fallible outcome: Result(T, E) = Ok(T) | Err(E)"),
    ("Console", "Standard I/O capability"),
    ("Clock", "System clock and timer capability"),
    ("Dir", "Directory and filesystem access capability"),
    ("File", "File handle capability"),
    ("Net", "Network access capability"),
    ("Rand", "Random number generator capability"),
    ("Env", "Environment variables capability"),
    ("Exec", "Process execution capability"),
    ("SecretStore", "Sealed secret storage capability"),
    ("Secret", "Sealed secret capability"),
];

const CONSTRUCTORS: &[(&str, &str)] = &[
    ("Some", "Some(value) — Option variant holding a value"),
    ("None", "None — Option variant representing absence of value"),
    ("Ok", "Ok(value) — Result variant representing successful outcome"),
    ("Err", "Err(error) — Result variant representing failure"),
    ("()", "() - unit value"),
    ("true", "true — boolean true literal"),
    ("false", "false — boolean false literal"),
];

const BUILTINS: &[(&str, &str)] = &[
    ("print", "fn(console: Console, message: String) -> ()\nPrint message to standard output."),
    ("now", "fn(clock: Clock) -> Int\nCurrent time in milliseconds."),
    ("get_env", "fn(env: Env, key: String) -> Result(Option(String), CapError)\nRead an environment variable: None when unset, Denied outside the grant."),
    ("read", "fn(file: File) -> Result(String, Error)\nRead entire file as string."),
    ("write", "fn(file: File, content: String) -> Result((), Error)\nWrite string to file."),
    ("append", "fn(file: File, content: String) -> Result((), Error)\nAppend string to file."),
    ("exists", "fn(dir: Dir, path: String) -> Bool\nCheck if path exists."),
    ("is_dir", "fn(dir: Dir, path: String) -> Bool\nCheck if path is a directory."),
    ("list", "fn(dir: Dir) -> Result([String], Error)\nList directory entries."),
    ("subtree", "fn(dir: Dir, path: String) -> Dir\nNarrow directory to subtree."),
    ("read_file", "fn(path: String) -> Result(String, Error)\nRead file directly."),
    ("write_file", "fn(path: String, content: String) -> Result((), Error)\nWrite file directly."),
    ("make_dir", "fn(path: String) -> Result((), Error)\nCreate directory."),
    ("exec", "fn(cmd: Exec, command: String, args: [String]) -> Result(String, Error)\nExecute external process."),
    ("connect", "fn(net: Net, address: String) -> Result(NetConn, Error)\nConnect TCP socket."),
    ("listen", "fn(net: Net, address: String) -> Result(NetListener, Error)\nListen on TCP port."),
    ("accept", "fn(listener: NetListener) -> Result(NetConn, Error)\nAccept incoming TCP connection."),
    ("send_line", "fn(conn: NetConn, line: String) -> Result((), Error)\nSend line over connection."),
    ("send_bytes", "fn(conn: NetConn, bytes: Bytes) -> Result((), Error)\nSend bytes over connection."),
    ("recv_line", "fn(conn: NetConn) -> Result(String, Error)\nReceive line from connection."),
    ("recv_all", "fn(conn: NetConn) -> Result(String, Error)\nReceive all data from connection."),
    ("recv_bytes", "fn(conn: NetConn, count: Int) -> Result(Bytes, Error)\nReceive byte count from connection."),
    ("close", "fn(handle: Closeable) -> ()\nClose open handle."),
    ("send", "fn(tx: Sender(T), value: T) -> Future(())\nSend value on channel."),
    ("fail", "fn(message: String) -> Never\nAbort with error message."),
];

const SNIPPETS: &[(&str, &str, &str)] = &[
    ("fn", "fn ${1:name}(${2:params}) -> ${3:Type}:\n    ${0}", "Function definition"),
    ("pure fn", "pure fn ${1:name}(${2:params}) -> ${3:Type}:\n    ${0}", "Pure function definition"),
    ("async fn", "async fn ${1:name}(${2:params}) -> ${3:Type}:\n    ${0}", "Asynchronous function definition"),
    ("gen fn", "gen fn ${1:name}(${2:params}) -> Iter(${3:Type}):\n    ${0}", "Generator function definition"),
    ("match", "match ${1:expr}:\n    ${2:pattern} -> ${0}", "Pattern matching"),
    ("for", "for ${1:item} in ${2:iter}:\n    ${0}", "For-in loop"),
    ("for await", "for await ${1:item} in ${2:stream}:\n    ${0}", "Asynchronous stream iteration"),
    ("if", "if ${1:condition}:\n    ${0}", "If condition"),
    ("if-else", "if ${1:condition}:\n    ${2}\nelse:\n    ${0}", "If-else branch"),
    ("type", "type ${1:Name}:\n    ${0}", "Type definition"),
    ("type alias", "type ${1:Name} = ${2:Type}", "Type alias definition"),
    ("trait", "trait ${1:Name}:\n    ${0}", "Trait definition"),
    ("impl", "impl ${1:Type}:\n    ${0}", "Inherent impl block"),
    ("impl trait", "impl ${1:Trait} for ${2:Type}:\n    ${0}", "Trait impl block"),
    ("capability", "capability ${1:Name}:\n    ${0}", "Capability declaration"),
];

const LIST_METHODS: &[(&str, &str, &str)] = &[
    ("push", "fn(item: T) -> [T]", "Appends an item to the end of the list."),
    ("pop", "fn() -> Option(T)", "Removes and returns the last element, or None if empty."),
    ("length", "fn() -> Int", "Returns the number of elements in the list."),
    ("map", "fn(f: fn(T) -> U) -> [U]", "Applies a transformation function to each element."),
    ("filter", "fn(predicate: fn(T) -> Bool) -> [T]", "Returns elements that satisfy the predicate."),
    ("fold", "fn(initial: U, f: fn(U, T) -> U) -> U", "Reduces elements to a single value using an accumulator."),
    ("contains", "fn(item: T) -> Bool", "Checks whether the list contains the given element."),
    ("at", "fn(index: Int) -> Option(T)", "Returns the element at index, if within bounds."),
    ("join", "fn(separator: String) -> String", "Joins string representations of elements with a separator."),
    ("reverse", "fn() -> [T]", "Returns a new list with elements in reverse order."),
    ("sort", "fn() -> [T]", "Returns a new list with elements sorted in ascending order."),
    ("slice", "fn(start: Int, end: Int) -> [T]", "Returns a sub-slice from start to end index."),
    ("first", "fn() -> Option(T)", "Returns the first element of the list, if non-empty."),
    ("last", "fn() -> Option(T)", "Returns the last element of the list, if non-empty."),
    ("is_empty", "fn() -> Bool", "Returns true if the list contains no elements."),
    ("clear", "fn() -> [T]", "Returns an empty list."),
    ("concat", "fn(other: [T]) -> [T]", "Concatenates another list onto the end."),
];

const STRING_METHODS: &[(&str, &str, &str)] = &[
    ("length", "fn() -> Int", "Returns the number of Unicode characters in the string."),
    ("split", "fn(separator: String) -> [String]", "Splits the string around occurrences of separator."),
    ("lines", "fn() -> [String]", "Splits the string into individual lines."),
    ("trim", "fn() -> String", "Removes leading and trailing whitespace."),
    ("starts_with", "fn(prefix: String) -> Bool", "Tests whether the string begins with prefix."),
    ("ends_with", "fn(suffix: String) -> Bool", "Tests whether the string ends with suffix."),
    ("contains", "fn(needle: String) -> Bool", "Tests whether the string contains needle."),
    ("replace", "fn(from: String, to: String) -> String", "Replaces occurrences of `from` with `to`."),
    ("to_upper", "fn() -> String", "Converts all characters to uppercase."),
    ("to_lower", "fn() -> String", "Converts all characters to lowercase."),
    ("chars", "fn() -> [String]", "Returns a list of single-character strings."),
    ("bytes", "fn() -> Bytes", "Returns the UTF-8 bytes of the string."),
    ("at", "fn(index: Int) -> Option(String)", "Returns the character at character index."),
    ("slice", "fn(start: Int, end: Int) -> String", "Extracts a substring slice from start to end."),
    ("is_empty", "fn() -> Bool", "Returns true if string length is zero."),
    ("repeat", "fn(count: Int) -> String", "Repeats the string count times."),
];

const DICT_METHODS: &[(&str, &str, &str)] = &[
    ("get", "fn(key: K) -> Option(V)", "Returns the value associated with key, if present."),
    ("set", "fn(key: K, value: V) -> Dict(K, V)", "Inserts or updates a key-value pair."),
    ("remove", "fn(key: K) -> Dict(K, V)", "Removes the entry for key."),
    ("contains_key", "fn(key: K) -> Bool", "Checks if key exists in the dictionary."),
    ("keys", "fn() -> [K]", "Returns a list of all keys in the dictionary."),
    ("values", "fn() -> [V]", "Returns a list of all values in the dictionary."),
    ("items", "fn() -> [(K, V)]", "Returns key-value pairs as a list of tuples."),
    ("length", "fn() -> Int", "Returns the number of entries in the dictionary."),
    ("is_empty", "fn() -> Bool", "Returns true if dictionary has no entries."),
    ("clear", "fn() -> Dict(K, V)", "Removes all entries from dictionary."),
];

const OPTION_METHODS: &[(&str, &str, &str)] = &[
    ("unwrap", "fn() -> T", "Unwraps the Some value, aborting if None."),
    ("unwrap_or", "fn(default: T) -> T", "Unwraps the Some value or returns default if None."),
    ("is_some", "fn() -> Bool", "Returns true if Option is Some."),
    ("is_none", "fn() -> Bool", "Returns true if Option is None."),
    ("map", "fn(f: fn(T) -> U) -> Option(U)", "Applies f to contained value if Some."),
    ("and_then", "fn(f: fn(T) -> Option(U)) -> Option(U)", "Chains an Option-producing computation."),
];

const RESULT_METHODS: &[(&str, &str, &str)] = &[
    ("unwrap", "fn() -> T", "Unwraps the Ok value, aborting with error if Err."),
    ("unwrap_or", "fn(default: T) -> T", "Unwraps the Ok value or returns default if Err."),
    ("is_ok", "fn() -> Bool", "Returns true if Result is Ok."),
    ("is_err", "fn() -> Bool", "Returns true if Result is Err."),
    ("map", "fn(f: fn(T) -> U) -> Result(U, E)", "Applies f to contained value if Ok."),
    ("map_err", "fn(f: fn(E) -> F) -> Result(T, F)", "Applies f to contained error if Err."),
    ("and_then", "fn(f: fn(T) -> Result(U, E)) -> Result(U, E)", "Chains a Result-producing computation."),
];

const CAPABILITY_CONSOLE_METHODS: &[(&str, &str, &str)] = &[
    ("print", "fn(message: String) -> ()", "Prints message to standard output followed by newline."),
    ("input", "fn() -> String", "Reads a line of text from standard input."),
];

const CAPABILITY_CLOCK_METHODS: &[(&str, &str, &str)] = &[
    ("now", "fn() -> Int", "Returns current system time in milliseconds since Unix epoch."),
    ("sleep", "fn(duration: Duration) -> ()", "Pauses execution for the specified duration."),
];

const CAPABILITY_DIR_METHODS: &[(&str, &str, &str)] = &[
    ("read", "fn(path: String) -> Result(String, CapError)", "Reads contents of a file at path within directory."),
    ("read_bytes", "fn(path: String) -> Result(Bytes, CapError)", "Reads binary contents of a file at path."),
    ("write", "fn(path: String, content: String) -> Result((), CapError)", "Writes content to file at path."),
    ("write_bytes", "fn(path: String, content: Bytes) -> Result((), CapError)", "Writes binary data to file at path."),
    ("append", "fn(path: String, content: String) -> Result((), CapError)", "Appends content to file at path."),
    ("append_bytes", "fn(path: String, content: Bytes) -> Result((), CapError)", "Appends binary data to file at path."),
    ("exists", "fn(path: String) -> Result(Bool, CapError)", "Whether path names an entry this capability can see."),
    ("is_dir", "fn(path: String) -> Result(Bool, CapError)", "Whether path names a subdirectory this capability can see."),
    ("list", "fn() -> Result(List(String), CapError)", "Lists entry names in this directory."),
    ("subtree", "fn(subpath: String) -> Result(Dir, CapError)", "Narrows directory capability to a sub-path."),
    ("make_dir", "fn(path: String) -> Result((), CapError)", "Creates a directory at path."),
    ("create_new", "fn(path: String, content: String) -> Result((), CapError)", "Creates a new file; a taken name is AlreadyExists."),
    ("replace", "fn(path: String, content: String) -> Result((), CapError)", "Atomically replaces file contents."),
    ("rename", "fn(from: String, to: String) -> Result((), CapError)", "Renames or moves a file or directory."),
    ("set_executable", "fn(path: String) -> Result((), CapError)", "Sets executable permission on file."),
    ("only", "fn(policy: DirPolicy) -> Dir", "Narrows directory capability to permitted paths."),
];

const CAPABILITY_EXEC_METHODS: &[(&str, &str, &str)] = &[
    ("exec", "fn(dir: Dir, path: String, args: String, stdin: String) -> Result(String, Error)", "Executes binary within directory capability."),
    ("only", "fn(programs: [String]) -> Exec", "Narrows exec capability to allowlisted programs."),
];

const CAPABILITY_SECRET_METHODS: &[(&str, &str, &str)] = &[
    ("reveal", "fn() -> Bytes", "Reveals raw secret bytes (requires Reveal right)."),
    ("seal", "fn() -> Secret", "Seals secret for handle-only usage."),
];

const CAPABILITY_FILE_METHODS: &[(&str, &str, &str)] = &[
    ("read", "fn() -> Result(String, Error)", "Reads entire file contents as UTF-8 string."),
    ("write", "fn(content: String) -> Result((), Error)", "Overwrites file with content."),
    ("append", "fn(content: String) -> Result((), Error)", "Appends content to file."),
    ("read_bytes", "fn() -> Result(Bytes, Error)", "Reads entire file contents as bytes."),
    ("write_bytes", "fn(content: Bytes) -> Result((), Error)", "Writes bytes to file."),
    ("close", "fn() -> ()", "Closes open file handle."),
];

const CAPABILITY_NET_METHODS: &[(&str, &str, &str)] = &[
    ("connect", "fn(address: String) -> Result(NetConn, Error)", "Connects TCP socket to address."),
    ("listen", "fn(address: String) -> Result(NetListener, Error)", "Binds TCP listening socket."),
    ("only", "fn(policy: NetPolicy) -> Net", "Narrows net capability to permitted destinations."),
    ("deny", "fn(policy: NetPolicy) -> Net", "Narrows net capability by denying destinations."),
];

const CAPABILITY_RAND_METHODS: &[(&str, &str, &str)] = &[
    ("int", "fn(min: Int, max: Int) -> Int", "Generates pseudo-random integer in [min, max]."),
    ("float", "fn() -> Float", "Generates pseudo-random float in [0.0, 1.0)."),
    ("bytes", "fn(count: Int) -> Bytes", "Generates random byte buffer of length count."),
];

const CAPABILITY_ENV_METHODS: &[(&str, &str, &str)] = &[
    ("get", "fn(key: String) -> Option(String)", "Reads environment variable value."),
    ("var", "fn(key: String) -> Option(String)", "Reads environment variable value."),
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
        .map_or((false, signature), |rest| (true, rest.trim_start()));
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix("pure fn ")
        .or_else(|| rest.strip_prefix("async fn "))
        .or_else(|| rest.strip_prefix("gen fn "))
        .or_else(|| rest.strip_prefix("comptime fn "))
        .or_else(|| rest.strip_prefix("once fn "))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TypeDeclaration<'a> {
    name: &'a str,
    kind: &'a str,
}

fn type_declaration(line: &str) -> Option<TypeDeclaration<'_>> {
    let s = line.trim_start().trim_end();
    let s = s.strip_prefix("pub ").unwrap_or(s);
    let s = s.strip_prefix("sealed ").unwrap_or(s);
    let s = s.strip_prefix("must ").unwrap_or(s);
    let s = s.strip_prefix("grantable ").unwrap_or(s);
    let s = s.trim_start();
    if let Some(rest) = s.strip_prefix("type ") {
        let name_part = rest
            .split_once(['(', ':', '=', ' '])
            .map_or(rest, |(n, _)| n)
            .trim();
        if !name_part.is_empty() {
            return Some(TypeDeclaration {
                name: name_part,
                kind: "type",
            });
        }
    } else if let Some(rest) = s.strip_prefix("capability ") {
        let name_part = rest
            .split_once(['(', ':', '=', ' '])
            .map_or(rest, |(n, _)| n)
            .trim();
        if !name_part.is_empty() {
            return Some(TypeDeclaration {
                name: name_part,
                kind: "capability",
            });
        }
    } else if let Some(rest) = s.strip_prefix("trait ") {
        let name_part = rest
            .split_once(['(', ':', '=', ' '])
            .map_or(rest, |(n, _)| n)
            .trim();
        if !name_part.is_empty() {
            return Some(TypeDeclaration {
                name: name_part,
                kind: "trait",
            });
        }
    }
    None
}

#[derive(Debug, Clone)]
struct ScopeVar<'a> {
    name: &'a str,
    ty: Option<&'a str>,
}

fn extract_scope_vars<'a>(text: &'a str, cursor_line: usize) -> Vec<ScopeVar<'a>> {
    let mut vars = Vec::new();
    let lines: Vec<&'a str> = text.lines().collect();
    if cursor_line >= lines.len() {
        return vars;
    }

    let mut fn_line_idx = None;
    for i in (0..=cursor_line).rev() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if function_declaration(trimmed).is_some() {
            fn_line_idx = Some(i);
            break;
        }
    }

    let Some(header_idx) = fn_line_idx else {
        return vars;
    };

    let header_line = lines[header_idx];
    if let Some(open_paren) = header_line.find('(') {
        if let Some(close_paren) = header_line[open_paren..].find(')') {
            let params_str = &header_line[open_paren + 1..open_paren + close_paren];
            for param_chunk in params_str.split(',') {
                let p = param_chunk.trim();
                if p.is_empty() {
                    continue;
                }
                let clean = p
                    .strip_prefix("own ")
                    .or_else(|| p.strip_prefix("var "))
                    .or_else(|| p.strip_prefix("let "))
                    .or_else(|| p.strip_prefix("inout "))
                    .or_else(|| p.strip_prefix("sink "))
                    .unwrap_or(p)
                    .trim_start();
                if let Some((name, ty)) = clean.split_once(':') {
                    vars.push(ScopeVar {
                        name: name.trim(),
                        ty: Some(ty.trim()),
                    });
                } else {
                    vars.push(ScopeVar {
                        name: clean.trim(),
                        ty: None,
                    });
                }
            }
        }
    }

    for i in (header_idx + 1)..cursor_line {
        let t = lines[i].trim_start();
        if let Some(rest) = t.strip_prefix("let ").or_else(|| t.strip_prefix("var ")) {
            let rest = rest.trim_start();
            if let Some((left, right)) = rest.split_once('=') {
                let right = right.trim();
                let inferred_ty = if right.starts_with('[') {
                    Some("[T]")
                } else if right.starts_with('"') || right.starts_with("f\"") {
                    Some("String")
                } else if right.starts_with('{') {
                    Some("Dict")
                } else if right.starts_with("Some(") || right == "None" {
                    Some("Option")
                } else if right.starts_with("Ok(") || right.starts_with("Err(") {
                    Some("Result")
                } else {
                    None
                };

                if let Some((name, ty)) = left.split_once(':') {
                    vars.push(ScopeVar {
                        name: name.trim(),
                        ty: Some(ty.trim()),
                    });
                } else {
                    let name = left.trim();
                    if !name.is_empty() && !name.starts_with('(') {
                        vars.push(ScopeVar {
                            name,
                            ty: inferred_ty,
                        });
                    }
                }
            }
        } else if let Some(rest) = t.strip_prefix("for ") {
            if let Some((item, _)) = rest.split_once(" in ") {
                let name = item.trim();
                if !name.is_empty() {
                    vars.push(ScopeVar { name, ty: None });
                }
            }
        }
    }

    vars
}

enum CompletionContext<'a> {
    Dot {
        receiver: &'a str,
        _partial: &'a str,
    },
    TypeAnnotation {
        _partial: &'a str,
    },
    Import {
        _partial: &'a str,
    },
    FromImport {
        module: &'a str,
        _partial: &'a str,
    },
    General {
        _partial: &'a str,
        line: Option<usize>,
    },
}

fn extract_receiver(s: &str) -> &str {
    let s = s.trim_end();
    if s.ends_with(')') {
        let mut depth = 0;
        let mut start_idx = 0;
        for (i, c) in s.char_indices().rev() {
            if c == ')' {
                depth += 1;
            } else if c == '(' {
                depth -= 1;
                if depth == 0 {
                    start_idx = i;
                    break;
                }
            }
        }
        let before_paren = &s[..start_idx];
        let mut id_len = 0;
        for c in before_paren.chars().rev() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                id_len += c.len_utf8();
            } else {
                break;
            }
        }
        &s[start_idx - id_len..]
    } else if s.ends_with(']') {
        let mut depth = 0;
        let mut start_idx = 0;
        for (i, c) in s.char_indices().rev() {
            if c == ']' {
                depth += 1;
            } else if c == '[' {
                depth -= 1;
                if depth == 0 {
                    start_idx = i;
                    break;
                }
            }
        }
        let before_bracket = &s[..start_idx];
        let mut id_len = 0;
        for c in before_bracket.chars().rev() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                id_len += c.len_utf8();
            } else {
                break;
            }
        }
        &s[start_idx - id_len..]
    } else {
        let mut id_len = 0;
        for c in s.chars().rev() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                id_len += c.len_utf8();
            } else {
                break;
            }
        }
        &s[s.len() - id_len..]
    }
}

fn detect_completion_context<'a>(
    text: &'a str,
    position: Option<(usize, usize)>,
) -> CompletionContext<'a> {
    let Some((line_idx, char_idx)) = position else {
        return CompletionContext::General {
            _partial: "",
            line: None,
        };
    };

    let Some(line_text) = text.lines().nth(line_idx) else {
        return CompletionContext::General {
            _partial: "",
            line: Some(line_idx),
        };
    };

    let byte_idx = line_text
        .char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(line_text.len());
    let prefix = &line_text[..byte_idx];
    let trimmed_prefix = prefix.trim_start();

    if trimmed_prefix.starts_with("import ") {
        let partial = trimmed_prefix.strip_prefix("import ").unwrap_or("").trim_start();
        return CompletionContext::Import { _partial: partial };
    }
    if trimmed_prefix.starts_with("from ") && trimmed_prefix.contains(" import ") {
        let after_from = trimmed_prefix.strip_prefix("from ").unwrap_or("");
        if let Some((mod_name, rest)) = after_from.split_once(" import ") {
            let partial = rest.rsplit(',').next().unwrap_or(rest).trim_start();
            return CompletionContext::FromImport {
                module: mod_name.trim(),
                _partial: partial,
            };
        }
    }

    let end_trimmed = prefix.trim_end();
    let mut partial_len = 0;
    for c in end_trimmed.chars().rev() {
        if c.is_ascii_alphanumeric() || c == '_' {
            partial_len += c.len_utf8();
        } else {
            break;
        }
    }
    let partial_start = end_trimmed.len() - partial_len;
    let partial = &end_trimmed[partial_start..];
    let before_partial = &end_trimmed[..partial_start];

    if before_partial.ends_with('.') {
        let before_dot = before_partial.strip_suffix('.').unwrap_or("").trim_end();
        let is_float = !before_dot.is_empty()
            && before_dot.chars().all(|c| c.is_ascii_digit())
            && partial.chars().all(|c| c.is_ascii_digit())
            && !partial.is_empty();
        if !is_float && !before_dot.is_empty() {
            let receiver = extract_receiver(before_dot);
            if !receiver.is_empty() {
                return CompletionContext::Dot {
                    receiver,
                    _partial: partial,
                };
            }
        }
    }

    let clean = prefix.trim_end();
    if clean.ends_with(':')
        || clean.ends_with("->")
        || clean.ends_with(" as")
        || clean.ends_with("type ")
    {
        return CompletionContext::TypeAnnotation { _partial: "" };
    }
    if let Some((_before, after_colon)) = clean.rsplit_once(':') {
        let after_trim = after_colon.trim_start();
        if !after_trim.is_empty()
            && after_trim.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && (clean.contains("fn ")
                || clean.contains("let ")
                || clean.contains("var ")
                || clean.contains("type ")
                || clean.contains("capability "))
        {
            return CompletionContext::TypeAnnotation {
                _partial: after_trim,
            };
        }
    }
    if let Some((_before, after_arrow)) = clean.rsplit_once("->") {
        let after_trim = after_arrow.trim_start();
        if !after_trim.is_empty()
            && after_trim.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return CompletionContext::TypeAnnotation {
                _partial: after_trim,
            };
        }
    }

    CompletionContext::General {
        _partial: partial,
        line: Some(line_idx),
    }
}

fn is_module_name(
    name: &str,
    text: &str,
    uri: Option<&str>,
    docs: &HashMap<String, String>,
) -> bool {
    witchy_syntax::linker::PRELUDE_MODULES.contains(&name)
        || witchy_syntax::linker::STD_MODULES.contains(&name)
        || text.lines().any(|l| {
            let t = l.trim();
            t == format!("import {name}")
                || t.starts_with(&format!("import {name} "))
                || t.starts_with(&format!("from {name} import "))
        })
        || module_source(name, uri, docs).is_some()
}

fn push_completion_item(
    items: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    label: &str,
    kind: u32,
    detail: Option<&str>,
    doc: Option<&str>,
    insert_text: Option<&str>,
    is_snippet: bool,
    sort_priority: char,
) {
    let key = if is_snippet {
        format!("snippet:{label}")
    } else {
        label.to_string()
    };
    if !seen.insert(key) {
        return;
    }
    let mut obj = serde_json::Map::new();
    obj.insert("label".to_string(), json!(label));
    obj.insert("kind".to_string(), json!(kind));
    if let Some(d) = detail {
        obj.insert("detail".to_string(), json!(d));
    }
    if let Some(dc) = doc {
        if !dc.is_empty() {
            obj.insert(
                "documentation".to_string(),
                json!({ "kind": "markdown", "value": dc }),
            );
        }
    }
    if let Some(it) = insert_text {
        obj.insert("insertText".to_string(), json!(it));
    }
    if is_snippet {
        obj.insert("insertTextFormat".to_string(), json!(2));
    }
    obj.insert(
        "sortText".to_string(),
        json!(format!("{sort_priority}_{label}")),
    );
    items.push(Value::Object(obj));
}

/// Completion items: context-aware suggestions for dot/receiver access,
/// type annotations, imports, local variables, functions, and language snippets.
fn completion_response(docs: &HashMap<String, String>, params: &Value) -> Value {
    let uri = params["textDocument"]["uri"].as_str();
    let Some(text) = uri.and_then(|u| docs.get(u)) else {
        return json!([]);
    };

    let position = params.get("position").and_then(|p| {
        let line = p.get("line")?.as_u64()? as usize;
        let character = p.get("character")?.as_u64()? as usize;
        Some((line, character))
    });

    let context = detect_completion_context(text, position);
    let mut items: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    match context {
        CompletionContext::Dot { receiver, .. } => {
            // 1. Module dot completion: math., list., string., helper., etc.
            if is_module_name(receiver, text, uri, docs) {
                if let Some(src) = module_source(receiver, uri, docs) {
                    let lines: Vec<&str> = src.lines().collect();
                    for (idx, line) in lines.iter().enumerate() {
                        if let Some(decl) = function_declaration(line).filter(|d| d.public) {
                            let mut doc_lines = Vec::new();
                            for j in (0..idx).rev() {
                                let dt = lines[j].trim_start();
                                if let Some(c) = dt.strip_prefix("//") {
                                    doc_lines.push(c.trim());
                                } else {
                                    break;
                                }
                            }
                            doc_lines.reverse();
                            let doc = if doc_lines.is_empty() {
                                None
                            } else {
                                Some(doc_lines.join("\n"))
                            };

                            push_completion_item(
                                &mut items,
                                &mut seen,
                                decl.name,
                                3, // Function
                                Some(decl.signature),
                                doc.as_deref(),
                                Some(decl.name),
                                false,
                                '0',
                            );
                        }
                        if let Some(type_decl) = type_declaration(line) {
                            push_completion_item(
                                &mut items,
                                &mut seen,
                                type_decl.name,
                                7, // Class
                                Some(type_decl.kind),
                                None,
                                Some(type_decl.name),
                                false,
                                '1',
                            );
                        }
                    }
                }
                return json!(items);
            }

            // 2. Capability operations: console., clock., dir., file., net., rand., env., exec., secret.
            let vars = position.map_or_else(Vec::new, |(l, _)| extract_scope_vars(text, l));
            let receiver_ty = vars.iter().find(|v| v.name == receiver).and_then(|v| v.ty);

            let (base_ty, rights) = receiver_ty.map_or(("", None), |ty| {
                let trimmed = ty.trim();
                if let Some((base, r)) = trimmed.split_once('[') {
                    (base.trim(), Some(r.strip_suffix(']').unwrap_or(r).trim()))
                } else if let Some((base, _)) = trimmed.split_once('(') {
                    (base.trim(), None)
                } else {
                    (trimmed, None)
                }
            });

            let is_console = receiver == "console" || base_ty == "Console";
            let is_clock = receiver == "clock" || base_ty == "Clock";
            let is_dir = receiver == "dir" || receiver == "root" || receiver == "cwd" || base_ty == "Dir";
            let is_file = receiver == "file" || base_ty == "File";
            let is_net = receiver == "net" || base_ty == "Net";
            let is_rand = receiver == "rand" || base_ty == "Rand";
            let is_env = receiver == "env" || base_ty == "Env";
            let is_exec = receiver == "exec" || receiver == "runner" || base_ty == "Exec";
            let is_secret = receiver == "secret" || base_ty == "Secret";

            if is_console {
                for (name, sig, doc) in CAPABILITY_CONSOLE_METHODS {
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '0');
                }
                return json!(items);
            }
            if is_clock {
                for (name, sig, doc) in CAPABILITY_CLOCK_METHODS {
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '0');
                }
                return json!(items);
            }
            if is_dir {
                let has_write = rights.map_or(true, |r| r.contains("Write"));
                for (name, sig, doc) in CAPABILITY_DIR_METHODS {
                    let is_write_op = matches!(*name, "write" | "write_bytes" | "append" | "append_bytes" | "make_dir" | "create_new" | "replace" | "rename" | "set_executable");
                    let sort_p = if is_write_op && !has_write { '1' } else { '0' };
                    let doc_text = if is_write_op && !has_write {
                        format!("{doc}\n\n*Requires `Dir[Write]` capability right.*")
                    } else {
                        doc.to_string()
                    };
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(&doc_text), Some(name), false, sort_p);
                }
                return json!(items);
            }
            if is_file {
                let has_write = rights.map_or(true, |r| r.contains("Write"));
                for (name, sig, doc) in CAPABILITY_FILE_METHODS {
                    let is_write_op = matches!(*name, "write" | "write_bytes" | "append" | "close");
                    let sort_p = if is_write_op && !has_write { '1' } else { '0' };
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, sort_p);
                }
                return json!(items);
            }
            if is_net {
                for (name, sig, doc) in CAPABILITY_NET_METHODS {
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '0');
                }
                return json!(items);
            }
            if is_rand {
                for (name, sig, doc) in CAPABILITY_RAND_METHODS {
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '0');
                }
                return json!(items);
            }
            if is_env {
                for (name, sig, doc) in CAPABILITY_ENV_METHODS {
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '0');
                }
                return json!(items);
            }
            if is_exec {
                for (name, sig, doc) in CAPABILITY_EXEC_METHODS {
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '0');
                }
                return json!(items);
            }
            if is_secret {
                for (name, sig, doc) in CAPABILITY_SECRET_METHODS {
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '0');
                }
                return json!(items);
            }

            // 3. Enum/Option/Result constructors: Option.Some, Option.None, Result.Ok, Result.Err
            if receiver == "Option" {
                push_completion_item(&mut items, &mut seen, "Some", 4, Some("Some(value)"), Some("Option constructor"), Some("Some"), false, '0');
                push_completion_item(&mut items, &mut seen, "None", 4, Some("None"), Some("Option None variant"), Some("None"), false, '0');
                return json!(items);
            }
            if receiver == "Result" {
                push_completion_item(&mut items, &mut seen, "Ok", 4, Some("Ok(value)"), Some("Result Ok variant"), Some("Ok"), false, '0');
                push_completion_item(&mut items, &mut seen, "Err", 4, Some("Err(error)"), Some("Result Err variant"), Some("Err"), false, '0');
                return json!(items);
            }
            if receiver == "Net" {
                push_completion_item(&mut items, &mut seen, "tcp", 3, Some("fn(host: String, port: Int) -> NetPolicy"), Some("Constructs TCP network policy."), Some("tcp"), false, '0');
                push_completion_item(&mut items, &mut seen, "udp", 3, Some("fn(host: String, port: Int) -> NetPolicy"), Some("Constructs UDP network policy."), Some("udp"), false, '0');
                push_completion_item(&mut items, &mut seen, "private", 3, Some("fn() -> NetPolicy"), Some("Policy matching private network addresses."), Some("private"), false, '0');
                return json!(items);
            }

            // 4. Instance methods on typed variables / receivers
            let is_string = base_ty == "String";
            let is_list = receiver_ty.is_some_and(|t| t.trim().starts_with('[')) || base_ty == "List";
            let is_dict = base_ty == "Dict";
            let is_opt = base_ty == "Option";
            let is_res = base_ty == "Result";

            if is_string {
                for (name, sig, doc) in STRING_METHODS {
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '0');
                }
                return json!(items);
            }
            if is_list {
                for (name, sig, doc) in LIST_METHODS {
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '0');
                }
                return json!(items);
            }
            if is_dict {
                for (name, sig, doc) in DICT_METHODS {
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '0');
                }
                return json!(items);
            }
            if is_opt {
                for (name, sig, doc) in OPTION_METHODS {
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '0');
                }
                return json!(items);
            }
            if is_res {
                for (name, sig, doc) in RESULT_METHODS {
                    push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '0');
                }
                return json!(items);
            }

            // 5. Document impl methods matching receiver owner or general receiver methods
            let doc_lines: Vec<&str> = text.lines().collect();
            let mut current_impl_owner: Option<&str> = None;
            for (i, line) in doc_lines.iter().enumerate() {
                let trimmed = line.trim_start();
                if line.starts_with(' ') || line.starts_with('\t') {
                    if let Some(decl) = function_declaration(trimmed) {
                        let mut doc_lines_fn = Vec::new();
                        for j in (0..i).rev() {
                            let dt = doc_lines[j].trim_start();
                            if let Some(c) = dt.strip_prefix("//") {
                                doc_lines_fn.push(c.trim());
                            } else {
                                break;
                            }
                        }
                        doc_lines_fn.reverse();
                        let doc = if doc_lines_fn.is_empty() { None } else { Some(doc_lines_fn.join("\n")) };

                        let matches_owner = current_impl_owner.is_some_and(|o| o == receiver || base_ty == o);
                        let sort_p = if matches_owner { '0' } else { '2' };
                        push_completion_item(&mut items, &mut seen, decl.name, 2, Some(decl.signature), doc.as_deref(), Some(decl.name), false, sort_p);
                    }
                } else if let Some(rest) = trimmed.strip_prefix("impl ") {
                    let owner_part = if let Some((_, type_part)) = rest.split_once(" for ") {
                        type_part.split_once([':', ' ', '(']).map_or(type_part, |(n, _)| n).trim()
                    } else {
                        rest.split_once([':', ' ', '(']).map_or(rest, |(n, _)| n).trim()
                    };
                    current_impl_owner = Some(owner_part);
                } else {
                    current_impl_owner = None;
                }
            }

            // Fallback for general receivers: offer standard collection and string methods
            for (name, sig, doc) in LIST_METHODS {
                push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '1');
            }
            for (name, sig, doc) in STRING_METHODS {
                push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '1');
            }
            for (name, sig, doc) in DICT_METHODS {
                push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '1');
            }
            for (name, sig, doc) in OPTION_METHODS {
                push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '1');
            }
            for (name, sig, doc) in RESULT_METHODS {
                push_completion_item(&mut items, &mut seen, name, 2, Some(sig), Some(doc), Some(name), false, '1');
            }
            json!(items)
        }

        CompletionContext::TypeAnnotation { .. } => {
            // Built-in types
            for (name, doc) in BUILTIN_TYPES {
                push_completion_item(&mut items, &mut seen, name, 7, Some("type"), Some(doc), Some(name), false, '0');
            }
            // Types declared in document
            for line in text.lines() {
                if let Some(td) = type_declaration(line) {
                    let kind_num = if td.kind == "trait" { 8 } else { 7 };
                    push_completion_item(&mut items, &mut seen, td.name, kind_num, Some(td.kind), None, Some(td.name), false, '1');
                }
                if let Some((_, names)) = parse_from_import(line.trim_start()) {
                    for name in names {
                        if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                            push_completion_item(&mut items, &mut seen, &name, 7, Some("imported type"), None, Some(&name), false, '2');
                        }
                    }
                }
            }
            // Conventions
            for conv in &["own", "var", "let", "inout", "dyn"] {
                push_completion_item(&mut items, &mut seen, conv, 14, Some("convention"), None, Some(conv), false, '3');
            }
            json!(items)
        }

        CompletionContext::Import { .. } => {
            for m in witchy_syntax::linker::STD_MODULES {
                push_completion_item(&mut items, &mut seen, m, 9, Some("standard module"), None, Some(m), false, '0');
            }
            for m in &["glamour", "markdown", "highlight"] {
                push_completion_item(&mut items, &mut seen, m, 9, Some("playground module"), None, Some(m), false, '1');
            }
            json!(items)
        }

        CompletionContext::FromImport { module, .. } => {
            if let Some(src) = module_source(module, uri, docs) {
                for line in src.lines() {
                    if let Some(decl) = function_declaration(line).filter(|d| d.public) {
                        push_completion_item(&mut items, &mut seen, decl.name, 3, Some(decl.signature), None, Some(decl.name), false, '0');
                    }
                    if let Some(td) = type_declaration(line) {
                        let kind_num = if td.kind == "trait" { 8 } else { 7 };
                        push_completion_item(&mut items, &mut seen, td.name, kind_num, Some(td.kind), None, Some(td.name), false, '1');
                    }
                }
            }
            json!(items)
        }

        CompletionContext::General { line, .. } => {
            // 1. Local scope variables and parameters
            if let Some(l) = line {
                let vars = extract_scope_vars(text, l);
                for v in vars {
                    let detail = v.ty.map_or_else(|| "local variable".to_string(), |t| format!(": {t}"));
                    push_completion_item(&mut items, &mut seen, v.name, 6, Some(&detail), None, Some(v.name), false, '0');
                }
            }

            // 2. Document functions, types, traits, capabilities
            let doc_lines: Vec<&str> = text.lines().collect();
            for (idx, line) in doc_lines.iter().enumerate() {
                let t = line.trim_start();
                if let Some(declaration) = function_declaration(t) {
                    let mut doc_lines_fn = Vec::new();
                    for j in (0..idx).rev() {
                        let dt = doc_lines[j].trim_start();
                        if let Some(c) = dt.strip_prefix("//") {
                            doc_lines_fn.push(c.trim());
                        } else {
                            break;
                        }
                    }
                    doc_lines_fn.reverse();
                    let doc = if doc_lines_fn.is_empty() { None } else { Some(doc_lines_fn.join("\n")) };
                    push_completion_item(&mut items, &mut seen, declaration.name, 3, Some(declaration.signature), doc.as_deref(), Some(declaration.name), false, '1');
                }
                if let Some(td) = type_declaration(t) {
                    let kind_num = if td.kind == "trait" { 8 } else { 7 };
                    push_completion_item(&mut items, &mut seen, td.name, kind_num, Some(td.kind), None, Some(td.name), false, '1');
                }
                if let Some(module) = t.strip_prefix("import ") {
                    push_module_completions(&mut items, module.trim(), uri, docs);
                }
                if let Some((module, names)) = parse_from_import(t) {
                    for name in names {
                        push_completion_item(&mut items, &mut seen, &name, 3, Some("imported symbol"), None, Some(&name), false, '2');
                    }
                    push_module_completions(&mut items, &module, uri, docs);
                }
            }

            // 3. Prelude modules and qualified functions
            for module in witchy_syntax::linker::PRELUDE_MODULES {
                push_completion_item(&mut items, &mut seen, module, 9, Some("prelude module"), None, Some(module), false, '3');
                push_module_completions(&mut items, module, None, docs);
            }

            // 4. Constructors & Built-in types
            for (c, doc) in CONSTRUCTORS {
                push_completion_item(&mut items, &mut seen, c, 4, Some("constructor"), Some(doc), Some(c), false, '2');
            }
            for (t, doc) in BUILTIN_TYPES {
                push_completion_item(&mut items, &mut seen, t, 7, Some("type"), Some(doc), Some(t), false, '2');
            }

            // 5. Built-in capability operations and functions
            for (b, doc) in BUILTINS {
                push_completion_item(&mut items, &mut seen, b, 3, Some("builtin"), Some(doc), Some(b), false, '3');
            }

            // 6. Keywords
            for k in KEYWORDS {
                push_completion_item(&mut items, &mut seen, k, 14, Some("keyword"), None, Some(k), false, '4');
            }

            // 7. Snippets
            for (label, body, desc) in SNIPPETS {
                push_completion_item(&mut items, &mut seen, label, 15, Some(desc), None, Some(body), true, '5');
            }

            json!(items)
        }
    }
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

    let checked = match witchy_types::pipeline::link_checked(
        modules, &entry,
        witchy_syntax::linker::ComptimeExpander::with_sibling_request(
            witchy_types::pipeline::require_expansion_free, |_| false,
        ),
    ) {
        Ok(checked) => checked,
        Err(witchy_types::pipeline::PipelineError::Link(error)) => {
            // BUG-162: map the link error onto the line it names (or the import it
            // blames) instead of always pinning it to line 0.
            let message = error.to_string();
            return vec![line_diag(
                link_error_line(&error, text, &entry),
                text,
                &message,
            )];
        }
        Err(witchy_types::pipeline::PipelineError::Type(error)) => {
            let line0 = extract_line(&error.message).map_or(0, |n| n.saturating_sub(1));
            return vec![line_diag(
                line0,
                text,
                &format!("type error: {}", error.message),
            )];
        }
        Err(witchy_types::pipeline::PipelineError::Source(error)) => {
            // Complete body checking now runs at the source-proof boundary, so
            // semantic diagnostics that historically arrived as `Type` arrive
            // here before destructive lowering. Preserve the LSP's stable
            // user-facing category and prefer the structured source location.
            let line0 = error
                .location
                .as_ref()
                .map(|location| location.line.saturating_sub(1))
                .or_else(|| extract_line(&error.message).map(|line| line.saturating_sub(1)))
                .unwrap_or(0);
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
                        "call it directly so the checked capacity state stays in the compiled ABI, or use normal mode"
                    } else {
                        "keep the receiver uniquely owned and free of aliases or active loans"
                    };
                    diags.push(line_diag(
                        line0,
                        text,
                        &format!(
                            "`{}` cannot satisfy the no-copy `var` contract of `{}`: {} - `mode {modes}` requires a `unique` ownership proof; {advice}",
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
            // summaries are declared facts. Explicit reference types already
            // declare their access contract; owned payloads still need a convention.
            if enforce {
                for item in &linked.items {
                    let ast::Item::Function(f) = item else { continue };
                    if !witchy::is_entry_function(&f.name, &entry) {
                        continue;
                    }
                    for p in &f.params {
                        let explicit_reference = matches!(p.ty.as_ref(), Some(ast::Type::Qualified(
                            ast::TypeQual::Borrow(_) | ast::TypeQual::BorrowMut(_), _
                        )));
                        if p.convention == ast::Convention::Let && !explicit_reference
                            && witchy::ownership_relevant(&p.ty)
                        {
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
    severity_diag(start_line, start_char, end_line, end_char, 1, message)
}

fn severity_diag(
    start_line: u32,
    start_char: u32,
    end_line: u32,
    end_char: u32,
    severity: u8,
    message: &str,
) -> Value {
    json!({
        "range": {
            "start": { "line": start_line, "character": start_char },
            "end": { "line": end_line, "character": end_char }
        },
        "severity": severity,
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
