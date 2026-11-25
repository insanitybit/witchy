//! Bounded input and atomic single-module publication. A refused compilation
//! never touches an existing output, including its permissions.
use std::{
    borrow::Cow,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// One contiguous segment of the bundled compiler input.
#[derive(Clone, PartialEq)]
pub(super) struct Part {
    pub(super) start: usize, pub(super) path: PathBuf, pub(super) identity: PathBuf,
    /// The text as `include` read it (before imports were rewritten), and the identities of the
    /// files it imports: what the key law's gate checks, so it judges the compiled text, not a
    /// second read of the disk (review round 14: a file rewritten between reads passed).
    pub(super) raw: String, pub(super) imports: Vec<PathBuf>,
    /// The package this file belongs to, when it came in through `import "@name"`: what a policy
    /// names it by (`@name`, and `@name/PATH` for its own files), wherever it is on disk.
    pub(super) edges: Vec<(String, u32)>,
    package: Option<Package>,
}

/// A package by name: its first file (what `import "@name"` resolved to) and the directory that
/// file is in, which holds every file of the package.
#[derive(Clone, PartialEq)]
struct Package { name: String, entry: PathBuf, root: PathBuf, requires: Option<Vec<String>> }

/// Bundled source. `import "relative.wy"` lines may open a file, before any
/// declaration. Each imported file is included once, before its importer, and
/// is checked exactly like text written in place: an import grants no trust.
/// A file without imports is passed through byte for byte.
pub(super) fn read_source(input: &Path) -> Result<String, String> { Ok(read_bundle(input)?.0) }

pub(super) fn read_bundle(input: &Path) -> Result<(String, Vec<Part>), String> {
    read_bundle_under(input, None)
}

fn read_bundle_under(input: &Path, test_root: Option<&Path>) -> Result<(String, Vec<Part>), String> {
    let (mut text, mut parts, mut seen) = (String::new(), Vec::new(), Vec::new());
    include(input, input, None, test_root, &mut text, &mut parts, &mut seen, 0)?;
    Ok((text, parts))
}

/// Disk-cache identity for a consumer of this compiler. Frame the actual import graph,
/// including renamed paths, raw imports, package authority and the adjacent policy.
/// The consumer also keys the compiler bytes, selected toolchain and request options.
pub(super) fn input_key(parts: &[Part], policy: Option<&str>, checker: Option<&str>) -> Result<String, String> {
    let mut data = Vec::new();
    let mut field = |v: &[u8]| {
        data.extend_from_slice(&(v.len() as u64).to_be_bytes());
        data.extend_from_slice(v);
    };
    field(b"witchy.input.v1");
    field(&(parts.len() as u64).to_be_bytes());
    for part in parts {
        field(part.path.as_os_str().as_encoded_bytes());
        field(part.identity.as_os_str().as_encoded_bytes());
        field(part.raw.as_bytes());
        field(&(part.imports.len() as u64).to_be_bytes());
        for path in &part.imports { field(path.as_os_str().as_encoded_bytes()); }
        field(&[u8::from(part.package.is_some())]);
        if let Some(package) = &part.package {
            field(package.name.as_bytes());
            field(package.entry.as_os_str().as_encoded_bytes());
            field(package.root.as_os_str().as_encoded_bytes());
            field(&[u8::from(package.requires.is_some())]);
            if let Some(names) = &package.requires {
                field(&(names.len() as u64).to_be_bytes());
                for name in names { field(name.as_bytes()); }
            }
        }
    }
    field(checker.unwrap_or("").as_bytes());
    field(&[u8::from(policy.is_some())]);
    if let Some(policy) = policy { field(policy.as_bytes()); }
    super::proof_digest::bytes(&data).map_err(|e| e.to_string())
}

/// The standard library, shipped inside the compiler: `import console` is `wasm-console.wy`.
/// A bundled name always means this copy, wherever the program is.
const STANDARD: &[(&str, &str)] = &[
    ("wasm-prop.wy", include_str!("../../library/wasm-prop.wy")),
    ("wasm-test-runner.wy", include_str!("../../library/wasm-test-runner.wy")),
    ("wasm-testing.wy", include_str!("../../library/wasm-testing.wy")),
    ("wasm-bufio.wy", include_str!("../../library/wasm-bufio.wy")),
    ("wasm-bufio-fs.wy", include_str!("../../library/wasm-bufio-fs.wy")),
    ("wasm-bufio-net.wy", include_str!("../../library/wasm-bufio-net.wy")),
    ("wasm-buffer.wy", include_str!("../../library/wasm-buffer.wy")),
    ("wasm-console.wy", include_str!("../../library/wasm-console.wy")),
    ("wasm-console-bytes.wy", include_str!("../../library/wasm-console-bytes.wy")),
    ("wasm-key-agreement.wy", include_str!("../../library/wasm-key-agreement.wy")),
    ("wasm-certificates.wy", include_str!("../../library/wasm-certificates.wy")),
    ("wasm-crypto.wy", include_str!("../../library/wasm-crypto.wy")),
    ("wasm-deque.wy", include_str!("../../library/wasm-deque.wy")),
    ("wasm-discard.wy", include_str!("../../library/wasm-discard.wy")),
    ("wasm-discard-core.wy", include_str!("../../library/wasm-discard-core.wy")),
    ("wasm-encoding.wy", include_str!("../../library/wasm-encoding.wy")),
    ("wasm-args.wy", include_str!("../../library/wasm-args.wy")),
    ("wasm-env.wy", include_str!("../../library/wasm-env.wy")),
    ("wasm-exec.wy", include_str!("../../library/wasm-exec.wy")),
    ("wasm-format.wy", include_str!("../../library/wasm-format.wy")),
    ("wasm-pointer-format.wy", include_str!("../../library/wasm-pointer-format.wy")),
    ("wasm-fs.wy", include_str!("../../library/wasm-fs.wy")),
    ("wasm-word.wy", include_str!("../../library/wasm-word.wy")),
    ("wasm-simd.wy", include_str!("../../library/wasm-simd.wy")),
    ("wasm-foldhash.wy", include_str!("../../library/wasm-foldhash.wy")),
    ("wasm-hash.wy", include_str!("../../library/wasm-hash.wy")),
    ("wasm-hashmap.wy", include_str!("../../library/wasm-hashmap.wy")),
    ("wasm-grouped-hashmap.wy", include_str!("../../library/wasm-grouped-hashmap.wy")),
    ("wasm-keymap.wy", include_str!("../../library/wasm-keymap.wy")),
    ("wasm-graph.wy", include_str!("../../library/wasm-graph.wy")),
    ("wasm-heap.wy", include_str!("../../library/wasm-heap.wy")),
    ("wasm-big.wy", include_str!("../../library/wasm-big.wy")),
    ("wasm-int.wy", include_str!("../../library/wasm-int.wy")),
    ("wasm-float.wy", include_str!("../../library/wasm-float.wy")),
    ("wasm-json-tree.wy", include_str!("../../library/wasm-json-tree.wy")),
    ("wasm-json-sorted.wy", include_str!("../../library/wasm-json-sorted.wy")),
    ("wasm-json-strings.wy", include_str!("../../library/wasm-json-strings.wy")),
    ("wasm-json.wy", include_str!("../../library/wasm-json.wy")),
    ("wasm-cell-list.wy", include_str!("../../library/wasm-cell-list.wy")),
    ("wasm-cell.wy", include_str!("../../library/wasm-cell.wy")),
    ("wasm-pointer-list.wy", include_str!("../../library/wasm-pointer-list.wy")),
    ("wasm-list.wy", include_str!("../../library/wasm-list.wy")),
    ("wasm-link.wy", include_str!("../../library/wasm-link.wy")),
    ("wasm-net.wy", include_str!("../../library/wasm-net.wy")),
    ("wasm-option.wy", include_str!("../../library/wasm-option.wy")),
    ("wasm-query.wy", include_str!("../../library/wasm-query.wy")),
    ("wasm-http-framing.wy", include_str!("../../library/wasm-http-framing.wy")),
    ("wasm-http-head.wy", include_str!("../../library/wasm-http-head.wy")),
    ("wasm-https.wy", include_str!("../../library/wasm-https.wy")),
    ("wasm-https-client.wy", include_str!("../../library/wasm-https-client.wy")),
    ("wasm-https-fs.wy", include_str!("../../library/wasm-https-fs.wy")),
    ("wasm-http.wy", include_str!("../../library/wasm-http.wy")),
    ("wasm-rand.wy", include_str!("../../library/wasm-rand.wy")),
    ("wasm-replace.wy", include_str!("../../library/wasm-replace.wy")),
    ("wasm-result.wy", include_str!("../../library/wasm-result.wy")),
    ("wasm-local-service.wy", include_str!("../../library/wasm-local-service.wy")),
    ("wasm-mldsa-signer.wy", include_str!("../../library/wasm-mldsa-signer.wy")),
    ("wasm-secret.wy", include_str!("../../library/wasm-secret.wy")),
    ("wasm-sort.wy", include_str!("../../library/wasm-sort.wy")),
    ("wasm-ops.wy", include_str!("../../library/wasm-ops.wy")),
    ("wasm-time.wy", include_str!("../../library/wasm-time.wy")),
    ("wasm-tls-certificates.wy", include_str!("../../library/tls/certificates.wy")),
    ("wasm-tls-client-auth.wy", include_str!("../../library/tls/client_auth.wy")),
    ("wasm-tls-client-exchange.wy", include_str!("../../library/tls/client_exchange.wy")),
    ("wasm-tls-client-flight.wy", include_str!("../../library/tls/client_flight.wy")),
    ("wasm-tls-client-hello.wy", include_str!("../../library/tls/client_hello.wy")),
    ("wasm-tls-extensions.wy", include_str!("../../library/tls/extensions.wy")),
    ("wasm-tls-hello.wy", include_str!("../../library/tls/hello.wy")),
    ("wasm-tls.wy", include_str!("../../library/tls/session.wy")),
    ("wasm-tls-transport.wy", include_str!("../../library/tls/transport.wy")),
    ("wasm-tls-trust-pem.wy", include_str!("../../library/tls/trust_pem.wy")),
    ("wasm-tls-server-auth.wy", include_str!("../../library/tls/server_auth.wy")),
    ("wasm-tls-server-socket.wy", include_str!("../../library/tls/server_socket.wy")),
    ("wasm-tls-p384.wy", include_str!("../../library/tls/p384.wy")),
    ("wasm-tls-kdf.wy", include_str!("../../library/tls/kdf.wy")),
    ("wasm-tls-key-schedule.wy", include_str!("../../library/tls/key_schedule.wy")),
    ("wasm-tls-record.wy", include_str!("../../library/tls/record.wy")),
    ("wasm-span.wy", include_str!("../../library/wasm-span.wy")),
    ("wasm-string.wy", include_str!("../../library/wasm-string.wy")),
    ("wasm-tasks.wy", include_str!("../../library/wasm-tasks.wy")),
    ("wasm-text.wy", include_str!("../../library/wasm-text.wy")),
    ("wasm-url.wy", include_str!("../../library/wasm-url.wy")),
    ("wasm-vec.wy", super::VECTOR_LIBRARY),
    ("wasm-vec-text.wy", include_str!("../../library/wasm-vec-text.wy")),
];
/// Where the standard modules appear to be, in diagnostics and file identities.
const STANDARD_ROOT: &str = "<standard>";
/// The standard module that declares a public type of this name, as `import` names it
/// (`HashMap` -> `hashmap`): for a refusal of an unknown type. Read only on that path.
pub(super) fn module_declaring_type(name: &str) -> Option<String> {
    STANDARD.iter().find_map(|(file, source)| {
        let declares = source.lines().any(|line| {
            let rest = line.strip_prefix("pub sealed type ").or_else(|| line.strip_prefix("pub type ")).or_else(|| line.strip_prefix("pub trait "));
            rest.is_some_and(|rest| rest.strip_prefix(name).is_some_and(|after| after.starts_with([':', '<', ' ', '='])))
        });
        declares.then(|| file.trim_start_matches("wasm-").trim_end_matches(".wy").replace('-', "_"))
    })
}

/// The standard module that declares a public free function of this name (`sort` -> `sort`).
pub(super) fn module_declaring_function(name: &str) -> Option<String> {
    STANDARD.iter().find_map(|(file, source)| {
        // `pub fn name(`, with `pure` and `nopanic` between where the function is either.
        let declares = source.lines().any(|line| line.strip_prefix("pub ")
            .map(|rest| rest.strip_prefix("pure ").unwrap_or(rest)).map(|rest| rest.strip_prefix("nopanic ").unwrap_or(rest))
            .and_then(|rest| rest.strip_prefix("fn "))
            .is_some_and(|rest| rest.strip_prefix(name).is_some_and(|after| after.starts_with(['(', '<']))));
        declares.then(|| file.trim_start_matches("wasm-").trim_end_matches(".wy").replace('-', "_"))
    })
}

/// The standard module whose `impl` of a type (its own, or a trait's for it) holds a method of
/// this name (`String`, `find_text` -> `text`; `U64`, `min` -> `int`).
/// Whether the standard library gives `method` to exactly the type `owner` (`Dir<Write>`), as
/// written after `impl`: what a refusal reads to say a value granted for reading cannot write.
pub(super) fn standard_impl_has(owner: &str, method: &str) -> bool {
    STANDARD.iter().any(|(_, source)| {
        let mut inside = false;
        source.lines().any(|line| {
            if let Some(rest) = line.strip_prefix("impl ") { inside = rest.trim_end_matches(':') == owner; return false; }
            if !line.starts_with(' ') && !line.is_empty() && !line.starts_with("//") { inside = false; }
            inside && line.strip_prefix("    pub fn ").is_some_and(|rest| rest.strip_prefix(method).is_some_and(|after| after.starts_with(['(', '<'])))
        })
    })
}
pub(super) fn module_declaring_method(owner: &str, method: &str) -> Option<String> {
    STANDARD.iter().find_map(|(file, source)| {
        let mut inside = false;
        let found = source.lines().any(|line| {
            if let Some(rest) = line.strip_prefix("impl") {
                // `impl T:`, `impl<A> T<A>:`, `impl Trait for T:`: the type after `for`, or after `impl`.
                let rest = rest.trim_end_matches(':');
                let rest = if rest.starts_with('<') { rest.split_once("> ").map(|(_, r)| r).unwrap_or("") } else { rest.trim_start() };
                let target = rest.rsplit(" for ").next().unwrap_or(rest);
                inside = target.split('<').next().is_some_and(|name| name.trim() == owner);
                return false;
            }
            if !line.starts_with(' ') && !line.is_empty() && !line.starts_with("//") { inside = false; }
            inside && line.trim_start().strip_prefix("pub ").map(|rest| rest.strip_prefix("pure ").unwrap_or(rest)).map(|rest| rest.strip_prefix("nopanic ").unwrap_or(rest))
                .and_then(|rest| rest.strip_prefix("fn ")).or_else(|| line.strip_prefix("    fn "))
                .is_some_and(|rest| rest.strip_prefix(method).is_some_and(|after| after.starts_with(['(', '<'])))
        });
        found.then(|| file.trim_start_matches("wasm-").trim_end_matches(".wy").replace('-', "_"))
    })
}

fn standard(path: &Path) -> Option<&'static str> {
    let name = path.strip_prefix(STANDARD_ROOT).ok()?.to_str()?;
    STANDARD.iter().find(|(file, _)| *file == name).map(|(_, text)| *text)
}

/// Whether a file's text in the bundle is a standard module's as `include` writes it: every line
/// the shipped file's, except the header's imports, which `include` rewrites to positions in
/// the bundle, and each of those names a standard module (`import NAME FILE 1`).
/// (proof_reach.rs: what may be left unproved when nothing emitted reaches it.)
pub(super) fn standard_text(bundled: &str) -> bool {
    let rewritten = |line: &str| line.strip_prefix("import ").is_some_and(|rest| {
        let words: Vec<&str> = rest.split_whitespace().collect();
        words.len() == 3 && words[1].bytes().all(|b| b.is_ascii_digit()) && words[2] == "1"
    });
    STANDARD.iter().any(|(_, text)| {
        let (mut ours, mut shipped) = (bundled.lines(), text.lines());
        loop {
            match (ours.next(), shipped.next()) {
                (None, None) => return true,
                (Some(a), Some(b)) if a == b || (b.starts_with("import ") && rewritten(a)) => continue,
                _ => return false,
            }
        }
    })
}

/// Whether a bundled part is the standard library's file: the bundled copy, or a file whose
/// resolved name (its identity: symlinks and case followed) is a standard file's and whose text,
/// as it was read for the build, is exactly that file's.
impl Part {
    pub(super) fn standard_module(&self, name: &str) -> bool {
        self.path == Path::new(STANDARD_ROOT).join(format!("wasm-{}.wy", name.replace('_', "-")))
            && standard(&self.path).is_some()
    }
    pub(super) fn package_name(&self) -> Option<&str> { self.package.as_ref().map(|p| p.name.as_str()) }
    pub(super) fn local(&self) -> bool { self.package.is_none() && standard(&self.path).is_none() }
}
fn library_part(part: &Part) -> bool {
    if standard(&part.path).is_some() { return true; }
    let Some(name) = part.identity.file_name().and_then(|n| n.to_str()) else { return false };
    STANDARD.iter().any(|(file, text)| *file == name && part.raw == *text)
}

/// The name a file is qualified by: its stem, and a standard module's name without `wasm-`.
fn module_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let name = if path.starts_with(STANDARD_ROOT) { stem.strip_prefix("wasm-")?.replace('-', "_") } else { stem.to_string() };
    let plain = name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    plain.then_some(name)
}

/// Where package `name` is, from `PROGRAM.packages` beside the program: lines `name: path` or
/// `name VERSION: path` (the version is a label for readers; review is the policy's digests),
/// `#` comments, paths relative to the list. A line may end `; requires a b`: the packages this
/// one may import by name, none for `; requires` alone. Without it, any of the list's.
fn package(program: &Path, name: &str) -> Result<(PathBuf, Option<Vec<String>>), String> {
    let list = program.with_extension("packages");
    let text = fs::read_to_string(&list).map_err(|_| format!("no package list {} names packages", list.display()))?;
    for (number, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() { continue; }
        let Some((head, target)) = line.split_once(':') else { return Err(format!("{} line {}: `name: path` or `name VERSION: path`", list.display(), number + 1)); };
        let mut words = head.split_whitespace();
        let (Some(listed), version) = (words.next(), words.next()) else { return Err(format!("{} line {}: a package's name comes first", list.display(), number + 1)); };
        if words.next().is_some() || version.is_some_and(|v| v.is_empty()) { return Err(format!("{} line {}: `name: path` or `name VERSION: path`", list.display(), number + 1)); }
        if listed == name {
            let (target, requires) = match target.split_once(';') {
                Some((target, clause)) => {
                    let mut words = clause.split_whitespace();
                    if words.next() != Some("requires") { return Err(format!("{} line {}: after the path, `; requires NAME...`", list.display(), number + 1)); }
                    (target, Some(words.map(str::to_string).collect()))
                }
                None => (target, None),
            };
            let target = target.trim();
            if target.is_empty() { return Err(format!("{} line {}: no path", list.display(), number + 1)); }
            return Ok((list.parent().unwrap_or_else(|| Path::new(".")).join(target), requires));
        }
    }
    Err(format!("{} lists no package of that name", list.display()))
}

/// `seen` holds each file's identity with the package it was included for: a file is one
/// owner's, so one reached both as a package's and as another owner's is refused.
/// Where an import is refused: its line of the importing file (or, for the tool that reads
/// byte ranges, the old form with none).
fn import_at(line: usize) -> String {
    if crate::diagnostic::offsets_shown() { ": W0100 0..0: ".into() } else { format!(":{line}:1: W0100: ") }
}
fn include(path: &Path, identity: &Path, owner: Option<&Package>, test_root: Option<&Path>, text: &mut String, parts: &mut Vec<Part>, seen: &mut Vec<(PathBuf, Option<String>)>, depth: usize) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    let limit = super::MAX_PROGRAM_BYTES as u64 + 1;
    let mut source = String::new();
    match standard(path) {
        Some(shipped) => source.push_str(shipped),
        None => {
            use std::os::unix::fs::MetadataExt;
            if test_root.is_some() {
                let m=fs::symlink_metadata(path).map_err(fail)?;
                if !m.is_file() || m.nlink()!=1 { return Err("test source needs a singly linked regular file".into()); }
            }
            File::open(path).map_err(fail)?.take(limit).read_to_string(&mut source).map_err(fail)?;
        }
    }
    let mut body = String::with_capacity(source.len());
    let mut header = true;
    let mut edges = Vec::new();
    let mut imported: Vec<PathBuf> = Vec::new();
    for (index, line) in source.split_inclusive('\n').enumerate() {
        // An import is refused at its own line of the importing file.
        let number = index + 1;
        let trimmed = line.trim_end();
        let import = if header { trimmed.strip_prefix("import \"").and_then(|rest| rest.strip_suffix('"')).map(|r| (r.to_string(), false))
            // `import console`: a module of the standard library, by name.
            .or_else(|| trimmed.strip_prefix("import ").filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c == '-')).map(|n| (format!("wasm-{}.wy", n.replace('_', "-")), true))) } else { None };
        // `import json-tree`: a module is named with `_` (its file's `-` reads as a minus).
        if header && let Some(name) = trimmed.strip_prefix("import ") && name.contains('-') && !name.starts_with('"') {
            return Err(format!("{}{}import {name}: a module is named with `_`, as `import {}`", path.display(), import_at(number), name.replace('-', "_")));
        }
        match import {
            Some((relative, named)) => {
                let inside = standard(path).is_some();
                // First import: only now is file identity needed, so plain sources stay path independent.
                if seen.is_empty() { seen.push((if inside { path.to_path_buf() } else { fs::canonicalize(path).map_err(fail)? }, None)); }
                // A standard module's own imports, and every import by name, are standard modules.
                let (target, identity, package) = if named || inside {
                    let target = Path::new(STANDARD_ROOT).join(&relative);
                    if standard(&target).is_none() { return Err(format!("{}{}import {}: no standard module of that name; the standard modules are {}", path.display(), import_at(number), relative.trim_start_matches("wasm-").trim_end_matches(".wy").replace('-', "_"), STANDARD.iter().map(|(f, _)| f.trim_start_matches("wasm-").trim_end_matches(".wy").replace('-', "_")).collect::<Vec<_>>().join(", "))); }
                    (target.clone(), target, None)
                } else if let Some(name) = relative.strip_prefix('@') {
                    // `import "@httpd"`: a package by name, from the program's `PROGRAM.packages`
                    // (`httpd 1.0: vendor/httpd/httpd.wy`), one list for the whole program, as a
                    // lock file is. Its path is relative to that list.
                    // A package that declares its requirements imports only those (the lock's
                    // edges are then the build's): the program, and a package without a
                    // declaration, may import any of the list.
                    if let Some(Package { name: importer, requires: Some(requires), .. }) = owner && !requires.iter().any(|r| r == name) {
                        return Err(format!("{}{}import \"@{name}\": package @{importer} does not require {name} (its package list line requires {})", path.display(), import_at(number), if requires.is_empty() { "nothing".to_string() } else { requires.join(", ") }));
                    }
                    let (target, requires) = package(&seen[0].0, name).map_err(|e| format!("{}{}import \"@{name}\": {e}", path.display(), import_at(number)))?;
                    let identity = fs::canonicalize(&target).map_err(|e| format!("{}{}import \"@{name}\" ({}): {e}", path.display(), import_at(number), target.display()))?;
                    if test_root.is_some_and(|root| identity.starts_with(root)) {
                        return Err("a test dependency needs a separate resolved package root".into());
                    }
                    let root = identity.parent().map(Path::to_path_buf).unwrap_or_default();
                    let package = Package { name: name.to_string(), entry: identity.clone(), root, requires };
                    (target, identity, Some(package))
                } else {
                    let target = path.parent().unwrap_or_else(|| Path::new(".")).join(&relative);
                    // The import as written, not the path it resolved to: a build tool compiles a copy,
                    // and the copy's directory means nothing to the author.
                    let identity = fs::canonicalize(&target).map_err(|e| if e.kind() == std::io::ErrorKind::NotFound {
                        format!("{}{}import \"{relative}\": no such file beside the file that imports it (a path in quotes is read from the importing file's directory; a standard module is `import name`, a package `import \"@name\"`)", path.display(), import_at(number))
                    } else { format!("{}{}import \"{relative}\": {e}", path.display(), import_at(number)) })?;
                    // A package is the files under its directory: one of them may not reach
                    // outside it by path (another package is imported by name), so a build reads
                    // only what was locked and reviewed for it, and nothing of the host's.
                    if let Some(owner) = owner && !identity.starts_with(&owner.root) {
                        return Err(format!("{}{}import {relative}: package @{} may import only files in its own directory, or packages by name", path.display(), import_at(number), owner.name));
                    }
                    if let Some(root) = test_root {
                        let base = owner.map_or(root, |p| p.root.as_path());
                        let mut walk = path.parent().unwrap().to_path_buf();
                        let mut escaped = Path::new(&relative).is_absolute();
                        for component in Path::new(&relative).components() {
                            match component {
                                std::path::Component::Normal(name) => walk.push(name),
                                std::path::Component::ParentDir if walk != base => { walk.pop(); }
                                std::path::Component::CurDir => (),
                                _ => { escaped = true; break; }
                            }
                            if walk.is_symlink() || (walk != base && walk.join("witchy.toml").exists()) { escaped = true; break; }
                        }
                        if escaped || !identity.starts_with(base) {
                            return Err(format!("test import {relative} crosses its resolved owner or a symbolic link"));
                        }
                    }
                    (target, identity, owner.cloned())
                };
                let package_name = package.as_ref().map(|p| p.name.clone());
                imported.push(identity.clone());
                match seen.iter().find(|(file, _)| *file == identity) {
                    Some((_, first)) if *first != package_name => {
                        let who = |p: &Option<String>| p.as_ref().map_or_else(|| "the program".to_string(), |n| format!("package @{n}"));
                        return Err(format!("{}{}import {relative}: {} is {}'s file, and here it would be {}'s: a file belongs to one owner", path.display(), import_at(number), target.display(), who(first), who(&package_name)));
                    }
                    Some(_) => {}
                    None => {
                        if depth >= 16 || seen.len() >= 64 { return Err(format!("{}: import nesting or file count limit exceeded", path.display())); }
                        seen.push((identity.clone(), package_name));
                        include(&target, &identity, package.as_ref(), test_root, text, parts, seen, depth + 1)?;
                    }
                }
                // The parser learns which file this name means here (`import NAME FILE`), on
                // the same line, so the importer keeps its line numbers. A file whose name is
                // not a plain lowercase name cannot be qualified, and is still imported.
                // A package's first file is qualified by the package's name (`config_reader` for
                // `@config-reader`), not its file's, so it reads the same from any copy of it.
                let module = match relative.strip_prefix('@') { Some(name) if !named && !inside => module_name(Path::new(&format!("{}.wy", name.replace('-', "_")))), _ => module_name(&target) };
                // `memory.` and `provider.` name the compiler's own operations, in every file and
                // in the library's: no module takes either name (it would stand between a library
                // file and the operation it calls).
                if let Some(name) = module.as_deref().filter(|name| matches!(*name, "memory" | "provider")) {
                    return Err(format!("{}{}import {relative}: a module cannot be named `{name}`, which names the compiler's own operations (`{name}.…`) in every file; rename the file or package", path.display(), import_at(number)));
                }
                // Every import is written, named or not: what a file may name bare is what the
                // files it imports offer, so the parser must see each edge (`import FILE` for a
                // file that cannot be qualified). A third number says whose the imported file is:
                // 1 a standard module, 2 another package's first file; without it, the importer's
                // own (the same package: a relative import stays in its owner's directory).
                let kind = if named || inside { " 1" } else if relative.starts_with('@') { " 2" } else { "" };
                match (parts.iter().position(|part| part.identity == identity), module) {
                    (Some(file), Some(name)) => {
                        edges.push((name.clone(), file as u32));
                        body.push_str(&format!("import {name} {file}{kind}"));
                    }
                    (Some(file), None) => {
                        edges.push((String::new(), file as u32));
                        body.push_str(&format!("import {file}{kind}"));
                    }
                    // A file that is still being read imports this one, at some depth: an import
                    // cycle. Its place in the bundle is not known yet, and a name would have to
                    // mean a file read after its user. Refused, as a file that is not there is.
                    (None, _) => return Err(format!("{}{}import {}: {} imports this file in turn (an import cycle): move what both need into a third file that each imports", path.display(), import_at(number), relative, target.display())),
                }
                body.push_str(&line[trimmed.len()..]);
            }
            None => {
                // `from m import T` lines belong to the header too.
                if !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with("from ") { header = false; }
                body.push_str(line);
            }
        }
    }
    if depth > 0 && !body.ends_with('\n') { body.push('\n'); }
    if text.len() as u64 + body.len() as u64 > limit { return Err(format!("{}: bundled source exceeds {} bytes", path.display(), super::MAX_PROGRAM_BYTES)); }
    parts.push(Part { start: text.len(), path: path.to_path_buf(), identity: identity.to_path_buf(), raw: source, imports: imported, edges, package: owner.cloned() });
    text.push_str(&body);
    Ok(())
}

/// Each bundled file's name (as `names`) and the pin (`sha384:HEX`) of its text as the build read it (not
/// the file read again: automated review). That is the file's own bytes, before its imports were
/// rewritten to positions in the bundle: a rewritten line names a file by where it landed, so
/// two files could swap what they import and keep their digests, and a package's digest moved
/// with what the program imported before it (review of 9c8ac74b). Which file an import means
/// follows from the importer's text and its name, which the policy line pins too.
pub(super) fn digests(parts: &[Part], base: &Path) -> Result<Vec<(String, String)>, String> {
    names(parts, base).into_iter().zip(parts).map(|(name, part)| {
        let digest = super::proof_digest::pin(part.raw.as_bytes()).map_err(|e| format!("{}: {e}", part.path.display()))?;
        Ok((name, digest))
    }).collect()
}

/// `digests` as they were before the witchy-2 profile: each file's SHA-256, bare hex. Only for
/// checking an old pin before `--repin` replaces it (caps.rs `repin`).
pub(super) fn legacy_digests(parts: &[Part], base: &Path) -> Result<Vec<(String, String)>, String> {
    names(parts, base).into_iter().zip(parts).map(|(name, part)| {
        let digest = super::proof_digest::bytes(part.raw.as_bytes()).map_err(|e| format!("{}: {e}", part.path.display()))?;
        Ok((name, digest))
    }).collect()
}

/// Where each bundled file begins.
pub(super) fn starts(parts: &[Part]) -> Vec<usize> { parts.iter().map(|part| part.start).collect() }
/// Each file of the bundle by the path a policy names it with, in module order: where it is on
/// disk (its canonical identity), relative to the working directory when under it, so
/// `app/../deps/c.wy` is `deps/c.wy` (review: it took `app/`'s line) and a path through a symlink
/// is named where it leads (automated review: resolving `..` by text disagreed with the disk).
/// Only the compiler's own modules are under `<standard>/`; a name that would start with `<`
/// otherwise is `./<...`.
pub(super) fn names(parts: &[Part], base: &Path) -> Vec<String> {
    let here = base.canonicalize().ok();
    parts.iter().map(|part| {
        if standard(&part.path).is_some() { return part.path.display().to_string(); }
        // A package's files by the package's name: the same in a build from a checkout and one
        // from a registry copy, so one policy line holds both.
        if let Some(package) = &part.package {
            if part.identity == package.entry { return format!("@{}", package.name); }
            if let Ok(inside) = part.identity.strip_prefix(&package.root) { return format!("@{}/{}", package.name, inside.display()); }
        }
        // The program's own file may be named as given; every import is canonical already.
        let identity = part.identity.canonicalize().unwrap_or_else(|_| part.identity.clone());
        let name = match &here { Some(here) => identity.strip_prefix(here).unwrap_or(&identity), None => &identity }.display().to_string();
        if name.starts_with('<') { format!("./{name}") } else { name }
    }).collect()
}

/// Diagnostics name the file, line and column of the bundled offset.
/// Where byte `at` of the bundle is, as `file:line` (the file as given to the compiler).
pub(super) fn line_of(text: &str, parts: &[Part], at: usize) -> Option<String> {
    let part = parts.iter().rev().find(|part| part.start <= at)?;
    if at > text.len() || !text.is_char_boundary(at) { return None; }
    let line = text[part.start..at].matches('\n').count() + 1;
    let name = part.path.file_name().map_or_else(|| part.path.display().to_string(), |n| n.to_string_lossy().into_owned());
    Some(format!("{name}:{line}"))
}
/// A message may name a second place as `⟪at N⟫` (N a byte offset of the bundle, as the
/// diagnostic's own span is): written out as `file:line`, or `earlier` where it cannot be.
fn places<'a>(message: &'a str, text: Option<(&str, &[Part])>) -> Cow<'a, str> {
    if !message.contains("⟪at ") {
        return Cow::Borrowed(message);
    }
    let mut out = String::new();
    let mut rest = message;
    while let Some(open) = rest.find("⟪at ") {
        out.push_str(&rest[..open]);
        let after = &rest[open + "⟪at ".len()..];
        let Some(close) = after.find('⟫') else { out.push_str(&rest[open..]); return Cow::Owned(out); };
        let place = after[..close].parse::<usize>().ok().and_then(|at| text.and_then(|(text, parts)| line_of(text, parts, at)));
        out.push_str(place.as_deref().unwrap_or("earlier"));
        rest = &after[close + '⟫'.len_utf8()..];
    }
    out.push_str(rest);
    Cow::Owned(out)
}
pub(super) fn render(text: &str, parts: &[Part], e: &crate::diagnostic::Diagnostic) -> String {
    let message = places(&e.message, Some((text, parts)));
    let at = e.span.start;
    let part = parts.iter().rev().find(|part| part.start <= at);
    match part {
        Some(part) if e.span != (0..0) && at <= text.len() && text.is_char_boundary(at) => {
            let before = &text[part.start..at];
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            let begin = part.start + before.rfind('\n').map_or(0, |i| i + 1);
            let shown = text[begin..].lines().next().unwrap_or("");
            if crate::diagnostic::offsets_shown() { format!("{}:{line}:{column}: {} {:?}: {message}\n  {}", part.path.display(), e.code, e.span, shown.trim_end()) }
            else { format!("{}:{line}:{column}: {}: {message}\n  {}", part.path.display(), e.code, shown.trim_end()) }
        }
        _ => display_message(e, &message),
    }
}

pub(super) fn build(input: &Path, output: &Path, reviewed_key: Option<&str>) -> Result<(String, super::Measurements), String> {
    let mut session = super::proof_session::Session::publishing().map_err(|e| display(&e))?;
    let result = match reviewed_key {
        Some(key) => build_reviewed(input, output, key, session.as_mut()),
        None => build_in(input, output, None, session.as_mut(), true),
    };
    if result.is_err() { if let Some(session) = &mut session { session.refuse(); } }
    result
}

pub(super) fn check_package(input: &Path) -> Result<(), String> {
    let snapshot = BuildInput::read(input)?;
    let gate = policy_gate(input, &snapshot);
    let mut session = super::proof_session::Session::publishing().map_err(|e| display(&e))?;
    let result = (|| {
        super::check_package_bundle(&snapshot.source, &starts(&snapshot.parts), session.as_mut(), &gate)
            .map_err(|e| render_build_refusal(&snapshot.source, &snapshot.parts, &e))?;
        if let Some(checker) = &mut session {
            checker.validate_publication().map_err(|e| display(&e))?;
            checker.finish().map_err(|e| display(&e))?;
        }
        if BuildInput::read(input)? != snapshot {
            return Err("source or policy changed during package checking".into());
        }
        Ok(())
    })();
    if result.is_err() { if let Some(session) = &mut session { session.refuse(); } }
    result
}

fn build_reviewed(input: &Path, output: &Path, key: &str, session: Option<&mut super::proof_session::Session>) -> Result<(String, super::Measurements), String> {
    let start = std::time::Instant::now();
    let snapshot = BuildInput::read(input)?;
    let binding = super::compiler_session::binding()?;
    if input_key(&snapshot.parts, snapshot.policy.as_deref(), binding.as_deref())? != key {
        return Err("compiler inputs changed after grant review; output preserved".into());
    }
    build_loaded(input, output, None, session, true, start, &snapshot, None, &|| {
        if super::compiler_session::binding()? != binding {
            return Err("proof toolchain changed after grant review; output preserved".into());
        }
        Ok(())
    })
}
fn display(e: &crate::diagnostic::Diagnostic) -> String {
    display_message(e, &e.message)
}
fn display_message(e: &crate::diagnostic::Diagnostic, message: &str) -> String {
    let message = places(message, None);
    if e.span == (0..0) { return crate::diagnostic::unplaced(e.code, &message); }
    format!("{} {:?}: {message}", e.code, e.span)
}
#[derive(PartialEq)]
pub(super) struct BuildInput {
    pub source: String,
    pub parts: Vec<Part>,
    pub policy: Option<String>,
}
impl BuildInput {
    pub fn read(input: &Path) -> Result<Self, String> { Self::read_under(input, None) }
    pub fn read_under(input: &Path, root: Option<&Path>) -> Result<Self, String> {
        let (source, parts) = read_bundle_under(input, root)?;
        if root.is_none() {
            for part in parts.iter().filter(|p| standard(&p.path).is_none()) {
                let identity=part.path.canonicalize().map_err(|e|e.to_string())?;
                if let Some(owner)=identity.ancestors().skip(1).find(|p|p.join("witchy.toml").is_file())
                    && owner.join("test.wy").is_file() {
                    if identity==owner.join("test.wy") || identity.starts_with(owner.join("tests")) {
                        return Err("production builds cannot include package test members".into());
                    }
                    use std::os::unix::fs::MetadataExt;
                    if identity.metadata().map_err(|e|e.to_string())?.nlink()!=1 {
                        return Err("a production source has multiple filesystem links".into());
                    }
                }
            }
        }
        let policy = super::compiler_session::optional_text(&input.with_extension("policy"))?;
        Ok(Self {source,parts,policy})
    }
}

pub(super) fn build_in(input: &Path, output: &Path, expected: Option<&str>, session: Option<&mut super::proof_session::Session>, finish: bool) -> Result<(String, super::Measurements), String> {
    let start = std::time::Instant::now();
    let snapshot = BuildInput::read(input)?;
    build_loaded(input, output, expected, session, finish, start, &snapshot, None, &|| Ok(()))
}

pub(super) fn build_loaded(input: &Path, output: &Path, expected: Option<&str>, mut session: Option<&mut super::proof_session::Session>, finish: bool,
    start: std::time::Instant, snapshot: &BuildInput, reviewed: Option<std::sync::Arc<super::compiler_session::CheckedSource>>, validate: &dyn Fn() -> Result<(), String>) -> Result<(String, super::Measurements), String> {
    build_image(input, output, expected, session.take(), finish, start, snapshot, reviewed, validate, &snapshot.source, None)
}

pub(super) fn build_image(input: &Path, output: &Path, expected: Option<&str>, mut session: Option<&mut super::proof_session::Session>, finish: bool,
    start: std::time::Instant, snapshot: &BuildInput, reviewed: Option<std::sync::Arc<super::compiler_session::CheckedSource>>, validate: &dyn Fn() -> Result<(), String>,
    source: &str, test: Option<(&Path, &[u8])>) -> Result<(String, super::Measurements), String> {
    let parts = &snapshot.parts;
    let gate = policy_gate(input, snapshot);
    if let (Ok(input), Ok(output)) = (fs::canonicalize(input), fs::canonicalize(output)) {
        if input == output { return Err("source and module output must be different files".into()); }
    }
    // Keep the existing proof-free ordinary build independent of the platform SHA provider.
    let source_id = if test.is_some() || expected.is_some() || session.as_ref().is_some_and(|s| s.production()) {
        Some(super::proof_digest::bytes(source.as_bytes()).map_err(|e| e.to_string())?)
    } else { None };
    if expected.is_some() && expected != source_id.as_deref() { return Err("source differs from requested identity".into()); }
    let read_ns = start.elapsed().as_nanos();
    let (mut bytes, report, mut measurements) = super::compile_gated(&source, &starts(&parts), session.as_deref_mut(), Some(&gate), reviewed).map_err(|e| render_build_refusal(source, parts, &e))?;
    measurements.read_ns = read_ns;
    if let Some(checker) = session.as_deref_mut() {
        checker.validate_publication().map_err(|e| display(&e))?;
        if finish { checker.finish().map_err(|e| display(&e))?; }
    }
    // Compare full source bytes, not mtime; publication describes this exact snapshot.

    if let Some((_, section)) = test { bytes.extend_from_slice(section); }
    let report = if let Some(source_id) = source_id {
        let wasm_id = super::proof_digest::bytes(&bytes).map_err(|e| e.to_string())?;
        format!("{},\"source_sha256\":\"{source_id}\",\"wasm_sha256\":\"{wasm_id}\"}}", report.strip_suffix('}').expect("checker JSON"))
    } else { report };
    let start = std::time::Instant::now();
    if &BuildInput::read_under(input, test.map(|(root, _)| root))? != snapshot { return Err("source or policy changed during checking; output preserved".into()); }
    validate()?;
    publish(output, &bytes).map_err(|e| format!("{}: {e}", output.display()))?;
    measurements.publish_ns = start.elapsed().as_nanos();
    Ok((report, measurements))
}

fn render_build_refusal(source: &str, parts: &[Part], error: &crate::diagnostic::Diagnostic) -> String {
    let mut all = vec![render(source, parts, error)];
    all.extend(super::check::later_refusals().iter().map(|later| render(source, parts, later)));
    all.join("\n")
}

fn policy_gate(input: &Path, snapshot: &BuildInput)
    -> impl Fn(&crate::ast::Program, &super::logical_body::Program) -> Result<(), String>
{
    let BuildInput {parts, policy:rules, ..} = snapshot;
    // A policy kept beside the program (`app.policy` for `app.wy`) holds every build to it,
    // before anything is proved: a package that reaches further, or changed since its line was
    // reviewed, is refused (docs/wasm-capabilities.md). It is checked on the very program the
    // build parsed and typed (once: a second parse and check cost about 40% of a build).
    let policy = input.with_extension("policy");

    let base = policy.parent().filter(|dir| !dir.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    move |syntax: &crate::ast::Program, logical: &super::logical_body::Program| -> Result<(), String> {
        // The key law trusts String and Span as the library's (wasm/key_law.rs): each it relied on
        // must be declared in a standard file, not a program's own type of that name (review).
        let edges = parts.iter().enumerate().flat_map(|(from,p)| p.edges.iter().map(move |(alias,to)| (from as u32,alias,*to)));
        if !edges.eq(syntax.imports.iter().filter(|(from,_,_)| (*from as usize)<parts.len()).map(|(from,name,to)| (*from,name,*to))) {
            return Err("module imports must come from the resolved source graph".into());
        }
        // The key law trusts String and Span as the library's (wasm/key_law.rs): each it relied on
        // must be declared in the library's own file, not a program's own type of that name
        // (review). And what that file reaches must be the library's too (review round 2: a copy
        // of wasm-string.wy with the library's exact text imported an edited wasm-vec.wy beside
        // it): the library's files import one another by their own names, so every bundled file
        // with a library file's name must hold exactly that file's text.
        if !syntax.key_roots.is_empty() {
            // Each part compared with the library's text once, not once per root that names it.
            let library: Vec<bool> = parts.iter().map(library_part).collect();
            if let Some((name, _)) = syntax.key_roots.iter().find(|(_, m)| !library.get(*m as usize).copied().unwrap_or(false)) {
                if name.starts_with("impl ") {
                    return Err(format!("`{name}` is trusted as the library's by the key law, and this one is written in a file of the program: only the library implements these traits for its own key types"));
                }
                return Err(format!("`{name}` is trusted as the library's by the key law, and this `{name}` is declared in a file of the program, not the library: give the program's own another name, or import the library's"));
            }
            if let Some((part, _)) = parts.iter().zip(&library).find(|(part, library)| STANDARD.iter().any(|(file, _)| part.path.file_name().and_then(|n| n.to_str()) == Some(*file) || part.identity.file_name().and_then(|n| n.to_str()) == Some(*file)) && !**library) {
                return Err(format!("{} has the name of a standard library file and other text, and the key law trusts the library's String and Span: the program may not stand in its own copy", part.path.display()));
            }
            // By identity, not name (review round 13: an edited vec imported first as
            // `WASM-VEC.WY`, or through a symlink, was the file a byte-exact copy of wasm-string.wy
            // then got for `import "wasm-vec.wy"`, since a file enters the bundle once): every file
            // an on-disk library copy imports must itself hold the library's text.
            for (part, _) in parts.iter().zip(&library).filter(|(_, library)| **library) {
                if standard(&part.path).is_some() { continue; }
                for identity in &part.imports {
                    let Some(target) = parts.iter().position(|p| p.identity == *identity) else {
                        return Err(format!("{} imports {}, which is not in the build", part.path.display(), identity.display()));
                    };
                    let (target, library) = (&parts[target], library[target]);
                    if !library {
                        return Err(format!("{} imports {}, which is not the library's text, and the key law trusts the library's files: the program may not stand in its own copy", part.path.display(), target.path.display()));
                    }
                }
            }
        }
        let Some(rules) = rules else { return Ok(()) };
        let uses = super::caps::file_uses(logical, super::caps::main_grant(syntax), &names(&parts, &base));
        let mut violations = super::caps::check_policy(&uses, rules)?;
        violations.extend(super::caps::check_digests(&digests(&parts, &base)?, rules)?);
        if violations.is_empty() { return Ok(()); }
        Err(format!("{}: the program does not fit its policy: {}", policy.display(), violations.join("; ")))
    }
}

#[cfg(all(target_arch = "aarch64", any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
pub(super) fn build_native(input: &Path, output: &Path, measure: bool, manifest: Option<&Path>, expected_manifest: Option<&str>) -> Result<(), String> {
    let mut session = super::proof_session::Session::publishing().map_err(|e| display(&e))?;
    let result = build_native_in(input, output, None, session.as_mut(), true, manifest, expected_manifest);
    if result.is_err() && let Some(checker) = &mut session { checker.refuse(); }
    let times = result?;
    if measure { println!("{}", times.json()); }
    Ok(())
}

#[cfg(all(target_arch = "aarch64", any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
pub(super) fn build_native_in(input: &Path, output: &Path, expected: Option<&str>,
    session: Option<&mut super::proof_session::Session>, finish: bool,
    manifest: Option<&Path>, expected_manifest: Option<&str>,
) -> Result<super::native::Times, String> {
    build_native_snapshot(input, output, expected, session, finish, manifest, expected_manifest, &|_| Ok(()))
}

/// The resident compiler's manifest-bound native build: `current` checks the request's input
/// key against the snapshot read, and again against the snapshot before publication.
#[cfg(all(target_arch = "aarch64", any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
pub(super) fn build_native_checked(input: &Path, output: &Path, session: Option<&mut super::proof_session::Session>,
    manifest: &Path, expected_manifest: &str, current: &dyn Fn(&BuildInput) -> Result<(), String>,
) -> Result<super::native::Times, String> {
    build_native_snapshot(input, output, None, session, true, Some(manifest), Some(expected_manifest), current)
}

#[cfg(all(target_arch = "aarch64", any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
#[allow(clippy::too_many_arguments)]
fn build_native_snapshot(input: &Path, output: &Path, expected: Option<&str>,
    mut session: Option<&mut super::proof_session::Session>, finish: bool,
    manifest: Option<&Path>, expected_manifest: Option<&str>, current: &dyn Fn(&BuildInput) -> Result<(), String>,
) -> Result<super::native::Times, String> {
    let start = std::time::Instant::now();
    let snapshot = BuildInput::read(input)?;
    current(&snapshot)?;
    if let Some(expected) = expected {
        let actual = super::proof_digest::bytes(snapshot.source.as_bytes()).map_err(|e| e.to_string())?;
        if actual != expected { return Err("build request source identity mismatch".into()); }
    }
    // The launch manifest is build input: read once, its bytes compared again before
    // publication, and its digest embedded in the plan (docs/native-trusted-executables.md).
    let launch = manifest.map(super::read_manifest).transpose()?;
    if let Some(expected) = expected_manifest && launch.as_ref().map(|(_, digest)| digest.as_str()) != Some(expected) {
        return Err("build request manifest identity mismatch".into());
    }
    // Refuse replacement of any input file, including an imported source.
    if let Ok(target) = output.canonicalize()
        && (snapshot.parts.iter().any(|part| part.identity == target)
            || input.with_extension("policy").canonicalize().ok().as_ref() == Some(&target)
            || manifest.and_then(|m| m.canonicalize().ok()).as_ref() == Some(&target))
    {
        return Err("source, policy, manifest and executable output must be different files".into());
    }
    let gate = policy_gate(input, &snapshot);
    let read = start.elapsed();
    let (object, mut times) = super::native::compile(
        &snapshot.source, &starts(&snapshot.parts), session.as_deref_mut(), &gate,
        launch.as_ref().map(|(text, digest)| super::native::Manifest { text, digest }),
    ).map_err(|e| {
        let mut all = vec![render(&snapshot.source, &snapshot.parts, &e)];
        all.extend(super::check::later_refusals().iter()
            .map(|later| render(&snapshot.source, &snapshot.parts, later)));
        all.join("\n")
    })?;
    times.read_ns = read.as_nanos();
    let image = super::native::link(output, &object, &mut times)?;
    let start = std::time::Instant::now();
    if let Some(checker) = &mut session {
        checker.validate_publication().map_err(|e| display(&e))?;
        if finish { checker.finish().map_err(|e| display(&e))?; }
    }
    times.proof_ns += start.elapsed().as_nanos();

    let start = std::time::Instant::now();
    let reread = BuildInput::read(input)?;
    if reread != snapshot {
        return Err("source or policy changed during checking; output preserved".into());
    }
    current(&reread)?;
    // Exact equality with the admitted manifest also preserves its UTF-8 and digest.
    if let (Some(path), Some((text, _))) = (manifest, &launch)
        && !super::read_manifest_bytes(path).is_ok_and(|now| now == text.as_bytes())
    {
        return Err("launch manifest changed during checking; output preserved".into());
    }
    image.publish(output)?;
    times.publish_ns = start.elapsed().as_nanos();
    Ok(times)
}

pub(super) fn publish(output: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // create_new prevents following a pre-existing temporary symlink. The
    // temporary is in the destination directory, so rename is atomic and never
    // crosses filesystems. Failed writes and rename leave the old output intact.
    // The file is not synced to disk: that took 5 to 8 ms of a 20 ms build, and a
    // module lost to a power failure is rebuilt from its source. A reader still
    // sees the old module or the whole new one, never part of one.
    let temporary = parent.join(format!(
        ".witchy-wasm-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        drop(file);
        fs::rename(&temporary, output)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    #[test]
    fn native_publication_refuses_a_manifest_changed_after_linking() {
        use std::cell::Cell;

        let root = std::env::temp_dir().join(format!("witchy-native-manifest-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let input = root.join("main.wy");
        let manifest = root.join("main.grants.toml");
        let output = root.join("app");
        let original = b"[launch]\nversion = 1\n# a\n";
        fs::write(&input, "fn main(out: Console<Write>):\n    out.print_int(7)\n").unwrap();
        fs::write(&manifest, original).unwrap();
        fs::write(&output, b"prior executable").unwrap();
        let identity = super::super::proof_digest::bytes(original).unwrap();
        let rechecking = Cell::new(false);
        let result = build_native_checked(&input, &output, None, &manifest, &identity, &|_| {
            if rechecking.replace(true) {
                fs::write(&manifest, b"[launch]\nversion = 1\n# b\n").map_err(|e| e.to_string())?;
            }
            Ok(())
        });
        let preserved = fs::read(&output).unwrap();
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(result.err().as_deref(), Some("launch manifest changed during checking; output preserved"));
        assert_eq!(preserved, b"prior executable");
    }

    #[test]
    fn imports_bundle_each_file_once_before_its_importer_and_locate_diagnostics() {
        let root = std::env::temp_dir().join(format!("witchy-imports-{}", std::process::id()));
        fs::create_dir_all(root.join("lib")).unwrap();
        let write = |name: &str, text: &str| fs::write(root.join(name), text).unwrap();
        write("lib/base.wy", "fn base():\n    return");
        write("lib/mid.wy", "// header comment\nimport \"base.wy\"\nfn mid():\n    return\n");
        write("lib/own.wy", "import \"own.wy\"\nfn own():\n    return\n");
        write("selfish.wy", "import \"lib/own.wy\"\nfn main():\n    return\n");
        write("main.wy", "import \"lib/mid.wy\"\nimport \"lib/base.wy\"\nfn main():\n    return\n");
        write("plain.wy", "fn main():\n    return");
        write("late.wy", "fn main():\n    return\nimport \"lib/base.wy\"\n");
        write("missing.wy", "import \"absent.wy\"\n");

        let (text, parts) = read_bundle(&root.join("main.wy")).unwrap();
        // Each import line becomes `import NAME FILE` for the parser.
        assert_eq!(text, "fn base():\n    return\n// header comment\nimport base 0\nfn mid():\n    return\nimport mid 1\nimport base 0\nfn main():\n    return\n");
        // A file importing itself (or one importing it, at any depth) is a cycle, refused: the
        // edge would name a file not yet read, and an edge dropped would hide what it reaches.
        assert!(read_bundle(&root.join("selfish.wy")).err().is_some_and(|e| e.contains("import cycle")));
        assert_eq!(parts.len(), 3);
        // Line three of the importer stays line three although both imports were bundled.
        let at = text.find("fn main").unwrap();
        let shown = render(&text, &parts, &crate::diagnostic::Diagnostic::new("W0401", at..at + 2, "example"));
        assert!(shown.contains("main.wy:3:1: W0401: example") && shown.ends_with("fn main():"), "{shown}");
        // No place at all is said in words, never as `0..0`.
        let whole = render(&text, &parts, &crate::diagnostic::Diagnostic::new("W0403", 0..0, "example"));
        assert_eq!(whole, "W0403 (whole program, no single line): example");
        assert_eq!(crate::diagnostic::unplaced("W0402", "deadline"), "W0402 (proof check, no source line): deadline");
        let at = text.find("return\n// header").unwrap();
        let shown = render(&text, &parts, &crate::diagnostic::Diagnostic::new("W0401", at..at + 6, "example"));
        assert!(shown.contains("base.wy:2:5: W0401"), "{shown}");

        // No import: exact bytes, with no added newline. A late import is ordinary (refused) source.
        assert_eq!(read_source(&root.join("plain.wy")).unwrap(), "fn main():\n    return");
        assert!(read_source(&root.join("late.wy")).unwrap().contains("import \"lib/base.wy\""));
        assert!(read_source(&root.join("missing.wy")).unwrap_err().contains("absent.wy"));
        fs::remove_dir_all(root).unwrap();
    }

    /// A file's digest is of its own bytes (review of 9c8ac74b): two files of a package that swap
    /// what they import change their digests, and what the program imports before the package
    /// does not change the package's.
    #[test]
    fn a_files_digest_is_of_its_own_text() {
        let root = std::env::temp_dir().join(format!("witchy-digests-{}", std::process::id()));
        for dir in ["cfg/x", "cfg/y"] { fs::create_dir_all(root.join(dir)).unwrap(); }
        let write = |name: &str, text: &str| fs::write(root.join(name), text).unwrap();
        write("cfg/x/util.wy", "pub fn one() -> U64:\n    return 1\n");
        write("cfg/y/util.wy", "pub fn two() -> U64:\n    return 2\n");
        write("other.wy", "pub fn three() -> U64:\n    return 3\n");
        write("main.packages", "cfg: cfg/cfg.wy\n");
        let digests_of = |cfg: &str, a: &str, main: &str| {
            write("cfg/cfg.wy", cfg);
            write("cfg/a.wy", a);
            write("main.wy", main);
            let (_, parts) = read_bundle(&root.join("main.wy")).unwrap();
            digests(&parts, &root).unwrap().into_iter().filter(|(name, _)| name.starts_with("@cfg")).collect::<Vec<_>>()
        };
        let main = "import \"@cfg\"\nfn main():\n    return\n";
        let before = digests_of("import \"a.wy\"\nimport \"y/util.wy\"\n", "import \"x/util.wy\"\n", main);
        let swapped = digests_of("import \"a.wy\"\nimport \"x/util.wy\"\n", "import \"y/util.wy\"\n", main);
        assert_ne!(before.iter().find(|(n, _)| n == "@cfg"), swapped.iter().find(|(n, _)| n == "@cfg"));
        assert_ne!(before.iter().find(|(n, _)| n == "@cfg/a.wy"), swapped.iter().find(|(n, _)| n == "@cfg/a.wy"));
        let later = digests_of("import \"a.wy\"\nimport \"y/util.wy\"\n", "import \"x/util.wy\"\n", &format!("import \"other.wy\"\n{main}"));
        assert_eq!(before, later);
        let bytes = fs::read(root.join("cfg/cfg.wy")).unwrap();
        assert_eq!(later.iter().find(|(n, _)| n == "@cfg").unwrap().1, super::super::proof_digest::pin(&bytes).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    // A refusal's import advice names the standard module that declares a type, a free function
    // or a method; each named module is one `import` reaches. Nothing for a name none declares.
    #[test]
    fn import_advice_names_the_declaring_standard_module() {
        assert_eq!(module_declaring_type("HashMap").as_deref(), Some("hashmap"));
        assert_eq!(module_declaring_type("Json").as_deref(), Some("json_tree"));
        assert_eq!(module_declaring_type("Span").as_deref(), Some("span"));
        assert_eq!(module_declaring_type("NoSuchType"), None);
        // A prefix of a declared name is not that name.
        assert_eq!(module_declaring_type("HashMa"), None);
        // A trait is found too (2026-11-22: a reach refusal names `import discard_core`).
        assert_eq!(module_declaring_type("Discard").as_deref(), Some("discard_core"));
        assert_eq!(module_declaring_type("Hash").as_deref(), Some("hash"));
        assert_eq!(module_declaring_function("sort").as_deref(), Some("sort"));
        assert_eq!(module_declaring_function("push_base64").as_deref(), Some("encoding"));
        assert_eq!(module_declaring_function("push_base"), None);
        // A `pub nopanic fn` is found as a plain `pub fn` is.
        assert_eq!(module_declaring_function("replace").as_deref(), Some("replace"));
        assert_eq!(module_declaring_method("String", "find_text").as_deref(), Some("text"));
        assert_eq!(module_declaring_method("U64", "min").as_deref(), Some("int"));
        assert_eq!(module_declaring_method("Vec", "reverse").as_deref(), Some("vec"));
        // A trait's method in `impl Trait for T`.
        assert_eq!(module_declaring_method("F64", "show_on").as_deref(), Some("float"));
        assert_eq!(module_declaring_method("String", "no_such_method"), None);
        for module in ["hashmap", "json_tree", "span", "sort", "encoding", "text", "int", "vec", "float"] {
            assert!(standard(Path::new(STANDARD_ROOT).join(format!("wasm-{}.wy", module.replace('_', "-"))).as_path()).is_some(), "{module}");
        }
    }

    // A second place in a message: `⟪at N⟫` becomes `file:line` from the source, `earlier`
    // without it; text that only looks like a marker is kept as it is.
    #[test]
    fn a_second_place_in_a_message_is_written_out() {
        assert_eq!(places("first at ⟪at 3⟫", None), "first at earlier");
        assert_eq!(places("no marker", None), "no marker");
        assert!(matches!(places("no marker", None), Cow::Borrowed(_)));
        assert_eq!(places("open ⟪at 3 never closed", None), "open ⟪at 3 never closed");
        assert_eq!(places("⟪at x⟫ and ⟪at 1⟫", None), "earlier and earlier");
        let text = "fn a():\n    ()\nfn a():\n";
        let parts = vec![Part { start: 0, path: std::path::PathBuf::from("dir/prog.wy"), identity: std::path::PathBuf::from("dir/prog.wy"), raw: String::new(), imports: Vec::new(), edges: Vec::new(), package: None }];
        let second = text.rfind("fn a").unwrap();
        assert_eq!(places(&format!("again; first at ⟪at 0⟫, this at ⟪at {second}⟫"), Some((text, &parts))), "again; first at prog.wy:1, this at prog.wy:3");
        let error = crate::diagnostic::Diagnostic::new("W0403", 0..0, "first at ⟪at 0⟫");
        assert_eq!(render(text, &parts, &error), "W0403 (whole program, no single line): first at prog.wy:1");
        assert_eq!(display(&error), "W0403 (whole program, no single line): first at earlier");
    }
}
