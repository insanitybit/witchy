//! A native launch manifest (docs/native-trusted-executables.md): the grant document of a
//! trusted native executable, checked against `main` when the executable is built and embedded
//! in it as a versioned, read-only plan. It is the same document `wasm-grants` reads, with a
//! `[launch]` version, declared launch arguments in `[flags]` and typed sources (`Source`).
//!
//! ```toml
//! [launch]
//! version = 1
//! [flags]
//! project = { type = "path", required = true }      # `--project PATH` or `--project=PATH`
//! [dirs]
//! project = { root = { from = "arg", name = "project" }, rights = ["Read"], policy = "data-only",
//!             within = { from = "home", path = "Projects" } }
//! ```
//!
//! The plan names each grant's kind, rights, policy and sources, and nothing else: no comment,
//! review text or document path reaches the executable. Every entry must be taken by a
//! parameter of `main` and every declared argument by one source; a grant the native runtime
//! cannot enforce, or a combination of grants it cannot keep apart, is refused here.
use super::{Document, Parameter, Source, Value, absolute_path, launch_version, listener, secret_use, source, SIGN_PREFIX, TLS_SIGN};
use std::collections::BTreeMap;

/// The wire format's version, the first line's second field.
pub(in crate::compiler) const PLAN_VERSION: u32 = 1;
const MAX_PLAN_BYTES: usize = 64 * 1024;
pub(in crate::compiler) const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// The checked plan: its wire form (embedded, NUL-free), and a review of each binding.
#[derive(Debug)]
pub(crate) struct LaunchPlan {
    pub wire: Vec<u8>,
    pub review: Vec<String>,
    pub requires_aws_lc: bool,
}

/// The runtime's development flags: a declared argument never has one of these names, so a
/// reader of `./tool --output x` never has to ask which parser read it.
const RESERVED: &[&str] = &[
    "output", "stream-output", "plain-output", "limit-name", "fuel", "fuel-per-accept", "unmetered", "allocator",
    "no-links", "secret", "secret-seal", "secret-slot", "secret-object", "grant", "write-file", "write-dir",
    "instance-per-connection", "connections", "waiting-connections", "connection-deadline-ms",
    "net-bytes-per-connection", "output-bytes-per-connection", "data-only", "dir-id", "dir-rights", "dir", "file",
    "env", "args", "arg", "rand", "clock", "input", "input-stdin", "deadline-ms", "output-bytes", "input-bytes",
    "input-id", "fs-entries", "fs-bytes", "net-bytes", "net", "listen", "exec", "exec-env", "exec-bytes", "exec-processes", "help", "version",
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum FlagType { Path, Fd, Object }
struct Flag { ty: FlagType, secret: bool, used_by: Option<String>, default: Option<(String, String, String)> }

/// What a source is asked for.
#[derive(Clone, Copy, PartialEq)]
enum Want {
    /// A directory or file to open: an absolute literal, a path argument, home, cwd, or an
    /// inherited descriptor.
    Path,
    /// The boundary an operator-chosen path must stay under: an absolute literal, home or cwd.
    Within,
    /// A secret's bytes: a descriptor (literal or from a secret `fd` argument) or, unless it is
    /// a signing seed, a variable.
    Secret { signer: bool },
}

fn field(text: &str) -> Result<String, String> {
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err(format!("`{}` is empty or has a control character", text.escape_debug()));
    }
    Ok(text.to_owned())
}

pub(in crate::compiler) fn plan(document: &Document, parameters: &[Parameter], digest: &str) -> Result<LaunchPlan, Vec<String>> {
    let mut errors = Vec::new();
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut review = vec![format!("manifest sha256 {digest}, launch plan version {PLAN_VERSION}")];
    for section in document.keys() {
        if !matches!(section.as_str(), "launch" | "flags" | "limits" | "dirs" | "files" | "net" | "listen" | "env" | "exec" | "secrets" | "clock" | "rand" | "input" | "args" | "user_caps" | "paths") {
            errors.push(format!("[{section}] is not a grant section"));
        }
    }
    match document.get("launch") {
        Some(launch) => if let Err(why) = launch_version(launch) { errors.push(why) },
        None => errors.push("a launch manifest starts with `[launch]` and `version = 1`".into()),
    }
    // Native path grants never follow a link; `links = false` says so and changes nothing.
    for (key, value) in document.get("paths").into_iter().flatten() {
        if !(key == "links" && *value == Value::Bool(false)) {
            errors.push(format!("[paths].{key}: a native launch never follows a link; the only entry is `links = false`"));
        }
    }
    let mut flags: BTreeMap<String, Flag> = BTreeMap::new();
    for (name, value) in document.get("flags").into_iter().flatten() {
        let mut wrong = |why: &str| errors.push(format!("[flags].{name}: {why}"));
        if name.len() > 32 || !name.starts_with(|c: char| c.is_ascii_lowercase()) || name.ends_with('-')
            || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            wrong("an argument's name is 1 to 32 lowercase letters, digits and `-`, starting with a letter (it is written `--NAME`)");
            continue;
        }
        if RESERVED.contains(&name.as_str()) {
            wrong(&format!("`--{name}` is a development launch flag; a manifest-bound executable refuses those, so its own arguments use other names"));
            continue;
        }
        let Value::Table(table) = value else { wrong("an argument is `{ type = \"path\" }` or `{ type = \"fd\", secret = true }`"); continue };
        if let Some(other) = table.keys().find(|k| !matches!(k.as_str(), "type" | "required" | "secret" | "default")) {
            wrong(&format!("an argument has only `type`, `required`, `secret` and `default`, not `{other}`"));
            continue;
        }
        let ty = match table.get("type") {
            Some(Value::Text(t)) if t == "path" => FlagType::Path,
            Some(Value::Text(t)) if t == "fd" => FlagType::Fd,
            // `DEVICE:INODE` of a file a launcher read a secret from: it only restricts.
            Some(Value::Text(t)) if t == "object" => FlagType::Object,
            _ => { wrong("an argument's `type` is \"path\", \"fd\" or \"object\""); continue }
        };
        // An optional argument names what it is when absent: a fixed path, home or cwd source,
        // so a missing argument never silently grants less or other than reviewed.
        let default = match (table.get("required"), table.get("default")) {
            (None | Some(Value::Bool(true)), None) => None,
            (Some(Value::Bool(false)), Some(value)) if ty == FlagType::Path => match source(value) {
                Ok(Source::Literal(path)) => match absolute_path(&path) { Ok(()) => Some(("literal".to_string(), path.clone(), path)), Err(why) => { wrong(&format!("default: {why}")); continue } },
                Ok(Source::Home(path)) => Some(("home".into(), if path.is_empty() { ".".into() } else { path.clone() }, format!("{} under the launching account's home", if path.is_empty() { "." } else { &path }))),
                Ok(Source::Cwd(path)) => Some(("cwd".into(), if path.is_empty() { ".".into() } else { path.clone() }, format!("{} under the startup directory", if path.is_empty() { "." } else { &path }))),
                Ok(_) => { wrong("a default is a fixed absolute path, or a home or cwd source"); continue }
                Err(why) => { wrong(&format!("default: {why}")); continue }
            },
            (Some(Value::Bool(false)), None) => { wrong("an optional argument states its `default` (a fixed path, home or cwd source): absent, it would otherwise grant nothing reviewable"); continue }
            (Some(Value::Bool(false)), Some(_)) => { wrong("only a path argument has a default; a descriptor is given or the launch is refused"); continue }
            (None | Some(Value::Bool(true)), Some(_)) => { wrong("a `default` belongs to an optional argument (`required = false`)"); continue }
            _ => { wrong("`required` is a Bool"); continue }
        };
        let secret = match table.get("secret") { None => false, Some(Value::Bool(b)) => *b, Some(_) => { wrong("`secret` is a Bool"); continue } };
        if secret && ty == FlagType::Path {
            wrong("a path argument is shown in diagnostics and process listings; a secret argument is a descriptor (`type = \"fd\"`)");
            continue;
        }
        flags.insert(name.clone(), Flag { ty, secret, used_by: None, default });
    }
    for (name, flag) in &flags {
        let (kind, value) = flag.default.as_ref().map(|(k, v, _)| (k.clone(), v.clone())).unwrap_or(("-".into(), "-".into()));
        if value.is_empty() || value.chars().any(char::is_control) { errors.push(format!("[flags].{name}: its default cannot be carried")); continue; }
        records.push(vec!["flag".into(), name.clone(), match flag.ty { FlagType::Path => "path", FlagType::Fd => "fd", FlagType::Object => "object" }.into(), if flag.secret { "1" } else { "0" }.into(), kind, value]);
    }

    // A source, encoded as its kind and value, with a review phrase.
    // `identity = { from = "arg", name = FLAG }` on a directory, read file or input: the object
    // opened must be the one a launcher checked, given as DEVICE:INODE (an `object` argument).
    let identity = |flags: &mut BTreeMap<String, Flag>, owner: &str, value: &Value| -> Result<Vec<String>, String> {
        let Ok(Source::Arg(flag_name)) = source(value) else { return Err("`identity` is `{ from = \"arg\", name = \"...\" }`, an argument of `type = \"object\"`".into()) };
        match flags.get_mut(&flag_name) {
            Some(flag) if flag.ty == FlagType::Object && flag.used_by.is_none() => flag.used_by = Some(owner.to_owned()),
            _ => return Err(format!("`identity` names --{flag_name}, which is not an unused argument of `type = \"object\"`")),
        }
        Ok(vec!["identity".into(), "arg".into(), flag_name])
    };
    let resolve = |flags: &mut BTreeMap<String, Flag>, owner: &str, value: &Value, want: Want| -> Result<(String, String, String), String> {
        let source = match (value, want) {
            // A secret's established spellings, `fd:N` and `env:NAME`.
            (Value::Text(text), Want::Secret { .. }) => match text.split_once(':') {
                Some(("fd", n)) if !n.starts_with(['0', '+']) && n.parse::<u32>().is_ok_and(|n| (3..=1023).contains(&n)) => Source::Fd(n.parse().unwrap()),
                Some(("env", name)) => source(&Value::Table(BTreeMap::from([("from".to_string(), Value::Text("env".into())), ("name".to_string(), Value::Text(name.into()))])))?,
                _ => return Err(format!("`{}` is not a source: `fd:N` (3 to 1023), `env:NAME` or a source table", text.escape_debug())),
            },
            _ => source(value)?,
        };
        Ok(match (source, want) {
            (Source::Literal(path), Want::Path | Want::Within) => { absolute_path(&path)?; let shown = path.clone(); ("literal".into(), field(&path)?, shown) }
            (Source::Literal(_), Want::Secret { .. }) => return Err("a secret's bytes are never written in a manifest: use a descriptor or a variable".into()),
            (Source::Arg(name), Want::Path | Want::Secret { .. }) => {
                let Some(flag) = flags.get_mut(&name) else { return Err(format!("`{{ from = \"arg\", name = \"{}\" }}` names no argument declared in [flags]", name.escape_debug())) };
                if let Some(other) = &flag.used_by { return Err(format!("--{name} is already {other}'s source: one argument reaches one grant")); }
                match (flag.ty, want) {
                    (FlagType::Path, Want::Path) => {}
                    (FlagType::Fd, Want::Path) if !flag.secret => {}
                    (FlagType::Fd, Want::Secret { .. }) if flag.secret => {}
                    (FlagType::Fd, Want::Secret { .. }) => return Err(format!("--{name} carries a secret's descriptor: declare it `secret = true`")),
                    (FlagType::Fd, _) => return Err(format!("--{name} is a secret argument and reaches only a secret")),
                    (FlagType::Path, _) => return Err(format!("--{name} is a path; a secret comes from a descriptor (`type = \"fd\"`)")),
                    (FlagType::Object, _) => return Err(format!("--{name} is an object identity; it is a secret's `object`")),
                }
                flag.used_by = Some(owner.to_owned());
                let shown = match flag.ty {
                    FlagType::Object => unreachable!(),
                    FlagType::Path if flag.default.is_some() => format!("the path given as --{name} (unrestricted: any path the operator can open), or when absent {}", flag.default.as_ref().unwrap().2),
                    FlagType::Path => format!("the path given as --{name} (unrestricted: any path the operator can open)"),
                    FlagType::Fd => format!("the descriptor numbered by --{name}"),
                };
                ("arg".into(), name, shown)
            }
            (Source::Arg(_), Want::Within) => return Err("`within` is the binary author's boundary; an argument cannot choose it".into()),
            (Source::Home(path), Want::Path | Want::Within) => {
                let shown = if path.is_empty() { "the launching account's home (account database)".to_string() } else { format!("{path} under the launching account's home (account database)") };
                ("home".into(), if path.is_empty() { ".".into() } else { field(&path)? }, shown)
            }
            (Source::Cwd(path), Want::Path | Want::Within) => {
                let shown = if path.is_empty() { "the startup directory".to_string() } else { format!("{path} under the startup directory") };
                ("cwd".into(), if path.is_empty() { ".".into() } else { field(&path)? }, shown)
            }
            (Source::Fd(n), Want::Path | Want::Secret { .. }) => ("fd".into(), n.to_string(), format!("inherited descriptor {n}")),
            (Source::Fd(_), Want::Within) => return Err("`within` is an absolute path, home or cwd".into()),
            (Source::Env(name), Want::Secret { signer: false }) => ("env".into(), name.clone(), format!("the variable {name}, taken out of the environment at launch")),
            (Source::Env(_), Want::Secret { signer: true }) => return Err("a signing key comes from a descriptor, never a variable".into()),
            (Source::Home(_) | Source::Cwd(_), Want::Secret { .. }) => return Err("a secret comes from a descriptor or a variable, not a path".into()),
            (Source::Env(_), _) => return Err("a variable is not a path source in launch plan version 1".into()),
        })
    };

    let mut used: BTreeMap<(&str, &str), bool> = BTreeMap::new();
    for (section, entries) in document {
        if matches!(section.as_str(), "launch" | "flags" | "paths" | "limits") { continue; }
        for key in entries.keys() { used.insert((section.as_str(), key.as_str()), false); }
    }
    let mut kinds: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut env_names: Vec<String> = Vec::new();
    let mut secret_sources: Vec<(String, String, String)> = Vec::new();
    let mut store_names: Vec<String> = Vec::new();
    for parameter in parameters {
        let (name, ty) = (parameter.name.as_str(), parameter.ty.as_str());
        kinds.entry(if parameter.grantable { "grantable" } else { ty }).or_default().push(name);
        if ty == "Console<Write>" { records.push(vec!["output".into()]); review.push(format!("{name}: Console<Write>, the executable's standard output")); continue; }
        if ty == "Allocator" { records.push(vec!["allocator".into()]); review.push(format!("{name}: Allocator, heap memory")); continue; }
        let section = if parameter.grantable { "user_caps" } else { match ty {
            "Dir" | "Dir<Read>" | "Dir<Write>" => "dirs", "File<Read>" | "File<Write>" => "files", "Net" => "net", "Listener" => "listen",
            "Env" => "env", "Secret<Seal>" | "Secret<Reveal>" | "Signer" | "TlsSigner" | "SecretStore" => "secrets", "Clock" => "clock", "Rand" => "rand",
            "Console<Read>" => "input", "Args" => "args",
            "Exec" => "exec",
            "Socket" => "listen",
            other => { errors.push(format!("{name}: {other} cannot be granted by a launch manifest")); continue }
        } };
        let Some(value) = document.get(section).and_then(|entries| entries.get(name)) else {
            errors.push(format!("{name}: main takes a {ty}, and the manifest grants none ([{section}].{name})"));
            continue;
        };
        used.insert((section, name), true);
        let mut wrong = |what: &str| errors.push(format!("[{section}].{name}: {what}"));
        match (section, value) {
            ("dirs", Value::Table(table)) => {
                if let Some(other) = table.keys().find(|k| !matches!(k.as_str(), "root" | "rights" | "policy" | "within" | "identity")) { wrong(&format!("a directory has `root`, `rights`, `policy`, `within` and `identity`, not `{other}`")); continue; }
                let (rights, code): (&[&str], &str) = match ty { "Dir<Read>" => (&["Read"], "read"), "Dir<Write>" => (&["Write"], "write"), _ => (&["Read", "Write"], "all") };
                if let Some(stated) = table.get("rights") {
                    let said: Option<Vec<&str>> = match stated { Value::List(items) => items.iter().map(|i| if let Value::Text(t) = i { Some(t.as_str()) } else { None }).collect(), _ => None };
                    let mut said = said.unwrap_or_default();
                    said.sort();
                    if said != rights { wrong(&format!("main's {ty} has the rights {rights:?}, and the manifest states {said:?}")); continue; }
                }
                let policy = match table.get("policy") {
                    None => "any",
                    Some(Value::Text(p)) if p == "data-only" => "data-only",
                    Some(_) => { wrong("`policy` is \"data-only\" (plain data names, no dot components, protected names kept from writes) or absent"); continue }
                };
                let Some(root) = table.get("root") else { wrong("a directory states its `root`"); continue };
                let root = match resolve(&mut flags, name, root, Want::Path) { Ok(r) => r, Err(why) => { wrong(&why); continue } };
                let within = match table.get("within") {
                    None => None,
                    Some(_) if root.0 != "arg" || flags.get(&root.1).is_some_and(|f| f.ty != FlagType::Path) => { wrong("`within` restricts a path the operator chooses (`root = { from = \"arg\", ... }` of a path argument)"); continue }
                    Some(value) => match resolve(&mut flags, name, value, Want::Within) { Ok(w) => Some(w), Err(why) => { wrong(&format!("within: {why}")); continue } },
                };
                let shown_root = match &within { Some(w) if root.0 == "arg" => format!("the directory given as --{} (restricted to {})", root.1, w.2), _ => root.2.clone() };
                review.push(format!("{name}: {ty} at {shown_root}{}", if policy == "data-only" { ", data-only names" } else { "" }));
                let (wk, wv) = within.map(|w| (w.0, w.1)).unwrap_or(("-".into(), "-".into()));
                records.push(vec!["dir".into(), code.into(), policy.into(), root.0, root.1, wk, wv]);
                if let Some(value) = table.get("identity") {
                    match identity(&mut flags, name, value) { Ok(record) => { review.push(format!("{name}: opened only if it is the object given as --{} (DEVICE:INODE)", record[2])); records.push(record) } Err(why) => wrong(&why) }
                }
            }
            ("files", _) | ("input", _) => {
                // A file: a literal path, a source table, or `{ path = SOURCE, within = SOURCE }`.
                let stdin = matches!(value, Value::Table(t) if t.len() == 1 && t.get("stdin") == Some(&Value::Bool(true)));
                if section == "input" && stdin {
                    records.push(vec!["input".into(), "stdin".into(), "-".into(), "-".into(), "-".into()]);
                    review.push(format!("{name}: Console<Read>, from the executable's standard input"));
                    continue;
                }
                let writes = ty == "File<Write>";
                let mut mode = "replace";
                let (path, within) = match value {
                    // A source table (`from`) may itself have a `path` (home, cwd).
                    Value::Table(t) if t.contains_key("path") && !t.contains_key("from") => {
                        if let Some(other) = t.keys().find(|k| *k != "path" && *k != "within" && !(writes && *k == "mode") && !(!writes && *k == "identity")) { wrong(&format!("a file is `{{ path = SOURCE, within = SOURCE{} }}`, without `{other}`", if writes { ", mode = \"replace\" | \"create\"" } else { "" })); continue; }
                        match t.get("mode") {
                            None => {}
                            Some(Value::Text(m)) if m == "replace" || m == "create" => mode = if m == "create" { "create" } else { "replace" },
                            Some(_) => { wrong("a written file's `mode` is \"replace\" (made or replaced whole when closed) or \"create\" (made only if absent)"); continue }
                        }
                        (&t["path"], t.get("within"))
                    }
                    Value::Text(_) | Value::Table(_) => (value, None),
                    _ => { wrong(if section == "input" { "a Console<Read> is `{ stdin = true }`, a path or a source" } else { "a file is a path or a source" }); continue }
                };
                let path = match resolve(&mut flags, name, path, Want::Path) { Ok(p) => p, Err(why) => { wrong(&why); continue } };
                if writes && (path.0 == "fd" || (path.0 == "arg" && flags.get(&path.1).is_some_and(|f| f.ty == FlagType::Fd))) { wrong("a written file is named by a path, so it can be made or replaced in its directory"); continue; }
                let within = match within {
                    None => None,
                    Some(_) if path.0 != "arg" || flags.get(&path.1).is_some_and(|f| f.ty != FlagType::Path) => { wrong("`within` restricts a path the operator chooses (a path argument)"); continue }
                    Some(value) => match resolve(&mut flags, name, value, Want::Within) { Ok(w) => Some(w), Err(why) => { wrong(&format!("within: {why}")); continue } },
                };
                let shown = match &within { Some(w) => format!("the file given as --{} (restricted to {}, one link)", path.1, w.2), None => path.2.clone() };
                let (wk, wv) = within.map(|w| (w.0, w.1)).unwrap_or(("-".into(), "-".into()));
                if writes {
                    review.push(format!("{name}: {ty}, written to {shown} ({})", if mode == "create" { "made only if absent" } else { "made or replaced whole when closed" }));
                    records.push(vec!["write-file".into(), mode.into(), path.0, path.1, wk, wv]);
                } else {
                    review.push(format!("{name}: {ty}, read from {shown}"));
                    records.push(vec![if section == "input" { "input" } else { "file" }.into(), path.0, path.1, wk, wv]);
                    if let Value::Table(t) = value && let Some(bound) = t.get("identity").filter(|_| t.contains_key("path") && !t.contains_key("from")) {
                        match identity(&mut flags, name, bound) { Ok(record) => { review.push(format!("{name}: opened only if it is the object given as --{} (DEVICE:INODE)", record[2])); records.push(record) } Err(why) => wrong(&why) }
                    }
                }
            }
            ("clock" | "rand", Value::Bool(true)) => { records.push(vec![section.into()]); review.push(format!("{name}: {ty}")); }
            ("env", Value::List(items)) if !items.is_empty() && items.iter().all(|i| matches!(i, Value::Text(t) if super::plain_variable(t))) => {
                let names: Vec<String> = items.iter().map(|i| if let Value::Text(t) = i { t.clone() } else { unreachable!() }).collect();
                if (1..names.len()).any(|i| names[..i].contains(&names[i])) { wrong("a variable is listed once"); continue; }
                for variable in &names { records.push(vec!["env".into(), variable.clone()]); env_names.push(variable.clone()); }
                review.push(format!("{name}: Env, reading only {}", names.join(", ")));
            }
            ("env", _) => wrong("an Env is a list of variable names (`[\"LANG\"]`)"),
            ("args", _) => {
                let limits = match value {
                    Value::Bool(true) => Some((16, 4096)),
                    Value::Table(table) if !table.is_empty() && table.keys().all(|k| k == "count" || k == "max-bytes") => {
                        let number = |key: &str, default: u64, most: u64| match table.get(key) { None => Some(default), Some(Value::Number(n)) if (0..=most).contains(n) => Some(*n), _ => None };
                        number("count", 16, 256).zip(number("max-bytes", 4096, 65_536))
                    }
                    _ => None,
                };
                let Some((count, bytes)) = limits else { wrong("an Args is `true` or `{ count = 16, max-bytes = 4096 }` (at most 256 words and 65536 bytes)"); continue };
                records.push(vec!["args".into(), count.to_string(), bytes.to_string()]);
                review.push(format!("{name}: Args, at most {count} words and {bytes} bytes, the words after `--`"));
            }
            ("net", Value::List(items)) if !items.is_empty() && items.len() <= 16 && items.iter().all(|i| matches!(i, Value::Text(t) if !t.contains(',') && !t.chars().any(char::is_control) && t.rsplit_once(':').is_some_and(|(h, p)| !h.is_empty() && p.parse::<u16>().is_ok()))) => {
                let endpoints: Vec<String> = items.iter().map(|i| if let Value::Text(t) = i { t.clone() } else { unreachable!() }).collect();
                for endpoint in &endpoints { records.push(vec!["net".into(), endpoint.clone()]); }
                review.push(format!("{name}: Net, dialing only {}", endpoints.join(", ")));
            }
            // `main(connection: Socket)`: the executable listens and runs `main` once for each
            // connection it accepts, in a new sandboxed worker.
            ("listen", Value::Text(address)) if ty == "Socket" => match listener(address) {
                Some((flag, shown)) => { records.push(vec!["socket".into(), flag]); review.push(format!("{name}: Socket, one connection accepted on {shown}; main runs once for each, in a new worker")); }
                None => wrong("a connection is accepted on `\"HOST:PORT\"`, an IPv6 address in brackets (`\"[::1]:8080\"`)"),
            },
            ("listen", Value::Text(address)) => match listener(address) {
                Some((flag, shown)) => { records.push(vec!["listen".into(), flag]); review.push(format!("{name}: Listener on {shown}")); }
                None => wrong("a listener is `\"HOST:PORT\"`, an IPv6 address in brackets (`\"[::1]:8080\"`)"),
            },
            ("secrets", Value::Table(table)) if ty == "SecretStore" => {
                let mut granted = Vec::new();
                for (secret, how) in table {
                    if !super::plain_variable(secret) { wrong(&format!("`{}` is not a variable name", secret.escape_debug())); continue; }
                    match how {
                        Value::Text(t) if t == "reveal" || t == "seal" => {
                            records.push(vec!["store".into(), secret.clone(), t.clone()]);
                            store_names.push(secret.clone());
                            granted.push(format!("{secret} ({})", if t == "reveal" { "read and sign" } else { "sign only" }));
                        }
                        _ => wrong(&format!("the secret {secret} is granted as \"reveal\" or \"seal\"")),
                    }
                }
                review.push(format!("{name}: SecretStore, {}; values from the launching environment's variables of those names, taken out at launch", granted.join(", ")));
            }
            ("secrets", Value::Table(table)) => {
                if let Some(other) = table.keys().find(|k| *k != "use" && *k != "from" && *k != "object") {
                    wrong(&format!("a fixed secret in a launch manifest has `use` and `from`, not `{other}`{}", if other == "also_from" { " (a plan names one source)" } else { "" }));
                    continue;
                }
                let use_ = match secret_use(ty, table) { Ok(u) => u, Err(why) => { wrong(&why); continue } };
                let Some(from) = table.get("from") else { wrong("a fixed secret states where it comes from (`from`)"); continue };
                let from = match resolve(&mut flags, name, from, Want::Secret { signer: matches!(ty, "Signer" | "TlsSigner") }) { Ok(f) => f, Err(why) => { wrong(&why); continue } };
                let what = if use_ == TLS_SIGN { "signs TLS 1.3 server CertificateVerify transcripts".to_owned() }
                else if use_ == "seal" { "signs with the secret".to_string() } else if let Some(role) = use_.strip_prefix(SIGN_PREFIX) {
                    format!("signs `witchy/v2/{role}` objects (ML-DSA-65, witchy-2) with the key")
                } else { "reads the bytes of the secret".to_string() };
                // The file a launcher read this secret from, when it passes the bytes on a pipe:
                // no other grant may reach that file during the run.
                if let Some(object) = table.get("object") {
                    let Ok(Source::Arg(flag_name)) = source(object) else { wrong("`object` is `{ from = \"arg\", name = \"...\" }`, an argument of `type = \"object\"`"); continue };
                    match flags.get_mut(&flag_name) {
                        Some(flag) if flag.ty == FlagType::Object && flag.used_by.is_none() => flag.used_by = Some(name.to_owned()),
                        _ => { wrong(&format!("`object` names --{flag_name}, which is not an unused argument of `type = \"object\"`")); continue }
                    }
                    records.push(vec!["object".into(), "arg".into(), flag_name.clone()]);
                    review.push(format!("{name}: the file given as --{flag_name} (DEVICE:INODE) is refused to every other grant"));
                }
                review.push(format!("{name}: {ty}, {what} from {}", from.2));
                secret_sources.push((name.to_owned(), from.0.clone(), from.1.clone()));
                records.push(vec!["secret".into(), name.to_owned(), use_, from.0, from.1]);
            }
            // The programs are the binary author's, by name; the directory is the author's
            // too (no argument chooses it), opened without a link, and each program in it is
            // opened when the executable starts.
            ("exec", Value::Table(table)) => {
                if let Some(other) = table.keys().find(|k| !matches!(k.as_str(), "dir" | "programs" | "env")) { wrong(&format!("programs are `{{ dir = SOURCE, programs = [\"name\"], env = [\"NAME\"] }}`, without `{other}`")); continue; }
                let env = match super::exec_env(table.get("env")) { Ok(env) => env, Err(why) => { wrong(&why); continue } };
                let (Some(dir), Some(Value::List(programs))) = (table.get("dir"), table.get("programs")) else { wrong("programs are `{ dir = SOURCE, programs = [\"name\"] }`"); continue };
                let names: Option<Vec<String>> = programs.iter().map(|p| match p {
                    Value::Text(t) if !t.is_empty() && t.len() <= 255 && t != "." && t != ".." && !t.contains(['/', '\\', ',', ':']) && !t.chars().any(|c| c.is_control() || c.is_whitespace()) => Some(t.clone()),
                    _ => None,
                }).collect();
                let Some(names) = names.filter(|n| !n.is_empty() && n.len() <= 16) else { wrong("1 to 16 programs, each a plain name in its directory"); continue };
                if (1..names.len()).any(|i| names[..i].contains(&names[i])) { wrong("a program is listed once"); continue; }
                if matches!(dir, Value::Table(t) if t.get("from") == Some(&Value::Text("arg".into()))) { wrong("the programs' directory is the binary author's: an argument cannot choose it"); continue; }
                let dir = match resolve(&mut flags, name, dir, Want::Path) { Ok(d) => d, Err(why) => { wrong(&why); continue } };
                let environment = if env.is_empty() { "no environment".to_string() } else { format!("only the variables {} (values from the program, which can change what the program does)", env.join(", ")) };
                review.push(format!("{name}: Exec, running only {} from {} (each opened at start, run with {environment}, no input unless given and its own session and process group)", names.join(", "), dir.2));
                records.push(vec!["exec".into(), dir.0, dir.1, names.join(",")]);
                if !env.is_empty() { records.push(vec!["exec-env".into(), env.join(",")]); }
            }
            ("user_caps", Value::Table(table)) => {
                if table.get("type") != Some(&Value::Text(ty.to_string())) { wrong(&format!("main takes a {ty}: the entry says `type = \"{ty}\"`")); continue; }
                let mut fields = Vec::new();
                for (field, value) in table.iter().filter(|(k, _)| *k != "type") {
                    let shown = match value { Value::Number(n) => n.to_string(), Value::Bool(b) => b.to_string(), _ => { wrong(&format!("{field} is a number or a Bool")); continue } };
                    records.push(vec!["grant".into(), format!("{ty}.{field}={shown}")]);
                    fields.push(format!("{field} = {shown}"));
                }
                review.push(format!("{name}: {ty} (user capability), {}", fields.join(", ")));
            }
            _ => wrong(&format!("this is not how [{section}] grants a {ty}")),
        }
    }
    // Launch limits: each the development flag of that name, only for a grant main takes.
    let takes = |kinds_: &[&str]| kinds_.iter().any(|k| kinds.contains_key(k));
    let mut shown_limits = Vec::new();
    for (key, value) in document.get("limits").into_iter().flatten() {
        let (applies, most): (bool, u64) = match key.as_str() {
            "deadline-ms" => (true, u64::from(u32::MAX)),
            "output-bytes" => (takes(&["Console<Write>"]), 1 << 40),
            "input-bytes" => (takes(&["Console<Read>"]), 1 << 40),
            "fs-bytes" => (takes(&["File<Read>", "File<Write>", "Dir", "Dir<Read>", "Dir<Write>"]), 1 << 40),
            "fs-entries" => (takes(&["File<Write>", "Dir", "Dir<Write>"]), 1 << 40),
            "net-bytes" => (takes(&["Net", "Listener", "Socket"]), 1 << 40),
            "exec-bytes" => (takes(&["Exec"]), 256 << 20),
            // Linux: each program's RLIMIT_NPROC is the user's process count when it starts plus
            // this (not enforced for root, which the kernel exempts).
            "exec-processes" => (takes(&["Exec"]), 65_536),
            "connections" | "net-bytes-per-connection" => (takes(&["Socket"]), 1 << 40),
            "waiting-connections" => (takes(&["Socket"]), 4096),
            "connection-deadline-ms" => (takes(&["Socket"]), 86_400_000),
            "output-bytes-per-connection" => (takes(&["Socket"]) && takes(&["Console<Write>"]), 1 << 40),
            // Beside the run's own: each connection's worker and broker start with this much.
            "fs-bytes-per-connection" => (takes(&["Socket"]) && takes(&["File<Read>", "Dir", "Dir<Read>", "Dir<Write>"]), 1 << 40),
            "fs-entries-per-connection" => (takes(&["Socket"]) && takes(&["Dir", "Dir<Write>"]), 1 << 40),
            _ => { errors.push(format!("[limits].{key} is not a launch limit (deadline-ms, output-bytes, input-bytes, fs-bytes, fs-entries, net-bytes, exec-bytes, exec-processes, connections, waiting-connections, connection-deadline-ms, net-bytes-per-connection, output-bytes-per-connection, fs-bytes-per-connection, fs-entries-per-connection)")); continue }
        };
        if !applies { errors.push(format!("[limits].{key} limits a grant main does not take")); continue; }
        let positive = matches!(key.as_str(), "deadline-ms" | "connections" | "waiting-connections" | "connection-deadline-ms" | "exec-processes");
        match value {
            Value::Number(n) if *n <= most && (!positive || *n > 0) => {
                records.push(vec!["limit".into(), key.clone(), n.to_string()]);
                shown_limits.push(format!("{key} = {n}"));
            }
            _ => errors.push(format!("[limits].{key} is a number from {} to {most}", u8::from(positive))),
        }
    }
    if takes(&["Exec"]) && !shown_limits.iter().any(|l| l.starts_with("exec-processes ")) { shown_limits.push("exec-processes = 256 (default)".into()); }
    if !shown_limits.is_empty() { review.push(format!("limits: {}", shown_limits.join(", "))); }
    // Each connection runs in a new worker from a supervisor that never ran guest code, with a
    // broker of its own: a read file and the input are read once at launch and each worker reads
    // its own copy from the start; outbound endpoints and programs are the launch's. A written
    // file is one object every connection would share. TlsSigner handles are
    // usable in workers. Each signs through the fixed TLS server operation.
    if kinds.contains_key("Socket") && kinds.keys().any(|k| !matches!(*k, "Socket" | "Console<Write>" | "Allocator" | "Clock" | "Rand" | "Args" | "Env" | "grantable" | "Dir" | "Dir<Read>" | "Dir<Write>" | "File<Read>" | "Console<Read>" | "Net" | "Exec" | "TlsSigner")) {
        errors.push("a socket-entry server takes only directories, File<Read>, Console<Read>, Console<Write>, Net, Exec, Allocator, Clock, Rand, Args, Env, TlsSigner and user capabilities beside its Socket".into());
    }
    if kinds.contains_key("Socket") && kinds.contains_key("Console<Read>") {
        let bounded = records.iter().any(|r| r[0] == "limit" && r[1] == "input-bytes" && r[2].parse::<u64>().is_ok_and(|n| n <= 16 << 20));
        if !bounded {
            errors.push("a socket-entry server reads its input once, before the first connection: [limits].input-bytes (at most 16777216) is required".into());
        }
    }
    // The native runtime binds one grant of each of these kinds, so two parameters of one kind
    // would share one resource.
    for (kind, names) in &kinds {
        if names.len() > 1 && !matches!(*kind, "Secret<Seal>" | "Secret<Reveal>" | "Signer" | "TlsSigner" | "grantable" | "Console<Write>" | "Allocator") {
            errors.push(format!("main takes {} parameters of type {kind} ({}); a native launch binds one", names.len(), names.join(", ")));
        }
    }
    let dirs: Vec<&str> = ["Dir", "Dir<Read>", "Dir<Write>"].into_iter().filter(|k| kinds.contains_key(k)).collect();
    if dirs.len() > 1 && dirs != ["Dir<Read>", "Dir<Write>"] {
        errors.push(format!("main takes a {}; a native launch combines only one Dir<Read> with one Dir<Write>", dirs.join(" and a ")));
    }
    if dirs.len() > 1 && records.iter().any(|r| r[0] == "dir" && r[2] == "data-only") {
        errors.push("a data-only directory is the executable's one directory grant".into());
    }
    let secrets = kinds.keys().any(|k| matches!(*k, "Secret<Seal>" | "Secret<Reveal>" | "Signer" | "SecretStore"));
    // A read file and an input are single objects opened at launch and compared with the
    // secrets' sources there; a directory reaches objects later, by name.
    // Every open through a directory, to read or to append, is compared with the secrets'
    // objects on the opened descriptor before a byte moves; a replace, a create and a rename
    // write only new files and move names. So writable directories and written files are
    // allowed beside secrets (docs/native-trusted-executables.md, "Secrets").
    // A program Exec starts runs unsandboxed as the user: it could read a secret's file, or the
    // process's environment as other processes see it (review of 5fccb36f).
    if kinds.contains_key("TlsSigner") && kinds.keys().any(|k| matches!(*k, "Exec" | "Dir" | "Dir<Write>" | "File<Write>")) {
        errors.push("TLS signers require a launch without Exec or writable file and directory grants".into());
    } else if secrets && kinds.contains_key("Exec") {
        errors.push("native secrets cannot be combined with Exec: a program it starts runs outside the sandbox with the user's authority".into());
    }
    // One source, one route.
    for (i, (slot, kind, value)) in secret_sources.iter().enumerate() {
        if let Some((other, _, _)) = secret_sources[..i].iter().find(|(_, k, v)| k == kind && v == value) {
            errors.push(format!("[secrets].{slot}: its source is also [secrets].{other}'s: one source reaches one slot"));
        }
        if kind == "env" && env_names.contains(value) { errors.push(format!("[secrets].{slot}: env:{value} is also granted through [env]: a secret's variable is not ordinary environment")); }
        if kind == "env" && store_names.contains(value) { errors.push(format!("[secrets].{slot}: env:{value} is also a SecretStore's secret {value}: one source reaches one slot")); }
    }
    for name in &store_names {
        if env_names.contains(name) { errors.push(format!("the SecretStore's secret {name} is also granted through [env]")); }
    }
    for (name, flag) in &flags {
        if flag.used_by.is_none() { errors.push(format!("[flags].{name}: --{name} is declared and no grant takes it")); }
    }
    for ((section, key), taken) in &used {
        if !taken { errors.push(format!("[{section}].{key} is granted, and main takes no parameter `{key}` of that kind: a launch plan grants nothing extra")); }
    }
    // The runtime reader's bounds, so a plan built is a plan it reads.
    if flags.len() > 32 { errors.push(format!("{} declared arguments; a launch plan has at most 32", flags.len())); }
    if records.len() > 512 { errors.push(format!("{} launch plan records; a launch plan has at most 512", records.len())); }
    if !errors.is_empty() { return Err(errors); }
    let requires_aws_lc = records.iter().any(|record| matches!(record[0].as_str(), "secret" | "store" | "object"));
    let mut wire = format!("witchy-launch-plan\t{PLAN_VERSION}\t{digest}\n");
    for record in &records {
        for value in record {
            if value.is_empty() || value.contains(['\t', '\n', '\0']) || value.chars().any(char::is_control) {
                return Err(vec![format!("`{}` cannot be carried in a launch plan", value.escape_debug())]);
            }
        }
        wire.push_str(&record.join("\t"));
        wire.push('\n');
    }
    wire.push_str("end\n");
    if wire.len() > MAX_PLAN_BYTES { return Err(vec![format!("the launch plan is {} bytes, past its {MAX_PLAN_BYTES}-byte limit", wire.len())]); }
    Ok(LaunchPlan { wire: wire.into_bytes(), review, requires_aws_lc })
}

#[cfg(test)]
mod tests {
    use super::super::{parse, Parameter};
    use super::plan;
    fn parameter(name: &str, ty: &str) -> Parameter { Parameter { name: name.into(), ty: ty.into(), grantable: false } }
    const DIGEST: &str = "00";
    fn planned(text: &str, parameters: &[Parameter]) -> Result<String, String> {
        plan(&parse(text).map_err(|e| e.to_string())?, parameters, DIGEST)
            .map(|p| String::from_utf8(p.wire).unwrap()).map_err(|e| e.join("\n"))
    }

    #[test]
    fn hello_needs_no_entry_and_no_flag() {
        let wire = planned("[launch]\nversion = 1\n", &[parameter("out", "Console<Write>")]).unwrap();
        assert_eq!(wire, "witchy-launch-plan\t1\t00\noutput\nend\n");
        assert!(planned("", &[parameter("out", "Console<Write>")]).unwrap_err().contains("starts with `[launch]`"));
        assert!(planned("[launch]\nversion = 2\n", &[]).unwrap_err().contains("version = 2 is not known"));
    }

    #[test]
    fn a_directory_argument_binds_one_declared_flag() {
        let text = "[launch]\nversion = 1\n[flags]\nproject = { type = \"path\", required = true }\n[dirs]\nproject = { root = { from = \"arg\", name = \"project\" }, rights = [\"Read\"] }\n";
        let parameters = [parameter("project", "Dir<Read>"), parameter("out", "Console<Write>")];
        let wire = planned(text, &parameters).unwrap();
        assert_eq!(wire, "witchy-launch-plan\t1\t00\nflag\tproject\tpath\t0\t-\t-\ndir\tread\tany\targ\tproject\t-\t-\noutput\nend\n");
        let shown = plan(&parse(text).unwrap(), &parameters, DIGEST).unwrap().review;
        assert!(shown[1].contains("unrestricted"), "{shown:?}");
        let within = text.replace("rights = [\"Read\"] }", "rights = [\"Read\"], policy = \"data-only\", within = { from = \"home\", path = \"Projects\" } }");
        let wire = planned(&within, &parameters).unwrap();
        assert!(wire.contains("dir\tread\tdata-only\targ\tproject\thome\tProjects\n"), "{wire}");
        assert!(plan(&parse(&within).unwrap(), &parameters, DIGEST).unwrap().review[1].contains("restricted to Projects under the launching account's home"));
    }

    #[test]
    fn a_manifest_that_does_not_fit_main_is_refused() {
        let head = "[launch]\nversion = 1\n";
        let dir = [parameter("data", "Dir<Read>")];
        let refused = |body: &str, parameters: &[Parameter]| planned(&format!("{head}{body}"), parameters).unwrap_err();
        // Missing, extra, unused argument, undeclared argument, an argument used twice.
        assert!(refused("", &dir).contains("grants none"));
        assert!(refused("[dirs]\ndata = { root = \"/srv\" }\n[clock]\nclock = true\n", &dir).contains("grants nothing extra"));
        assert!(refused("[flags]\nx = { type = \"path\" }\n[dirs]\ndata = { root = \"/srv\" }\n", &dir).contains("no grant takes it"));
        assert!(refused("[dirs]\ndata = { root = { from = \"arg\", name = \"x\" } }\n", &dir).contains("names no argument"));
        assert!(refused("[flags]\nx = { type = \"path\" }\n[dirs]\ndata = { root = { from = \"arg\", name = \"x\" } }\n[files]\nf = { from = \"arg\", name = \"x\" }\n", &[parameter("data", "Dir<Read>"), parameter("f", "File<Read>")]).contains("one argument reaches one grant"));
        // Wider rights, a relative literal, a link-following path policy, unknown source and field.
        assert!(refused("[dirs]\ndata = { root = \"/srv\", rights = [\"Read\", \"Write\"] }\n", &dir).contains("has the rights"));
        assert!(refused("[dirs]\ndata = { root = \"srv\" }\n", &dir).contains("is absolute"));
        assert!(refused("[dirs]\ndata = { root = \"/srv/../etc\" }\n", &dir).contains("`..`"));
        assert!(refused("[paths]\nlinks = true\n[dirs]\ndata = { root = \"/srv\" }\n", &dir).contains("never follows a link"));
        assert!(refused("[dirs]\ndata = { root = { from = \"url\", value = \"x\" } }\n", &dir).contains("is not a source"));
        assert!(refused("[dirs]\ndata = { root = { from = \"home\", path = \"../x\" } }\n", &dir).contains("`..`"));
        assert!(refused("[dirs]\ndata = { root = { from = \"env\", name = \"DATA\" } }\n", &dir).contains("not a path source"));
        assert!(refused("[dirs]\ndata = { root = \"/srv\", mode = 1 }\n", &dir).contains("not `mode`"));
        // `within` only for an argument, and never chosen by one.
        assert!(refused("[dirs]\ndata = { root = \"/srv\", within = \"/\" }\n", &dir).contains("restricts a path the operator chooses"));
        assert!(refused("[flags]\nx = { type = \"path\" }\ny = { type = \"path\" }\n[dirs]\ndata = { root = { from = \"arg\", name = \"x\" }, within = { from = \"arg\", name = \"y\" } }\n", &dir).contains("cannot choose it"));
        // Reserved, optional and secret path arguments.
        assert!(refused("[flags]\noutput = { type = \"path\" }\n[dirs]\ndata = { root = { from = \"arg\", name = \"output\" } }\n", &dir).contains("development launch flag"));
        assert!(refused("[flags]\nx = { type = \"path\", required = false }\n[dirs]\ndata = { root = { from = \"arg\", name = \"x\" } }\n", &dir).contains("states its `default`"));
        assert!(refused("[flags]\nx = { type = \"fd\", required = false, default = \"/srv\" }\n[dirs]\ndata = { root = { from = \"arg\", name = \"x\" } }\n", &dir).contains("only a path argument has a default"));
        let optional = planned(&format!("{head}[flags]\nx = {{ type = \"path\", required = false, default = {{ from = \"home\", path = \"notes\" }} }}\n[dirs]\ndata = {{ root = {{ from = \"arg\", name = \"x\" }} }}\n"), &dir).unwrap();
        assert!(optional.contains("flag\tx\tpath\t0\thome\tnotes\n"), "{optional}");
        // Limits name what main takes.
        let limited = planned(&format!("{head}[limits]\ndeadline-ms = 5000\nfs-bytes = 64\n[dirs]\ndata = {{ root = \"/srv\" }}\n"), &dir).unwrap();
        assert!(limited.contains("limit\tdeadline-ms\t5000\n") && limited.contains("limit\tfs-bytes\t64\n"), "{limited}");
        assert!(refused("[limits]\nnet-bytes = 64\n[dirs]\ndata = { root = \"/srv\" }\n", &dir).contains("limits a grant main does not take"));
        assert!(refused("[limits]\nfuel = 64\n[dirs]\ndata = { root = \"/srv\" }\n", &dir).contains("not a launch limit"));
        assert!(refused("[limits]\ndeadline-ms = 0\n[dirs]\ndata = { root = \"/srv\" }\n", &dir).contains("deadline-ms"));
        // A socket-entry server: outbound Net, a read file, input and Exec beside its Socket,
        // per-connection file budgets; a written file and an unbounded input refused.
        let server = [parameter("connection", "Socket"), parameter("api", "Net"), parameter("data", "Dir<Write>")];
        let served = planned(&format!("{head}[listen]\nconnection = \"127.0.0.1:1\"\n[net]\napi = [\"a.example:1\"]\n[dirs]\ndata = {{ root = \"/srv\" }}\n[limits]\nfs-bytes-per-connection = 64\nfs-entries-per-connection = 2\n"), &server).unwrap();
        assert!(served.contains("limit\tfs-bytes-per-connection\t64\n") && served.contains("limit\tfs-entries-per-connection\t2\n"), "{served}");
        assert!(refused("[listen]\nconnection = \"127.0.0.1:1\"\n[files]\nout = { path = \"/srv/o\" }\n", &[parameter("connection", "Socket"), parameter("out", "File<Write>")]).contains("socket-entry server"));
        assert!(refused("[listen]\nconnection = \"127.0.0.1:1\"\n[input]\nin = { stdin = true }\n", &[parameter("connection", "Socket"), parameter("in", "Console<Read>")]).contains("input-bytes"));
        assert!(refused("[limits]\nfs-entries-per-connection = 2\n[dirs]\ndata = { root = \"/srv\" }\n", &dir).contains("limits a grant main does not take"));
        assert!(refused("[flags]\nx = { type = \"path\", secret = true }\n[dirs]\ndata = { root = { from = \"arg\", name = \"x\" } }\n", &dir).contains("a secret argument is a descriptor"));
        // Kinds the runtime cannot bind or keep apart.
        assert!(refused("", &[parameter("connection", "Socket")]).contains("grants none"));
        assert!(refused("[files]\nout = { from = \"fd\", number = 4 }\n", &[parameter("out", "File<Write>")]).contains("named by a path"));
        assert!(refused("[files]\nout = { path = \"/tmp/x\", mode = \"append\" }\n", &[parameter("out", "File<Write>")]).contains("`mode`"));
        assert!(refused("[flags]\nd = { type = \"path\" }\n[exec]\nrun = { dir = { from = \"arg\", name = \"d\" }, programs = [\"git\"] }\n", &[parameter("run", "Exec")]).contains("an argument cannot choose it"));
        assert!(refused("[exec]\nrun = { dir = \"/usr/bin\", programs = [\"../sh\"] }\n", &[parameter("run", "Exec")]).contains("plain name"));
        assert!(refused("[exec]\nrun = { dir = \"/usr/bin\", programs = [\"git\", \"git\"] }\n", &[parameter("run", "Exec")]).contains("listed once"));
        assert!(refused("[dirs]\na = { root = \"/a\" }\nb = { root = \"/b\" }\n", &[parameter("a", "Dir<Read>"), parameter("b", "Dir<Read>")]).contains("a native launch binds one"));
        assert!(refused("[dirs]\na = { root = \"/a\" }\nb = { root = \"/b\" }\n", &[parameter("a", "Dir"), parameter("b", "Dir<Read>")]).contains("combines only one Dir<Read> with one Dir<Write>"));
        // A directory bound to a launcher's identity argument.
        let bound = planned(&format!("{head}[flags]\nroot-id = {{ type = \"object\" }}\n[dirs]\ndata = {{ root = \"/srv\", identity = {{ from = \"arg\", name = \"root-id\" }} }}\n"), &dir).unwrap();
        assert!(bound.contains("dir\tread\tany\tliteral\t/srv\t-\t-\nidentity\targ\troot-id\n"), "{bound}");
        assert!(refused("[flags]\nroot-id = { type = \"path\" }\n[dirs]\ndata = { root = \"/srv\", identity = { from = \"arg\", name = \"root-id\" } }\n", &dir).contains("type = \"object\""));
        // One object argument serves one grant: two roots, or a root and a secret, refused.
        let two = [parameter("data", "Dir<Read>"), parameter("cfg", "File<Read>")];
        assert!(refused("[flags]\nroot-id = { type = \"object\" }\n[dirs]\ndata = { root = \"/srv\", identity = { from = \"arg\", name = \"root-id\" } }\n[files]\ncfg = { path = \"/etc/x\", identity = { from = \"arg\", name = \"root-id\" } }\n", &two).contains("not an unused argument"));
        let keyed = [parameter("data", "Dir<Read>"), parameter("k", "Secret<Seal>")];
        assert!(refused("[flags]\nid = { type = \"object\" }\n[dirs]\ndata = { root = \"/srv\", identity = { from = \"arg\", name = \"id\" } }\n[secrets]\nk = { use = \"seal\", from = \"fd:3\", object = { from = \"arg\", name = \"id\" } }\n", &keyed).contains("not an unused argument"));
        assert!(refused("[flags]\nid = { type = \"object\" }\n[dirs]\ndata = { root = { from = \"arg\", name = \"id\" }, identity = { from = \"arg\", name = \"id\" } }\n", &dir).len() > 0);
        // A secret beside a writable directory: planned; the runtime keeps them apart by identity.
        assert!(planned(&format!("{head}[dirs]\na = {{ root = \"/a\" }}\n[secrets]\nk = {{ use = \"seal\", from = \"fd:3\" }}\n"), &[parameter("a", "Dir<Write>"), parameter("k", "Secret<Seal>")]).is_ok());
    }

    #[test]
    fn sources_reach_only_the_grants_that_permit_them() {
        let head = "[launch]\nversion = 1\n";
        let wire = planned(&format!("{head}[dirs]\nc = {{ root = {{ from = \"cwd\" }}, rights = [\"Read\", \"Write\"] }}\n"), &[parameter("c", "Dir")]).unwrap();
        assert!(wire.contains("dir\tall\tany\tcwd\t.\t-\t-\n"), "{wire}");
        let wire = planned(&format!("{head}[input]\ni = {{ from = \"fd\", number = 5 }}\n"), &[parameter("i", "Console<Read>")]).unwrap();
        assert!(wire.contains("input\tfd\t5\t-\t-\n"), "{wire}");
        let wire = planned(&format!("{head}[input]\ni = {{ stdin = true }}\n"), &[parameter("i", "Console<Read>")]).unwrap();
        assert!(wire.contains("input\tstdin\t-\t-\t-\n"), "{wire}");
        let signer = [parameter("targets", "Signer")];
        let wire = planned(&format!("{head}[flags]\nkey-fd = {{ type = \"fd\", secret = true }}\n[secrets]\ntargets = {{ use = \"sign/witchy-2/targets\", from = {{ from = \"arg\", name = \"key-fd\" }} }}\n"), &signer).unwrap();
        assert!(wire.contains("flag\tkey-fd\tfd\t1\t-\t-\n") && wire.contains("secret\ttargets\tsign/witchy-2/targets\targ\tkey-fd\n"), "{wire}");
        let refused = |body: &str, parameters: &[Parameter]| planned(&format!("{head}{body}"), parameters).unwrap_err();
        assert!(refused("[flags]\nk = { type = \"fd\" }\n[secrets]\ntargets = { use = \"sign/witchy-2/targets\", from = { from = \"arg\", name = \"k\" } }\n", &signer).contains("declare it `secret = true`"));
        assert!(refused("[secrets]\ntargets = { use = \"sign/witchy-2/targets\", from = \"env:SEED\" }\n", &signer).contains("never a variable"));
        assert!(refused("[secrets]\ntargets = { use = \"sign/witchy-2/targets\", from = \"fd:4\", also_from = [\"fd:5\"] }\n", &signer).contains("a plan names one source"));
        assert!(refused("[secrets]\ntargets = { use = \"sign/witchy-1/targets\", from = \"fd:4\" }\n", &signer).contains("retired"));
        assert!(refused("[secrets]\nk = { use = \"seal\", from = \"env:K\" }\n[env]\ne = [\"K\"]\n", &[parameter("k", "Secret<Seal>"), parameter("e", "Env")]).contains("not ordinary environment"));
        assert!(refused("[secrets]\na = { use = \"seal\", from = \"fd:4\" }\nb = { use = \"reveal-bytes\", from = { from = \"fd\", number = 4 } }\n", &[parameter("a", "Secret<Seal>"), parameter("b", "Secret<Reveal>")]).contains("one source reaches one slot"));
        assert!(refused("[flags]\nk = { type = \"fd\", secret = true }\n[dirs]\nd = { root = { from = \"arg\", name = \"k\" } }\n", &[parameter("d", "Dir<Read>")]).contains("reaches only a secret"));

        let tls = "[listen]\nsocket = \"127.0.0.1:0\"\n[secrets]\nissuer = { use = \"sign/tls13/server-p384\", from = \"fd:3\" }\n";
        let server = [parameter("socket", "Socket"), parameter("issuer", "TlsSigner")];
        let wire = planned(&format!("{head}{tls}"), &server).unwrap();
        assert!(wire.contains("secret\tissuer\tsign/tls13/server-p384\tfd\t3\n"), "{wire}");
        assert!(refused(&tls.replace("fd:3", "env:KEY"), &server).contains("never a variable"));
        let writable = [parameter("socket", "Socket"), parameter("issuer", "TlsSigner"), parameter("data", "Dir<Write>")];
        assert!(refused(&format!("{tls}[dirs]\ndata = {{ root = \"/srv\" }}\n"), &writable).contains("without Exec or writable"));
    }

    /// The shared document still renders the engine's flags, and refuses what only a launch
    /// can resolve.
    #[test]
    fn exec_processes_is_the_documents_and_shown() {
        let exec = [parameter("exec", "Exec")];
        let default = planned("[launch]\nversion = 1\n[exec]\nexec = { dir = \"/usr/bin\", programs = [\"true\"] }\n", &exec).unwrap();
        assert!(!default.contains("exec-processes"), "{default}");
        let plan = super::plan(&super::super::parse("[launch]\nversion = 1\n[exec]\nexec = { dir = \"/usr/bin\", programs = [\"true\"] }\n").unwrap(), &exec, "d").unwrap();
        assert!(plan.review.iter().any(|l| l.contains("exec-processes = 256 (default)")), "{:?}", plan.review);
        let set = planned("[launch]\nversion = 1\n[exec]\nexec = { dir = \"/usr/bin\", programs = [\"true\"] }\n[limits]\nexec-processes = 20\n", &exec).unwrap();
        assert!(set.contains("limit\texec-processes\t20"), "{set}");
        assert!(planned("[launch]\nversion = 1\n[exec]\nexec = { dir = \"/usr/bin\", programs = [\"true\"] }\n[limits]\nexec-processes = 0\n", &exec).is_err());
        assert!(planned("[launch]\nversion = 1\n[limits]\nexec-processes = 20\n", &[]).is_err(), "only beside an Exec");
    }

    #[test]
    fn wasm_grants_reads_the_same_document() {
        let text = "[launch]\nversion = 1\n[dirs]\ndata = { root = { from = \"literal\", value = \"/srv/data\" } }\n";
        let granted = super::super::check(&parse(text).unwrap(), &[parameter("data", "Dir<Read>")], std::path::Path::new("/")).unwrap();
        assert_eq!(granted.flags, ["--allocator", "--dir=/srv/data"]);
        assert!(granted.warnings.is_empty(), "{:?}", granted.warnings);
        let launched = "[launch]\nversion = 1\n[flags]\np = { type = \"path\" }\n[dirs]\ndata = { root = { from = \"arg\", name = \"p\" } }\n";
        let errors = super::super::check(&parse(launched).unwrap(), &[parameter("data", "Dir<Read>")], std::path::Path::new("/")).unwrap_err().join("\n");
        assert!(errors.contains("manifest-bound native executable"), "{errors}");
    }
}
