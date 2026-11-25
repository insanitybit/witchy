//! `witchyc wasm-grants program.wy app.grants.toml` (book: capabilities-sandbox.md, "Grant
//! documents"): the whole grant of a run as reviewable data, bound to `main`'s parameters by
//! name and checked against them before anything is handed over.
//!
//! ```toml
//! [dirs]
//! data = { root = "./data" }            # main(data: Dir<Read>); rights come from the type
//! [files]
//! config = "config.toml"                # main(config: File<Read>)
//! [net]
//! api = ["api.example.com:443"]         # main(api: Net)
//! [listen]
//! server = "127.0.0.1:8080"             # main(server: Listener); IPv6 in brackets: "[::1]:8080"
//! [env]
//! environment = ["HOME", "LANG"]        # main(environment: Env)
//! [exec]
//! runner = { dir = "/opt/tools", programs = ["git"] }
//! [secrets]
//! store = { TOKEN = "reveal", SIGNING = "seal" }   # main(store: SecretStore); the caller's variables
//! signing = { use = "seal", from = "env:SIGNING_KEY" }                         # main(signing: Secret<Seal>)
//! token = { use = "reveal-bytes", from = "env:REPO_TOKEN", also_from = ["fd:3"] } # main(token: Secret<Reveal>)
//! targets = { use = "sign/witchy-2/targets", from = "fd:4" }                    # main(targets: Signer)
//! [clock]
//! clock = true
//! [rand]
//! rand = true
//! [input]
//! input = "requests.txt"                # main(input: Console<Read>)
//! # or: input = { stdin = true }        # the launcher's standard input, read whole
//! [user_caps]
//! limit = { type = "RateLimit", per_minute = 3, strict = true }
//! [paths]
//! links = false                         # no granted path goes through a symbolic link
//! ```
//!
//! `[paths] links = false` is not a parameter's: it makes the engine open each granted
//! directory, and each granted file's directory, from `/` without following a link
//! (`--no-links`). A directory swapped for a link while the run starts is refused rather than
//! followed. Paths must then be absolute plain names with no link on them.
//!
//! A parameter with no entry, an entry of the wrong kind, or a stated right the type does not
//! have is an error: the run would not start, or would grant something else than reviewed.
//! An entry no parameter takes is an over-grant, reported as a warning; a fixed secret's entry
//! (one with `use` or `from`) no parameter takes is an error. `Console<Write>` and
//! `Allocator` need no entry. Relative paths are the document's directory's.
//!
//! What it prints is the engine's flags, one per line, and a review of each binding.
use std::collections::BTreeMap;

mod launch;
pub(super) use launch::{plan as launch_plan, LaunchPlan, MAX_MANIFEST_BYTES};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Value { Text(String), Number(u64), Bool(bool), List(Vec<Value>), Table(BTreeMap<String, Value>) }

/// The document: section, then key, then value.
pub(super) type Document = BTreeMap<String, BTreeMap<String, Value>>;

/// The TOML subset grant documents use: `[section]`, `key = value`, comments, strings,
/// numbers, Bools, arrays and inline tables, each entry on one line.
pub(super) fn parse(text: &str) -> Result<Document, String> {
    let mut document = Document::new();
    let mut section: Option<String> = None;
    for (number, line) in text.lines().enumerate() {
        let at = |message: &str| format!("line {}: {message}", number + 1);
        let mut scanner = Scanner { bytes: line.as_bytes(), at: 0 };
        scanner.space();
        if scanner.done() { continue; }
        if scanner.eat(b'[') {
            let name = scanner.key().ok_or_else(|| at("a section is `[name]`"))?;
            if !scanner.eat(b']') { return Err(at("a section is `[name]`")); }
            scanner.space();
            if !scanner.done() { return Err(at("nothing follows a section's name")); }
            if document.contains_key(&name) { return Err(at("this section is already declared")); }
            document.insert(name.clone(), BTreeMap::new());
            section = Some(name);
            continue;
        }
        let Some(current) = &section else { return Err(at("an entry belongs to a `[section]`")) };
        let key = scanner.key().ok_or_else(|| at("an entry is `name = value`"))?;
        scanner.space();
        if !scanner.eat(b'=') { return Err(at("an entry is `name = value`")); }
        let value = scanner.value(0).map_err(|m| at(&m))?;
        scanner.space();
        if !scanner.done() { return Err(at("one entry per line")); }
        let entries = document.get_mut(current).expect("declared");
        if entries.insert(key, value).is_some() { return Err(at("this entry is already declared")); }
    }
    Ok(document)
}

struct Scanner<'a> { bytes: &'a [u8], at: usize }

impl Scanner<'_> {
    fn space(&mut self) {
        while self.at < self.bytes.len() && (self.bytes[self.at] == b' ' || self.bytes[self.at] == b'\t') { self.at += 1; }
        if self.bytes.get(self.at) == Some(&b'#') { self.at = self.bytes.len(); }
    }
    fn done(&self) -> bool { self.at >= self.bytes.len() }
    fn eat(&mut self, byte: u8) -> bool {
        self.space();
        if self.bytes.get(self.at) == Some(&byte) { self.at += 1; true } else { false }
    }
    fn key(&mut self) -> Option<String> {
        self.space();
        let start = self.at;
        while self.at < self.bytes.len() && (self.bytes[self.at].is_ascii_alphanumeric() || matches!(self.bytes[self.at], b'_' | b'-')) { self.at += 1; }
        (self.at > start).then(|| String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned())
    }
    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > 4 { return Err("values nest at most four deep".into()); }
        self.space();
        match self.bytes.get(self.at) {
            Some(b'"') => {
                self.at += 1;
                let mut out = Vec::new();
                loop {
                    match self.bytes.get(self.at) {
                        None => return Err("a string is closed on its line".into()),
                        Some(b'"') => { self.at += 1; break; }
                        Some(b'\\') => {
                            let escaped = match self.bytes.get(self.at + 1) { Some(b'"') => b'"', Some(b'\\') => b'\\', _ => return Err("a string escapes only `\\\"` and `\\\\`".into()) };
                            out.push(escaped);
                            self.at += 2;
                        }
                        Some(&byte) => { out.push(byte); self.at += 1; }
                    }
                }
                String::from_utf8(out).map(Value::Text).map_err(|_| "a string is UTF-8".into())
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                if self.eat(b']') { return Ok(Value::List(items)); }
                loop {
                    items.push(self.value(depth + 1)?);
                    if self.eat(b']') { return Ok(Value::List(items)); }
                    if !self.eat(b',') { return Err("an array is `[a, b]`".into()); }
                }
            }
            Some(b'{') => {
                self.at += 1;
                let mut table = BTreeMap::new();
                if self.eat(b'}') { return Ok(Value::Table(table)); }
                loop {
                    let key = self.key().ok_or("an inline table is `{ name = value }`")?;
                    if !self.eat(b'=') { return Err("an inline table is `{ name = value }`".into()); }
                    let value = self.value(depth + 1)?;
                    if table.insert(key, value).is_some() { return Err("a key is given twice".into()); }
                    if self.eat(b'}') { return Ok(Value::Table(table)); }
                    if !self.eat(b',') { return Err("an inline table is `{ a = 1, b = 2 }`".into()); }
                }
            }
            _ => {
                let word = self.key().ok_or("a value is a string, number, Bool, array or inline table")?;
                match word.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    number => number.parse().map(Value::Number).map_err(|_| format!("`{number}` is not a value")),
                }
            }
        }
    }
}

/// What `main` takes: each parameter's name and type, as written (`Dir<Read>`), with a
/// grantable capability's type name.
pub(super) struct Parameter { pub name: String, pub ty: String, pub grantable: bool }

/// The engine's flags for `document`, a review of each binding, and warnings; or why the
/// document does not fit `main`.
#[derive(Debug)]
pub(super) struct Granted { pub flags: Vec<String>, pub review: Vec<String>, pub warnings: Vec<String> }

pub(super) fn check(document: &Document, parameters: &[Parameter], base: &std::path::Path) -> Result<Granted, Vec<String>> {
    let mut errors = Vec::new();
    let mut flags = Vec::new();
    let mut review = Vec::new();
    let mut used: BTreeMap<(&str, &str), bool> = BTreeMap::new();
    for (section, entries) in document {
        if !matches!(section.as_str(), "dirs" | "files" | "net" | "listen" | "env" | "exec" | "secrets" | "clock" | "rand" | "input" | "args" | "user_caps" | "paths" | "sqlite" | "local_service" | "launch" | "flags" | "limits") {
            errors.push(format!("[{section}] is not a grant section"));
        }
        for key in entries.keys() { used.insert((section.as_str(), key.as_str()), false); }
    }
    let path = |value: &str| -> String {
        let p = std::path::Path::new(value);
        let joined = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
        joined.to_string_lossy().into_owned()
    };
    let has_dir_read = parameters.iter().any(|p| p.ty == "Dir<Read>" || p.ty == "Dir");
    // A checked module always imports its own heap's authorizer (the buffer profile), whether
    // or not `main` takes an `Allocator`: memory inside the module, not authority outside it.
    flags.push("--allocator".into());
    // The same document can be a native launch manifest (`launch_plan`). Its version is
    // checked here too; a declared launch argument is resolved only by a manifest-bound native
    // executable, so the engine's flags cannot carry it.
    if let Some(launch) = document.get("launch") {
        for key in launch.keys() { used.insert(("launch", key.as_str()), true); }
        if let Err(why) = launch_version(launch) { errors.push(why); }
    }
    // `[limits]` are the engine's limit flags of those names (a launch plan checks which apply).
    for (key, value) in document.get("limits").into_iter().flatten() {
        used.insert(("limits", key.as_str()), true);
        match value {
            Value::Number(n) if matches!(key.as_str(), "deadline-ms" | "output-bytes" | "input-bytes" | "fs-bytes" | "fs-entries" | "net-bytes" | "exec-bytes" | "connections" | "waiting-connections" | "connection-deadline-ms" | "net-bytes-per-connection" | "output-bytes-per-connection") => {
                flags.push(format!("--{key}={n}"));
                review.push(format!("limit: {key} = {n}"));
            }
            // Refused, not dropped: the engine has no process limit for a program.
            Value::Number(_) if key == "exec-processes" => errors.push("[limits].exec-processes is a native Linux executable's limit (`witchyc build --manifest`): the engine runs programs without it".into()),
            _ => errors.push(format!("[limits].{key} is not a launch limit with a number")),
        }
    }
    for key in document.get("flags").into_iter().flat_map(|entries| entries.keys()) {
        used.insert(("flags", key.as_str()), true);
        errors.push(format!("[flags].{key}: a declared launch argument is resolved by a manifest-bound native executable (`witchyc build --manifest`), not by engine flags"));
    }
    if let Some(paths) = document.get("paths") {
        for (key, value) in paths {
            used.insert(("paths", key.as_str()), true);
            match (key.as_str(), value) {
                ("links", Value::Bool(false)) => { flags.push("--no-links".into()); review.push("paths: no granted path goes through a symbolic link".into()); }
                ("links", Value::Bool(true)) => {}
                _ => errors.push(format!("[paths].{key}: the only entry is `links = false`")),
            }
        }
    }
    for parameter in parameters {
        let (name, ty) = (parameter.name.as_str(), parameter.ty.as_str());
        if ty == "Console<Write>" { flags.push("--output".into()); review.push(format!("{name}: Console<Write>, the program's output")); continue; }
        if ty == "Allocator" { review.push(format!("{name}: Allocator, heap memory")); continue; }
        let section = if parameter.grantable { "user_caps" } else { match ty {
            "Dir" | "Dir<Read>" | "Dir<Write>" => "dirs", "File<Read>" | "File<Write>" => "files", "Net" => "net", "Listener" | "Socket" => "listen",
            "Env" => "env", "Exec" => "exec", "SecretStore" | "Secret<Seal>" | "Secret<Reveal>" | "Signer" | "TlsSigner" | "Mldsa87Signer" => "secrets", "Clock" => "clock", "Rand" => "rand", "Console<Read>" => "input", "Args" => "args",
            "Sqlite" => "sqlite",
            "LocalService" => "local_service",
            other => { errors.push(format!("{name}: {other} cannot be granted by a document")); continue; }
        } };
        let Some(value) = document.get(section).and_then(|entries| entries.get(name)) else {
            // Named in another section: the document grants something other than the type.
            let elsewhere = document.iter().find(|(_, entries)| entries.contains_key(name)).map(|(s, _)| s.clone());
            errors.push(match elsewhere {
                Some(other) => format!("{name}: main takes a {ty}, which is granted in [{section}], not [{other}]"),
                None => format!("{name}: main takes a {ty}, and the document grants none ([{section}].{name})"),
            });
            continue;
        };
        used.insert((section, name), true);
        let mut wrong = |what: &str| errors.push(format!("[{section}].{name}: {what}"));
        match (section, value) {
            ("dirs", Value::Table(table)) => {
                let root = match table.get("root").map(source) {
                    Some(Ok(Source::Literal(root))) => root,
                    Some(Ok(_)) => { wrong("this root is resolved when a manifest-bound native executable starts; the engine's flags take a path (`root = \"path\"`)"); continue }
                    Some(Err(why)) => { wrong(&why); continue }
                    None => { wrong("a directory is `{ root = \"path\" }`"); continue }
                };
                let root = &root;
                // A stated right must be the type's: the type is what the code can do.
                let rights: &[&str] = match ty { "Dir<Read>" => &["Read"], "Dir<Write>" => &["Write"], _ => &["Read", "Write"] };
                if let Some(stated) = table.get("rights") {
                    let said: Option<Vec<&str>> = match stated { Value::List(items) => items.iter().map(|i| if let Value::Text(t) = i { Some(t.as_str()) } else { None }).collect(), _ => None };
                    let mut said = said.unwrap_or_default();
                    said.sort();
                    if said != rights { wrong(&format!("main's {ty} has the rights {rights:?}, and the document states {said:?}")); continue; }
                }
                if table.keys().any(|k| k != "root" && k != "rights") { wrong("a directory has only `root` and `rights` (`policy` and `within` are a native launch manifest's)"); continue; }
                match ty {
                    "Dir<Write>" if has_dir_read => flags.push(format!("--write-dir={}", path(root))),
                    "Dir<Write>" => { flags.push(format!("--dir={}", path(root))); flags.push("--dir-rights=write".into()); }
                    "Dir" => { flags.push(format!("--dir={}", path(root))); flags.push("--dir-rights=read,write".into()); }
                    _ => flags.push(format!("--dir={}", path(root))),
                }
                review.push(format!("{name}: {ty} at {}", path(root)));
            }
            ("files", Value::Text(file)) => {
                flags.push(format!("{}={}", if ty == "File<Write>" { "--write-file" } else { "--file" }, path(file)));
                review.push(format!("{name}: {ty}, the file {}", path(file)));
            }
            // One entry is one endpoint: the engine splits its flag on `,`.
            ("net", Value::List(items)) if !items.is_empty() && items.iter().all(|i| matches!(i, Value::Text(t) if !t.contains(',') && t.rsplit_once(':').is_some_and(|(h, p)| !h.is_empty() && p.parse::<u16>().is_ok()))) => {
                let endpoints: Vec<String> = items.iter().map(|i| if let Value::Text(t) = i { t.clone() } else { unreachable!() }).collect();
                flags.push(format!("--net={}", endpoints.join(",")));
                review.push(format!("{name}: Net, dialing only {}", endpoints.join(", ")));
            }
            ("sqlite", Value::Table(table)) => {
                let Some(Value::Text(file)) = table.get("file") else { wrong("SQLite needs a database file"); continue };
                let Some(rights) = sqlite_rights(table) else { wrong("SQLite needs read, insert, or delete lists of table names, or a separate create list"); continue };
                if let Some(reply) = table.get("reply_bytes") {
                    let Value::Number(bytes) = reply else { wrong("SQLite reply_bytes must be an integer"); continue };
                    if !(1..=2_097_152).contains(bytes) { wrong("SQLite reply_bytes must be between 1 and 2097152"); continue; }
                    flags.push(format!("--sqlite-reply-bytes={bytes}"));
                    review.push(format!("{name}: SQLite reply at most {bytes} bytes"));
                }
                flags.push(format!("--sqlite={}", path(file)));
                for (right, names) in &rights {
                    flags.push(format!("--sqlite-{right}={names}"));
                }
                review.push(format!("{name}: Sqlite, one transaction on {} with {rights:?}", path(file)));
            }
            // `main(connection: Socket)`: the engine listens, and the program is run once for
            // each connection it accepts, in a new instance.
            ("local_service", Value::Text(endpoint)) => {
                let descriptor = endpoint.strip_prefix("fd:").and_then(|number| number.parse::<u32>().ok())
                    .filter(|fd| (3..=1023).contains(fd) && endpoint.as_str() == format!("fd:{fd}").as_str());
                if let Some(fd) = descriptor.filter(|_| parameters.iter().any(|parameter| parameter.ty == "Socket")) {
                    flags.push(format!("--local-service-fd={fd}"));
                    review.push(format!("{name}: LocalService, one request per connection to an inherited Unix stream fd:{fd}"));
                } else {
                    wrong("a LocalService uses a connected Unix stream \"fd:N\" and requires a Socket parameter");
                }
            }
            ("listen", Value::Text(address)) if ty == "Socket" => match listener(address) {
                Some((flag, shown)) => { flags.push(format!("--listen={flag}")); flags.push("--instance-per-connection".into()); review.push(format!("{name}: Socket, one connection accepted on {shown}; the program is run once for each, in a new instance")); }
                None => wrong("a connection is accepted on `\"HOST:PORT\"`, an IPv6 address in brackets (`\"[::1]:8080\"`)"),
            },
            ("listen", Value::Text(address)) => match listener(address) {
                Some((flag, shown)) => { flags.push(format!("--listen={flag}")); review.push(format!("{name}: Listener on {shown}")); }
                None => wrong("a listener is `\"HOST:PORT\"`, an IPv6 address in brackets (`\"[::1]:8080\"`)"),
            },
            ("env", Value::List(items)) if items.iter().all(|i| matches!(i, Value::Text(t) if !t.is_empty() && !t.contains('='))) => {
                let names: Vec<String> = items.iter().map(|i| if let Value::Text(t) = i { t.clone() } else { unreachable!() }).collect();
                for variable in &names { flags.push(format!("--env={variable}")); }
                review.push(format!("{name}: Env, reading only {}", if names.is_empty() { "nothing".into() } else { names.join(", ") }));
            }
            ("exec", Value::Table(table)) => {
                let (Some(Value::Text(dir)), Some(Value::List(programs))) = (table.get("dir"), table.get("programs")) else { wrong("programs are `{ dir = \"/path\", programs = [\"name\"] }`"); continue };
                let names: Option<Vec<&str>> = programs.iter().map(|p| if let Value::Text(t) = p { (!t.is_empty() && !t.contains(['/', ',', ':'])).then_some(t.as_str()) } else { None }).collect();
                let Some(names) = names.filter(|n| !n.is_empty()) else { wrong("each program is a plain name in its directory"); continue };
                if let Some(other) = table.keys().find(|k| !matches!(k.as_str(), "dir" | "programs" | "env")) { wrong(&format!("programs are `{{ dir = \"/path\", programs = [\"name\"], env = [\"NAME\"] }}`, without `{other}`")); continue; }
                let env = match exec_env(table.get("env")) { Ok(env) => env, Err(why) => { wrong(&why); continue } };
                flags.push(format!("--exec={}:{}", path(dir), names.join(",")));
                if !env.is_empty() { flags.push(format!("--exec-env={}", env.join(","))); }
                review.push(format!("{name}: Exec, running only {} from {}{}", names.join(", "), path(dir), exec_env_review(&env)));
            }
            // A fixed secret (`main(signing: Secret<Seal>)`): its one use, which must be the
            // type's, and where its bytes come from. The flag names the source, never a value.
            ("secrets", Value::Table(table)) if ty != "SecretStore" => {
                match fixed_secret(ty, table) {
                    Ok((use_, from, also)) => {
                        flags.push(format!("--secret-slot={name}:{use_}:{from}"));
                        let alternatives = if also.is_empty() { String::new() } else { format!(" (declared alternatives, not selected: {})", also.join(", ")) };
                        let what = if use_ == TLS_SIGN { "signs TLS 1.3 server CertificateVerify transcripts".to_owned() } else if use_ == "seal" { "signs with the secret".to_string() } else if let Some(role) = use_.strip_prefix(SIGN_PREFIX) {
                            format!("signs `witchy/v2/{role}` objects (ML-DSA-65, witchy-2) with the key")
                        } else if use_ == TUF_TARGETS_SIGN { "signs TUF targets metadata (ML-DSA-87)".to_owned()
                        } else if let Some(domain) = use_.strip_prefix(APPLICATION_SIGN) {
                            format!("signs messages in domain `{}` (ML-DSA-87)", domain.replace("%20", " "))
                        } else { "reads the bytes of the secret".to_string() };
                        review.push(format!("{name}: {ty}, {what} from {from}{alternatives}"));
                    }
                    Err(why) => wrong(&why),
                }
            }
            ("secrets", Value::Table(table)) => {
                let mut granted = Vec::new();
                for (secret, how) in table {
                    match how {
                        Value::Text(t) if t == "reveal" => { flags.push(format!("--secret={secret}")); granted.push(format!("{secret} (read and sign)")); }
                        Value::Text(t) if t == "seal" => { flags.push(format!("--secret-seal={secret}")); granted.push(format!("{secret} (sign only)")); }
                        _ => wrong(&format!("the secret {secret} is granted as \"reveal\" or \"seal\"")),
                    }
                }
                review.push(format!("{name}: SecretStore, {}; values from the caller's variables of those names", granted.join(", ")));
            }
            ("clock" | "rand", Value::Bool(true)) => { flags.push(format!("--{section}")); review.push(format!("{name}: {ty}")); }
            ("input", Value::Text(file)) => { flags.push(format!("--input={}", path(file))); review.push(format!("{name}: Console<Read>, from {}", path(file))); }
            // The launcher's own standard input, read whole before the program starts: a tool in a pipe.
            ("input", Value::Table(table)) if table.len() == 1 && table.get("stdin") == Some(&Value::Bool(true)) => {
                flags.push("--input-stdin".into()); review.push(format!("{name}: Console<Read>, from the launcher's standard input"));
            }
            // The words the operator gives at the launch, after `--`: how many, and how many bytes
            // in all. The words themselves are never in the document or in these flags: the
            // launcher passes them, each as one `--arg=`.
            ("args", Value::Bool(true)) => { flags.push("--args=16:4096".into()); review.push(format!("{name}: Args, at most 16 words and 4096 bytes, given at the launch after `--`")); }
            ("args", Value::Table(table)) if !table.is_empty() && table.keys().all(|k| k == "count" || k == "max-bytes") => {
                let number = |key: &str, default: u64, most: u64| match table.get(key) { None => Some(default), Some(Value::Number(n)) if (0..=most).contains(n) => Some(*n), _ => None };
                let (Some(count), Some(bytes)) = (number("count", 16, 256), number("max-bytes", 4096, 65_536)) else { wrong("`count` is at most 256 words and `max-bytes` at most 65536"); continue };
                flags.push(format!("--args={count}:{bytes}")); review.push(format!("{name}: Args, at most {count} words and {bytes} bytes, given at the launch after `--`"));
            }
            ("args", _) => wrong(&format!("an Args is granted the launch's words (`{name} = true`), or with its limits (`{name} = {{ count = 16, max-bytes = 4096 }}`)")),
            ("input", _) => wrong(&format!("a Console<Read> is granted a file to read whole (`{name} = \"requests.txt\"`) or the launcher's standard input (`{name} = {{ stdin = true }}`)")),
            ("user_caps", Value::Table(table)) => {
                if table.get("type") != Some(&Value::Text(ty.to_string())) { wrong(&format!("main takes a {ty}: the entry says `type = \"{ty}\"`")); continue; }
                let mut fields = Vec::new();
                for (field, value) in table.iter().filter(|(k, _)| *k != "type") {
                    let shown = match value { Value::Number(n) => n.to_string(), Value::Bool(b) => b.to_string(), _ => { wrong(&format!("{field} is a number or a Bool")); continue } };
                    flags.push(format!("--grant={ty}.{field}={shown}"));
                    fields.push(format!("{field} = {shown}"));
                }
                review.push(format!("{name}: {ty} (user capability), {}", fields.join(", ")));
            }
            _ => wrong(&format!("this is not how [{section}] grants a {ty}")),
        }
    }
    // The flags are split on whitespace and glob-expanded by the shell that passes them
    // (`$(wasm-grants ...)`): a value with a space, a control character or a glob character
    // could become more or other flags than were reviewed (found by an adversarial review).
    for flag in &flags {
        if flag.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '*' | '?' | '[' | ']')) { errors.push(format!("`{}` has a space, control or glob character, which the shell would turn into flags nobody reviewed", flag.escape_debug())); }
    }
    // One source, one route: a fixed secret's source is no other secret's, and an `env:` one is
    // not also a variable the program reads through `Env`, nor a store's secret of that name
    // (which the engine reads from the variable of that name).
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    let ordinary: Vec<&str> = document.get("env").into_iter().flat_map(|entries| entries.values())
        .flat_map(|value| if let Value::List(items) = value { items.iter().filter_map(|i| if let Value::Text(t) = i { Some(t.as_str()) } else { None }).collect() } else { Vec::new() }).collect();
    let stored: Vec<&str> = parameters.iter().filter(|p| p.ty == "SecretStore")
        .filter_map(|p| match document.get("secrets").and_then(|entries| entries.get(&p.name)) { Some(Value::Table(table)) => Some(table.keys().map(String::as_str).collect::<Vec<_>>()), _ => None }).flatten().collect();
    for (key, value) in document.get("secrets").into_iter().flatten() {
        let Value::Table(table) = value else { continue };
        // A store's table names its secrets, whatever they are called.
        if parameters.iter().any(|p| &p.name == key && p.ty == "SecretStore") { continue; }
        if !table.contains_key("use") && !table.contains_key("from") { continue; }
        // Declared but taken by no `Secret<...>` parameter: an error, not a warning.
        if !parameters.iter().any(|p| &p.name == key && (p.ty == "Secret<Seal>" || p.ty == "Secret<Reveal>" || p.ty == "Signer" || p.ty == "TlsSigner" || p.ty == "Mldsa87Signer")) {
            used.insert(("secrets", key.as_str()), true);
            errors.push(format!("[secrets].{key}: a fixed secret is declared, and main takes no parameter `{key}: Secret<Seal>`, `Secret<Reveal>`, `Signer`, `TlsSigner` or `Mldsa87Signer`"));
            continue;
        }
        let listed: Vec<&str> = table.get("from").into_iter().chain(match table.get("also_from") { Some(Value::List(items)) => items.iter().collect(), _ => Vec::new() })
            .filter_map(|v| if let Value::Text(t) = v { Some(t.as_str()) } else { None }).collect();
        for source in listed {
            if let Some(other) = sources.insert(source.to_string(), key.clone()) {
                errors.push(format!("[secrets].{key}: {source} is also [secrets].{other}'s source: one source reaches one slot"));
            }
            if let Some(variable) = source.strip_prefix("env:") {
                if ordinary.contains(&variable) { errors.push(format!("[secrets].{key}: {source} is also granted through [env]: a secret's variable is not ordinary environment")); }
                if stored.contains(&variable) { errors.push(format!("[secrets].{key}: {source} is also a SecretStore's secret {variable}: one source reaches one slot")); }
            }
        }
    }
    let warnings = used.iter().filter(|(_, used)| !**used).map(|((section, key), _)| format!("over-grant: [{section}].{key} is granted, and main takes no parameter `{key}` of that kind")).collect();
    if errors.is_empty() { Ok(Granted { flags, review, warnings }) } else { Err(errors) }
}

/// Where a value comes from. A plain string is a literal; a table names its source:
/// `{ from = "literal", value = "..." }`, `{ from = "arg", name = "project" }` (a declared launch
/// argument), `{ from = "home", path = "..." }` (under the launching account's home, from the
/// account database), `{ from = "cwd", path = "..." }` (under the startup directory),
/// `{ from = "env", name = "NAME" }` and `{ from = "fd", number = 3 }`. Which sources a grant
/// takes is the grant's (`launch::plan`); a `home` or `cwd` path is relative and plain.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Source { Literal(String), Arg(String), Home(String), Cwd(String), Env(String), Fd(u32) }

pub(super) fn source(value: &Value) -> Result<Source, String> {
    let table = match value {
        Value::Text(text) => return Ok(Source::Literal(text.clone())),
        Value::Table(table) => table,
        _ => return Err("a source is a string or `{ from = \"literal\" | \"arg\" | \"home\" | \"cwd\" | \"env\" | \"fd\", ... }`".into()),
    };
    let Some(Value::Text(from)) = table.get("from") else {
        return Err("a source table names where its value comes from: `from = \"literal\"`, `\"arg\"`, `\"home\"`, `\"cwd\"`, `\"env\"` or `\"fd\"`".into());
    };
    let field = |key: &str| -> Result<(), String> {
        match table.keys().find(|k| *k != "from" && *k != key) {
            Some(other) => Err(format!("a `from = \"{from}\"` source has only `{key}`, not `{other}`")),
            None => Ok(()),
        }
    };
    let text = |key: &str| match table.get(key) { Some(Value::Text(t)) => Ok(t.clone()), _ => Err(format!("a `from = \"{from}\"` source states `{key} = \"...\"`")) };
    match from.as_str() {
        "literal" => { field("value")?; Ok(Source::Literal(text("value")?)) }
        "arg" => { field("name")?; let name = text("name")?; Ok(Source::Arg(name)) }
        "home" | "cwd" => {
            field("path")?;
            let path = if table.contains_key("path") { text("path")? } else { String::new() };
            relative_path(&path)?;
            Ok(if from == "home" { Source::Home(path) } else { Source::Cwd(path) })
        }
        "env" => {
            field("name")?;
            let name = text("name")?;
            if !plain_variable(&name) { return Err(format!("`{}` is not a variable name (letters, digits and `_`, at most 64)", name.escape_debug())); }
            Ok(Source::Env(name))
        }
        "fd" => {
            field("number")?;
            match table.get("number") {
                Some(Value::Number(n)) if (3..=1023).contains(n) => Ok(Source::Fd(*n as u32)),
                _ => Err("a descriptor is `number = N`, 3 to 1023 (not a standard stream)".into()),
            }
        }
        other => Err(format!("`from = \"{}\"` is not a source: literal, arg, home, cwd, env or fd", other.escape_debug())),
    }
}

fn plain_variable(n: &str) -> bool { n.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') && n.len() <= 64 && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') }

/// A path below a base: empty (the base itself) or plain components, with no `.`, `..`,
/// empty component, leading `/`, or control character.
pub(super) fn relative_path(path: &str) -> Result<(), String> {
    if path.is_empty() { return Ok(()); }
    if path.starts_with('/') { return Err(format!("`{}` is below its base: no leading `/`", path.escape_debug())); }
    plain_components(path)
}

fn plain_components(path: &str) -> Result<(), String> {
    if path.len() > 1024 { return Err("a path is at most 1024 bytes".into()); }
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." || component.len() > 255 || component.chars().any(char::is_control) {
            return Err(format!("`{}` has an empty, `.`, `..`, overlong or control-character component", path.escape_debug()));
        }
    }
    Ok(())
}

/// An absolute path fixed by the binary author: `/` and plain components.
pub(super) fn absolute_path(path: &str) -> Result<(), String> {
    match path.strip_prefix('/') {
        Some("") => Ok(()),
        Some(rest) => plain_components(rest),
        None => Err(format!("`{}`: a fixed path in a launch manifest is absolute; use `{{ from = \"cwd\", path = ... }}` for one under the startup directory", path.escape_debug())),
    }
}

fn launch_version(launch: &BTreeMap<String, Value>) -> Result<(), String> {
    match (launch.get("version"), launch.len()) {
        (Some(Value::Number(1)), 1) => Ok(()),
        (Some(Value::Number(n)), 1) => Err(format!("[launch] version = {n} is not known; this compiler reads version 1")),
        _ => Err("[launch] has exactly `version = 1`".into()),
    }
}

/// A signer's use: `sign/witchy-2/ROLE`, the `witchy-2` suite (ML-DSA-65) for one role.
const APPLICATION_SIGN: &str = "sign/ml-dsa-87/";
const TUF_TARGETS_SIGN: &str = "sign/tuf/targets-ml-dsa-87";
const TLS_SIGN: &str = "sign/tls13/server-p384";
const SIGN_PREFIX: &str = "sign/witchy-2/";
/// The retired suite's use (`witchy-1`, ML-DSA-44): refused by name, so a document from before the
/// cutover is told what changed rather than that its use is unknown.
const RETIRED_SIGN_PREFIX: &str = "sign/witchy-1/";

/// A `witchy-2` role, as a signer's use names it: 1 to 32 lowercase letters, digits and `-`
/// (the engine checks the same, `--secret-slot`).
/// A `[listen]` address, `HOST:PORT`: what the engine's flag carries, and how a review (and a
/// request's `Host` header) spells it. The port is digits. An IPv6 address is written in
/// brackets, as an authority is (`[::1]:8080`), and must be a literal; its flag carries the
/// canonical literal without them (`--listen=::1:8080`), since brackets are glob characters
/// to the shell the flags may pass through, and the last `:` still ends the host. Any other
/// host has no `:`, bracket or comma in it.
fn listener(address: &str) -> Option<(String, String)> {
    let (host, port) = address.rsplit_once(':')?;
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) || port.parse::<u16>().is_err() { return None; }
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        let literal = inner.parse::<std::net::Ipv6Addr>().ok()?;
        return Some((format!("{literal}:{port}"), format!("[{literal}]:{port}")));
    }
    if host.is_empty() || host.contains([':', '[', ']', ',']) { return None; }
    Some((address.to_owned(), address.to_owned()))
}

fn sqlite_rights(table: &BTreeMap<String, Value>) -> Option<Vec<(&str, String)>> {
    let mut rights = Vec::new();
    for (right, value) in table {
        if matches!(right.as_str(), "file" | "reply_bytes") { continue; }
        if !matches!(right.as_str(), "read" | "insert" | "delete" | "create") { return None; }
        let Value::List(names) = value else { return None };
        let names: Option<Vec<&str>> = names.iter().map(|item| {
            let Value::Text(name) = item else { return None };
            let plain = name.len() <= 64 && !name.starts_with("sqlite_")
                && name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                && name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
            plain.then_some(name.as_str())
        }).collect();
        let names = names?;
        if names.is_empty() || names.len() > 16 || names.iter().collect::<std::collections::BTreeSet<_>>().len() != names.len() { return None; }
        rights.push((right.as_str(), names.join(",")));
    }
    if rights.len() > 1 && table.contains_key("create") { return None; }
    (!rights.is_empty()).then_some(rights)
}

fn plain_role(role: &str) -> bool { !role.is_empty() && role.len() <= 32 && role.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') }

/// A fixed secret's entry, `{ use = "seal", from = "env:NAME", also_from = ["fd:3"] }`, checked
/// against the parameter's type: its use (`seal` or `reveal-bytes`, or for a `Signer`
/// `sign/witchy-2/ROLE`), its source and the declared alternatives. Each source is `env:NAME`
/// or `fd:N` (N at least 3: not the standard streams); any other field is refused.
fn fixed_secret(ty: &str, table: &BTreeMap<String, Value>) -> Result<(String, String, Vec<String>), String> {
    if let Some(field) = table.keys().find(|k| !matches!(k.as_str(), "use" | "from" | "also_from")) {
        return Err(format!("a fixed secret has only `use`, `from` and `also_from`, not `{field}`"));
    }
    let use_ = secret_use(ty, table)?;
    let source = |value: &Value| -> Result<String, String> {
        let Value::Text(text) = value else { return Err("a source is a string, `env:NAME` or `fd:N`".into()) };
        let plain = |n: &str| n.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') && n.len() <= 64 && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        match text.split_once(':') {
            // A signing seed never comes from a variable: an environment is inherited by children
            // and readable through `ps eww` and /proc before the engine can clear it (review of
            // the witchy-2 cutover). A Signer's source is a descriptor.
            Some(("env", _)) if ty == "Mldsa87Signer" => Err("an Mldsa87Signer seed comes from a descriptor, fd:N".into()),
            Some(("env", _)) if ty == "TlsSigner" => Err("a TlsSigner key comes from a descriptor, fd:N".into()),
            Some(("env", _)) if ty == "Signer" => Err(format!("`{}`: a Signer's seed comes from a descriptor, `fd:N`, never a variable", text.escape_debug())),
            Some(("env", variable)) if plain(variable) => Ok(text.clone()),
            Some(("fd", number)) if number.parse::<u32>().is_ok_and(|n| (3..=1023).contains(&n)) && !number.starts_with('0') => Ok(text.clone()),
            _ => Err(format!("`{}` is not a source: `env:NAME` (letters, digits, `_`) or `fd:N` (3 to 1023)", text.escape_debug())),
        }
    };
    let from = source(table.get("from").ok_or("a fixed secret states where it comes from: `from = \"env:NAME\"`")?)?;
    let also = match table.get("also_from") {
        None => Vec::new(),
        Some(Value::List(items)) => items.iter().map(source).collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("`also_from` is a list of sources".into()),
    };
    if also.contains(&from) || (1..also.len()).any(|i| also[..i].contains(&also[i])) { return Err("a source is listed once".into()); }
    Ok((use_, from, also))
}


fn application_domain(encoded: &str) -> bool {
    if encoded.contains(' ') || encoded.len() > 64 * 3 { return false; }
    let domain = encoded.replace("%20", " ");
    domain.len() <= 64 && domain.split(' ').all(plain_role)
}

/// A fixed secret's use, which must be its parameter type's.
pub(super) fn secret_use(ty: &str, table: &BTreeMap<String, Value>) -> Result<String, String> {
    let known = |said: &str| said == "seal" || said == "reveal-bytes" || said.starts_with(SIGN_PREFIX) || said == TLS_SIGN || said == TUF_TARGETS_SIGN || said.starts_with(APPLICATION_SIGN);
    Ok(match (ty, table.get("use")) {
        // A signer's role is the grant's: the program cannot choose it.
        ("Signer", Some(Value::Text(said))) if said.strip_prefix(SIGN_PREFIX).is_some_and(plain_role) => said.clone(),
        ("Signer", Some(Value::Text(said))) if said.starts_with(SIGN_PREFIX) => return Err(format!("\"{}\" names no role: a role is 1 to 32 lowercase letters, digits and `-`", said.escape_debug())),
        ("Signer", Some(Value::Text(said))) if known(said) => return Err(format!("main takes a Signer, whose use is \"sign/witchy-2/ROLE\", and the document says \"{said}\"")),
        ("Signer", None) => return Err("a fixed secret states its use: `use = \"sign/witchy-2/ROLE\"`".into()),
        ("Mldsa87Signer", Some(Value::Text(said))) if said == TUF_TARGETS_SIGN
            || said.strip_prefix(APPLICATION_SIGN).is_some_and(application_domain) => said.clone(),
        ("Mldsa87Signer", _) => return Err("an Mldsa87Signer requires a domain in sign/ml-dsa-87/DOMAIN or sign/tuf/targets-ml-dsa-87".into()),
        ("TlsSigner", Some(Value::Text(said))) if said == TLS_SIGN => said.clone(),
        ("TlsSigner", _) => return Err(format!("a TlsSigner requires use = {TLS_SIGN}")),
        ("Secret<Seal>", Some(Value::Text(said))) if said == "seal" => said.clone(),
        ("Secret<Reveal>", Some(Value::Text(said))) if said == "reveal-bytes" => said.clone(),
        (_, Some(Value::Text(said))) if known(said) => {
            let wanted = if ty == "Secret<Seal>" { "seal" } else { "reveal-bytes" };
            return Err(format!("main takes a {ty}, whose use is \"{wanted}\", and the document says \"{said}\""));
        }
        (_, Some(Value::Text(said))) if said.starts_with(RETIRED_SIGN_PREFIX) => return Err(format!("\"{}\": the witchy-1 suite (ML-DSA-44) is retired; a Signer's use is \"sign/witchy-2/ROLE\" (ML-DSA-65)", said.escape_debug())),
        (_, Some(_)) => return Err("`use` is \"seal\", \"reveal-bytes\" or \"sign/witchy-2/ROLE\"".into()),
        (_, None) => return Err(format!("a fixed secret states its use: `use = \"{}\"`", if ty == "Secret<Seal>" { "seal" } else { "reveal-bytes" })),
    })
}

/// An Exec grant's declared variable names (`env = ["LANG", "TZ"]`): the only variables a
/// program may be given, each value from the program (`Command.env`). At most 16 plain names
/// (an upper-case letter or `_`, then upper-case letters, digits or `_`, at most 64 bytes), each
/// once. Empty when the entry declares none: programs then get an empty environment.
pub(in crate::compiler) fn exec_env(value: Option<&Value>) -> Result<Vec<String>, String> {
    let Some(value) = value else { return Ok(Vec::new()) };
    let Value::List(items) = value else { return Err("`env` is a list of variable names, as `env = [\"LANG\", \"TZ\"]`".into()) };
    let mut names = Vec::new();
    for item in items {
        let Value::Text(name) = item else { return Err("each `env` entry is a variable name in quotes".into()) };
        let plain = super::exec_env_policy::plain_name(name.as_bytes());
        if !plain { return Err(format!("`env` names `{}`: a program's variable is a plain upper-case name (letters, digits, `_`; no `=`)", name.escape_debug())); }
        if exec_env_loads_code(name) { return Err(format!("`env` names `{name}`: a variable that makes a program load code or configuration from a path the Witchy program chooses is never given to a program (src/compiler/exec_env_refused.txt)")); }
        if names.contains(name) { return Err(format!("`env` names `{name}` twice")); }
        names.push(name.clone());
    }
    if names.len() > 16 { return Err("`env` declares at most 16 names".into()); }
    Ok(names)
}

/// Whether a variable name is on the refusal list (src/compiler/exec_env_refused.txt): one that makes
/// a program load code or configuration from a path the Witchy program chooses.
pub(in crate::compiler) fn exec_env_loads_code(name: &str) -> bool {
    super::exec_env_policy::loads_code(name.as_bytes())
}

pub(in crate::compiler) fn exec_env_review(names: &[String]) -> String {
    if names.is_empty() { String::new() } else { format!("; a program may be given only the variables {} (values from the program, which can change what the program does)", names.join(", ")) }
}

#[cfg(test)]
mod tests {
    use super::{check, parse, Parameter, Value};
    fn parameter(name: &str, ty: &str) -> Parameter { Parameter { name: name.into(), ty: ty.into(), grantable: false } }
    /// `[paths] links = false` asks the engine to open granted paths without following links.
    /// The refusal list (exec_env_refused.txt): every prefix class and some exact names are
    /// refused in a document; ordinary names, and names that only share a prefix with an exact
    /// entry, are not.
    #[test]
    fn exec_env_refuses_names_that_load_code() {
        let declared = |name: &str| super::exec_env(Some(&Value::List(vec![Value::Text(name.into())])));
        for name in ["LD_PRELOAD", "LD_LIBRARY_PATH", "DYLD_INSERT_LIBRARIES", "MALLOC_CONF", "GIT_SSH_COMMAND", "PYTHONPATH", "PERL5OPT", "RUBYOPT",
                     "NODE_OPTIONS", "BASH_ENV", "BASH_FUNC_X", "PATH", "IFS", "ENV", "HOME", "SHELL", "TMPDIR", "GCONV_PATH", "JAVA_TOOL_OPTIONS",
                     "_JAVA_OPTIONS", "OPENSSL_CONF", "SSL_CERT_FILE", "CURL_CA_BUNDLE", "LESSOPEN", "PAGER", "EDITOR", "VISUAL", "RESOLV_HOST_CONF"] {
            assert!(declared(name).is_err_and(|e| e.contains("load code or configuration")), "{name}");
        }
        for name in ["LANG", "LC_ALL", "TZ", "TERM", "COLUMNS", "PATHS", "HOMEPAGE", "MY_LD_FLAG"] {
            assert_eq!(declared(name).unwrap(), vec![name.to_string()], "{name}");
        }
    }

    #[test]
    fn a_document_can_refuse_links_on_granted_paths() {
        let granted = check(&parse("[paths]\nlinks = false\n[dirs]\ndata = { root = \"/srv/data\" }\n").unwrap(), &[parameter("data", "Dir<Read>")], std::path::Path::new("/")).unwrap();
        assert_eq!(granted.flags, ["--allocator", "--no-links", "--dir=/srv/data"]);
        assert!(granted.warnings.is_empty(), "{:?}", granted.warnings);
        let other = check(&parse("[paths]\nfollow = true\n").unwrap(), &[], std::path::Path::new("/")).unwrap_err();
        assert!(other[0].contains("the only entry is `links = false`"), "{other:?}");
    }

    #[test]
    fn the_subset_reads_sections_entries_and_nesting() {
        let document = parse("# a grant\n[dirs]\ndata = { root = \"./data\", rights = [\"Read\"] } # trailing\n[net]\napi = [\"a.example:443\", \"b.example:80\"]\n[clock]\nclock = true\n").unwrap();
        assert_eq!(document["net"]["api"], Value::List(vec![Value::Text("a.example:443".into()), Value::Text("b.example:80".into())]));
        assert_eq!(document["clock"]["clock"], Value::Bool(true));
        assert!(parse("data = 1\n").is_err(), "an entry needs a section");
        assert!(parse("[a]\nx = 1\nx = 2\n").is_err(), "an entry is declared once");
        assert!(parse("[a]\nx = \"open\n").is_err(), "a string closes on its line");
    }

    #[test]
    fn a_document_binds_main_by_name_and_says_what_it_grants() {
        let document = parse("[dirs]\ndata = { root = \"data\", rights = [\"Read\"] }\nout = { root = \"/tmp/out\" }\n[net]\napi = [\"api.example.com:443\"]\n[secrets]\nstore = { TOKEN = \"seal\" }\n[user_caps]\nlimit = { type = \"RateLimit\", per_minute = 3, strict = true }\n").unwrap();
        let parameters = [parameter("console", "Console<Write>"), parameter("data", "Dir<Read>"), parameter("out", "Dir<Write>"), parameter("api", "Net"), parameter("store", "SecretStore"),
            Parameter { name: "limit".into(), ty: "RateLimit".into(), grantable: true }];
        let granted = check(&document, &parameters, std::path::Path::new("/srv/app")).unwrap();
        assert_eq!(granted.flags, ["--allocator", "--output", "--dir=/srv/app/data", "--write-dir=/tmp/out", "--net=api.example.com:443", "--secret-seal=TOKEN", "--grant=RateLimit.per_minute=3", "--grant=RateLimit.strict=true"]);
        assert!(granted.warnings.is_empty(), "{:?}", granted.warnings);
    }

    #[test]
    fn an_input_is_a_file_or_the_launchers_standard_input() {
        let input = [parameter("input", "Console<Read>")];
        let file = check(&parse("[input]\ninput = \"requests.txt\"\n").unwrap(), &input, std::path::Path::new("/p")).unwrap();
        assert_eq!((file.flags, file.review), (vec!["--allocator".to_owned(), "--input=/p/requests.txt".to_owned()], vec!["input: Console<Read>, from /p/requests.txt".to_owned()]));
        let piped = check(&parse("[input]\ninput = { stdin = true }\n").unwrap(), &input, std::path::Path::new("/p")).unwrap();
        assert_eq!((piped.flags, piped.review), (vec!["--allocator".to_owned(), "--input-stdin".to_owned()], vec!["input: Console<Read>, from the launcher's standard input".to_owned()]));
        // Anything else says both ways (it said only "this is not how [input] grants a Console<Read>").
        for other in ["input = true", "input = { stdin = false }", "input = { stdin = true, file = \"x\" }", "input = [\"a\"]"] {
            let refused = check(&parse(&format!("[input]\n{other}\n")).unwrap(), &input, std::path::Path::new("/p")).unwrap_err();
            assert!(refused[0].contains("`input = \"requests.txt\"`") && refused[0].contains("`input = { stdin = true }`"), "{other}: {refused:?}");
        }
    }
    #[test]
    fn a_document_that_does_not_fit_main_is_refused_before_launch() {
        let missing = check(&parse("[net]\napi = [\"a.example:1\"]\n").unwrap(), &[parameter("data", "Dir<Read>")], std::path::Path::new("/")).unwrap_err();
        assert!(missing[0].contains("the document grants none"), "{missing:?}");
        let wrong_section = check(&parse("[files]\ndata = \"x\"\n").unwrap(), &[parameter("data", "Dir<Read>")], std::path::Path::new("/")).unwrap_err();
        assert!(wrong_section[0].contains("granted in [dirs], not [files]"), "{wrong_section:?}");
        let wider = check(&parse("[dirs]\ndata = { root = \"d\", rights = [\"Read\", \"Write\"] }\n").unwrap(), &[parameter("data", "Dir<Read>")], std::path::Path::new("/")).unwrap_err();
        assert!(wider[0].contains("has the rights [\"Read\"]"), "{wider:?}");
        let split = check(&parse("[dirs]\ndata = { root = \"d --write-dir=/\" }\n").unwrap(), &[parameter("data", "Dir<Read>")], std::path::Path::new("/")).unwrap_err();
        assert!(split[0].contains("flags nobody reviewed"), "{split:?}");
        let glob = check(&parse("[dirs]\ndata = { root = \"d*\" }\n").unwrap(), &[parameter("data", "Dir<Read>")], std::path::Path::new("/")).unwrap_err();
        assert!(glob[0].contains("glob"), "{glob:?}");
        let two = check(&parse("[net]\napi = [\"a.example:443,b.example:443\"]\n").unwrap(), &[parameter("api", "Net")], std::path::Path::new("/")).unwrap_err();
        assert!(two[0].contains("not how [net] grants"), "{two:?}");
        let over = check(&parse("[dirs]\ndata = { root = \"d\" }\n[clock]\nclock = true\n").unwrap(), &[parameter("data", "Dir<Read>")], std::path::Path::new("/")).unwrap();
        assert_eq!(over.warnings, ["over-grant: [clock].clock is granted, and main takes no parameter `clock` of that kind"]);
        // A native-only limit is refused on the engine's path, never dropped.
        let processes = check(&parse("[launch]\nversion = 1\n[limits]\nexec-processes = 8\n").unwrap(), &[], std::path::Path::new("/")).unwrap_err();
        assert!(processes.iter().any(|e| e.contains("exec-processes is a native Linux executable's limit")), "{processes:?}");
    }

    /// A listener is an address and a port. An IPv6 literal is written in brackets, reviewed in
    /// brackets, and carried to the engine without them (a flag never holds a glob character).
    #[test]
    fn a_listener_is_an_address_with_a_port_and_ipv6_is_bracketed() {
        let granted = |address: &str| check(&parse(&format!("[listen]\nserver = \"{address}\"\n")).unwrap(), &[parameter("server", "Listener")], std::path::Path::new("/"));
        // `Args`: the document grants how many words and bytes; the words are the launch's.
        let args = |entry: &str| check(&parse(&format!("[args]\nwords = {entry}\n")).unwrap(), &[parameter("words", "Args")], std::path::Path::new("/"));
        let plain = args("true").unwrap();
        assert_eq!((plain.flags, plain.review), (vec!["--allocator".to_owned(), "--args=16:4096".to_owned()], vec!["words: Args, at most 16 words and 4096 bytes, given at the launch after `--`".to_owned()]));
        assert_eq!(args("{ count = 2, max-bytes = 64 }").unwrap().flags[1], "--args=2:64");
        assert_eq!(args("{ count = 0 }").unwrap().flags[1], "--args=0:4096");
        for wrong in ["false", "{ count = 257 }", "{ max-bytes = 65537 }", "{ words = 3 }", "{ }", "\"a b\"", "[\"a\"]"] { assert!(args(wrong).is_err(), "{wrong}"); }
        // Granted and not taken: an over-grant, said, and no flag; taken and not granted: refused.
        let over = check(&parse("[args]\nwords = true\n").unwrap(), &[], std::path::Path::new("/")).unwrap();
        assert_eq!((over.flags, over.warnings.len()), (vec!["--allocator".to_owned()], 1));
        assert!(check(&parse("").unwrap(), &[parameter("words", "Args")], std::path::Path::new("/")).is_err());
        // A `Socket` parameter is the same grant, served an instance at a time.
        let each = check(&parse("[listen]\nconnection = \"127.0.0.1:8080\"\n").unwrap(), &[parameter("connection", "Socket")], std::path::Path::new("/")).unwrap();
        assert_eq!((each.flags, each.review), (vec!["--allocator".to_owned(), "--listen=127.0.0.1:8080".to_owned(), "--instance-per-connection".to_owned()], vec!["connection: Socket, one connection accepted on 127.0.0.1:8080; the program is run once for each, in a new instance".to_owned()]));
        let four = granted("127.0.0.1:8080").unwrap();
        assert_eq!((four.flags.as_slice(), four.review.as_slice()), (["--allocator".to_string(), "--listen=127.0.0.1:8080".into()].as_slice(), ["server: Listener on 127.0.0.1:8080".to_string()].as_slice()));
        let six = granted("[::1]:8417").unwrap();
        assert_eq!((six.flags.as_slice(), six.review.as_slice()), (["--allocator".to_string(), "--listen=::1:8417".into()].as_slice(), ["server: Listener on [::1]:8417".to_string()].as_slice()));
        // One spelling reaches the engine, whatever the document's.
        assert_eq!(granted("[0:0:0:0:0:0:0:1]:8417").unwrap().flags[1], "--listen=::1:8417");
        assert_eq!(granted("[2001:DB8::7]:80").unwrap().flags[1], "--listen=2001:db8::7:80");
        for refused in ["::1:8417", "[::1]", "[::1]:", "[::1]8417", "[::1:8417", "::1]:8417", "[localhost]:80", "[127.0.0.1]:80", "[::1%lo0]:80", "[]:80", "[*]:80", "a[b]:80",
            ":80", "127.0.0.1", "127.0.0.1:+80", "127.0.0.1:65536", "127.0.0.1:80,127.0.0.2:80", "127.0.0.1:80 --net=x:1"] {
            let errors = granted(refused).unwrap_err();
            assert!(errors[0].contains("a listener is"), "{refused}: {errors:?}");
        }
    }

    /// Fixed secrets bind by parameter name, whatever order `main` and the document use, and
    /// the flags name each slot's source, never a value.
    #[test]
    fn fixed_secrets_bind_by_name_and_name_only_their_source() {
        let document = parse("[secrets]\nalpha = { use = \"seal\", from = \"env:KEY_A\" }\nbeta = { use = \"seal\", from = \"env:KEY_B\" }\ntoken = { use = \"reveal-bytes\", from = \"env:REPO_TOKEN\", also_from = [\"fd:3\"] }\n").unwrap();
        let parameters = [parameter("beta", "Secret<Seal>"), parameter("token", "Secret<Reveal>"), parameter("alpha", "Secret<Seal>")];
        let granted = check(&document, &parameters, std::path::Path::new("/")).unwrap();
        assert_eq!(granted.flags, ["--allocator", "--secret-slot=beta:seal:env:KEY_B", "--secret-slot=token:reveal-bytes:env:REPO_TOKEN", "--secret-slot=alpha:seal:env:KEY_A"]);
        assert!(granted.warnings.is_empty(), "{:?}", granted.warnings);
        assert!(granted.review[1].contains("fd:3"), "{:?}", granted.review);
        // The legacy store beside them keeps its spelling.
        let both = parse("[secrets]\nstore = { TOKEN = \"seal\" }\nalpha = { use = \"seal\", from = \"fd:4\" }\n").unwrap();
        let granted = check(&both, &[parameter("store", "SecretStore"), parameter("alpha", "Secret<Seal>")], std::path::Path::new("/")).unwrap();
        assert_eq!(granted.flags, ["--allocator", "--secret-seal=TOKEN", "--secret-slot=alpha:seal:fd:4"]);
    }

    #[test]
    fn a_fixed_secret_that_does_not_fit_is_refused() {
        let refused = |text: &str, parameters: &[super::Parameter]| check(&parse(text).unwrap(), parameters, std::path::Path::new("/")).unwrap_err().join("\n");
        let seal = [parameter("key", "Secret<Seal>")];
        let reveal = [parameter("key", "Secret<Reveal>")];
        // Wrong kind, both ways.
        assert!(refused("[secrets]\nkey = { use = \"reveal-bytes\", from = \"env:K\" }\n", &seal).contains("whose use is \"seal\""));
        assert!(refused("[secrets]\nkey = { use = \"seal\", from = \"env:K\" }\n", &reveal).contains("whose use is \"reveal-bytes\""));
        // The legacy word is not a fixed secret's use.
        assert!(refused("[secrets]\nkey = { use = \"reveal\", from = \"env:K\" }\n", &reveal).contains("`use` is"));
        // Missing: no entry, no use, no source.
        assert!(refused("[secrets]\n", &seal).contains("the document grants none"));
        assert!(refused("[secrets]\nkey = { from = \"env:K\" }\n", &seal).contains("states its use"));
        assert!(refused("[secrets]\nkey = { use = \"seal\" }\n", &seal).contains("where it comes from"));
        // Unknown field, other sources, standard streams.
        assert!(refused("[secrets]\nkey = { use = \"seal\", from = \"env:K\", optional = true }\n", &seal).contains("not `optional`"));
        for source in ["file:/etc/key", "env:", "env:A-B", "fd:0", "fd:2", "fd:03", "K", "env:K --secret=X"] {
            let text = format!("[secrets]\nkey = {{ use = \"seal\", from = \"{source}\" }}\n");
            assert!(refused(&text, &seal).contains("is not a source"), "{source}");
        }
        // Unused: an error for a fixed secret, where the store's stays a warning.
        assert!(refused("[secrets]\nkey = { use = \"seal\", from = \"env:K\" }\nspare = { use = \"seal\", from = \"env:S\" }\n", &seal).contains("[secrets].spare: a fixed secret is declared"));
        let over = check(&parse("[secrets]\nstore = { TOKEN = \"seal\" }\n").unwrap(), &[], std::path::Path::new("/")).unwrap();
        assert_eq!(over.warnings.len(), 1);
        // One source, one route: not ordinary environment, not another slot, not a store's name.
        let two = [parameter("key", "Secret<Seal>"), parameter("other", "Secret<Reveal>"), parameter("environment", "Env"), parameter("store", "SecretStore")];
        assert!(refused("[secrets]\nkey = { use = \"seal\", from = \"env:K\" }\nother = { use = \"reveal-bytes\", from = \"env:O\" }\nstore = { T = \"seal\" }\n[env]\nenvironment = [\"K\"]\n", &two).contains("also granted through [env]"));
        assert!(refused("[secrets]\nkey = { use = \"seal\", from = \"env:K\" }\nother = { use = \"reveal-bytes\", from = \"fd:3\", also_from = [\"env:K\"] }\nstore = { T = \"seal\" }\n[env]\nenvironment = []\n", &two).contains("one source reaches one slot"));
        assert!(refused("[secrets]\nkey = { use = \"seal\", from = \"env:T\" }\nother = { use = \"reveal-bytes\", from = \"fd:3\" }\nstore = { T = \"seal\" }\n[env]\nenvironment = []\n", &two).contains("SecretStore's secret T"));
        assert!(refused("[secrets]\nkey = { use = \"seal\", from = \"fd:3\", also_from = [\"fd:3\"] }\n", &seal).contains("listed once"));
    }

    /// A `Signer`'s grant names its role (`sign/witchy-2/ROLE`), which reaches the engine's flag;
    /// no other use fits it, and its use fits no `Secret`.
    #[test]
    fn a_signer_is_granted_for_one_role() {
        let signer = [parameter("targets", "Signer")];
        let granted = check(&parse("[secrets]\ntargets = { use = \"sign/witchy-2/targets\", from = \"fd:4\" }\n").unwrap(), &signer, std::path::Path::new("/")).unwrap();
        assert!(granted.flags.contains(&"--secret-slot=targets:sign/witchy-2/targets:fd:4".to_string()), "{:?}", granted.flags);
        assert!(granted.review.iter().any(|line| line.contains("signs `witchy/v2/targets` objects")), "{:?}", granted.review);
        let refused = |text: &str, parameters: &[super::Parameter]| check(&parse(text).unwrap(), parameters, std::path::Path::new("/")).unwrap_err().join("\n");
        assert!(refused("[secrets]\ntargets = { use = \"seal\", from = \"fd:4\" }\n", &signer).contains("whose use is \"sign/witchy-2/ROLE\""));
        assert!(refused("[secrets]\ntargets = { use = \"reveal-bytes\", from = \"fd:4\" }\n", &signer).contains("whose use is \"sign/witchy-2/ROLE\""));
        for role in ["", "Targets", "tar gets", "a/b", "x:y", &"r".repeat(33)] {
            let text = format!("[secrets]\ntargets = {{ use = \"sign/witchy-2/{role}\", from = \"fd:4\" }}\n");
            assert!(refused(&text, &signer).contains("names no role"), "{role}");
        }
        assert!(refused("[secrets]\ntargets = { use = \"sign/witchy-3/targets\", from = \"fd:4\" }\n", &signer).contains("`use` is"));
        // The retired suite is refused by name, for a Signer and for any other parameter.
        assert!(refused("[secrets]\ntargets = { use = \"sign/witchy-1/targets\", from = \"fd:4\" }\n", &signer).contains("witchy-1 suite (ML-DSA-44) is retired"));
        // A seed never comes from a variable, as `from` or `also_from`.
        assert!(refused("[secrets]\ntargets = { use = \"sign/witchy-2/targets\", from = \"env:SEED\" }\n", &signer).contains("never a variable"));
        assert!(refused("[secrets]\ntargets = { use = \"sign/witchy-2/targets\", from = \"fd:4\", also_from = [\"env:SEED\"] }\n", &signer).contains("never a variable"));
        assert!(refused("[secrets]\ntargets = { from = \"fd:4\" }\n", &signer).contains("sign/witchy-2/ROLE"));
        let seal = [parameter("key", "Secret<Seal>")];
        assert!(refused("[secrets]\nkey = { use = \"sign/witchy-2/targets\", from = \"fd:4\" }\n", &seal).contains("whose use is \"seal\""));
        // Declared for no parameter: an error.
        assert!(refused("[secrets]\ntargets = { use = \"sign/witchy-2/targets\", from = \"fd:4\" }\n", &[]).contains("a fixed secret is declared"));
    }
}
