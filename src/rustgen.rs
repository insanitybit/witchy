//! Native backend: transpile a witchy module to Rust source, to be compiled by
//! `rustc`/LLVM into a native binary.
//!
//! witchy's surface (Rust-flavoured words, expression-oriented blocks, `a..b`
//! ranges) maps almost one-to-one onto Rust, so the compute core — functions
//! over `Int`/`Float`/`Bool`/`String`, arithmetic, control flow, recursion,
//! range `for`, `print`/`int_to_string` — transpiles directly and is then
//! optimized by LLVM (which routinely matches or beats Go's compiler). Anything
//! outside that subset returns a clear `Err` rather than emitting wrong code.
//!
//! NOTE: native output is NOT capability-sandboxed (it's an ordinary native
//! binary). It is for trusted, performance-critical code; untrusted code should
//! keep using the wasm `sandbox` path, where only granted host functions exist.

use crate::ast::{BinOp, Block, Expr, Function, Item, Module, Param, Stmt, Type, UnOp};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// A concrete function signature: the Rust types of its parameters and its return
/// type. Used to coerce a fn-reference value to a `Rc<dyn Fn(..) -> ..>`.
type FnSig = (Vec<String>, String);

thread_local! {
    /// Field names of each supported record type (single-variant, named fields,
    /// all-supported field types), keyed by constructor name. Populated up front
    /// by `transpile_module` so a positional `Ctor` can be emitted as a named
    /// Rust struct literal, and so record-typed params resolve in `rust_ty`.
    static RECORD_FIELDS: RefCell<HashMap<String, Vec<String>>> = RefCell::new(HashMap::new());
    /// Each supported enum's variant name -> enum name, so a constructor or a
    /// constructor pattern can be qualified as `Enum::Variant`.
    static VARIANT_TO_ENUM: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    /// The names of supported enum types (for `rust_ty`).
    static ENUM_NAMES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// Per enum constructor, which positional fields are recursive (so `Box`ed in
    /// Rust): wrapped with `Box::new` at construction, moved out (`*x`) at match.
    static VARIANT_BOXED: RefCell<HashMap<String, Vec<bool>>> = RefCell::new(HashMap::new());
    /// Per (emitted) function, whether each parameter is cloned at call sites
    /// (see `needs_clone_at_call`) so the caller keeps its value (witchy value
    /// semantics; Rust would otherwise move it and reject the caller's later use).
    static FN_PARAM_CLONE: RefCell<HashMap<String, Vec<bool>>> = RefCell::new(HashMap::new());
    /// Per function (by emitted name): which parameter positions are an explicit
    /// `let` borrow of a borrowable type, lowered to `&T`. A call site passes
    /// `&arg` (or the var itself if it's already a borrow) instead of cloning.
    static FN_PARAM_BORROW: RefCell<HashMap<String, Vec<bool>>> = RefCell::new(HashMap::new());
    /// Names of the current function's `&T` borrow parameters — so a use of one
    /// (forwarding it to another borrow, or iterating it) knows it's a reference.
    static CUR_BORROW: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// Set when the program uses a Dict, so the `WDict` runtime helper is emitted.
    static USES_DICT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Set when the program uses a Dir capability, so the I/O helper is emitted.
    static USES_DIR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Set when the program uses a Net capability, so the socket helper is emitted.
    static USES_NET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Set when a value is shown (generic `to_string`/`print`), so the `WShow`
    /// formatter (matching the interpreter's `Display`) is emitted.
    static USES_SHOW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Set when the program declares actors, so the actor runtime is emitted and
    /// `main` drains the mailbox after its body (the deterministic FIFO scheduler).
    static HAS_ACTORS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Message constructor names (across all actor handlers), so a `Ctor` in
    /// `send(target, Msg(..))` is recognized as a message, not a type constructor.
    static MSG_NAMES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// Each actor's fields (in declaration order), so `spawn Actor(args)` builds
    /// the struct: a field with an initializer defaults, others take spawn args.
    static ACTOR_FIELDS: RefCell<HashMap<String, Vec<crate::ast::Field>>> = RefCell::new(HashMap::new());
    /// True while emitting an actor handler body. `spawn` there would re-borrow the
    /// actor registry the drain loop already holds, so it's rejected.
    static IN_HANDLER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Parameter names of the function currently being emitted. A call whose name
    /// is one of these is a function-valued parameter (a closure), called
    /// directly (`f(x)`); any other call is a top-level function (`w_f(x)`).
    static CUR_PARAMS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// Functions with `inout` parameters: name -> the inout parameter positions.
    /// Such a function returns its inout values; a statement-position call
    /// reassigns the corresponding (variable) arguments — Hylo write-back.
    static FN_INOUT: RefCell<HashMap<String, Vec<usize>>> = RefCell::new(HashMap::new());
    /// Bindings whose value may be *moved* (not cloned) at their use, because the
    /// use is their last (single, loop-free) use in scope — last-use elision. Lets
    /// recursive traversals pass subtrees by move instead of deep-cloning them.
    static MOVEABLE: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// `Var` occurrences (AST-node pointers, as usize) the liveness pass found to
    /// be a last use — moveable even if the binding is used more than once.
    static MOVEABLE_OCC: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
    /// Variables holding a closure (`impl Fn`): they can't be cloned (no `Clone`),
    /// so a value position moves them. Function params of `Fn` type, and `let`s
    /// bound directly to a lambda.
    static NOCLONE: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// All top-level function names (module-qualified). A bare `Var` matching one
    /// is a first-class function value (`map(xs, dbl)`) -> the Rust `w_*` item.
    static FN_NAMES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// User record/enum types that get a `WShow` impl (all fields showable).
    static SHOWABLE_TYPES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// Top-level function signatures (param Rust types, return Rust type) when
    /// fully concrete — used to coerce a fn-reference value to `Rc<dyn Fn>`.
    /// `None` for a generic signature (a `dyn Fn` cast would have unbound vars).
    static FN_SIGS: RefCell<HashMap<String, Option<FnSig>>> = RefCell::new(HashMap::new());
}

/// Is `name` a top-level function used here as a first-class value?
fn is_fn_ref(name: &str) -> bool {
    FN_NAMES.with(|m| m.borrow().contains(name))
}

/// A named function used as a value: `Rc::new(w_*)`. When the function's
/// signature is fully concrete, coerce it to `Rc<dyn Fn(..) -> ..>` through a
/// typed `let` (so e.g. `[double, bonus]` — two distinct fn items — unify into
/// one `Vec<Rc<dyn Fn>>`; bare `Rc::new` would keep their unique fn-item types).
/// A function's (param Rust types, return Rust type) when fully concrete (no type
/// variables, every type supported); `None` otherwise. Used to type a fn value
/// as `Rc<dyn Fn>`. A generic signature is skipped (its `dyn Fn` would be open).
fn concrete_fn_sig(f: &Function) -> Option<FnSig> {
    let mut tvs = std::collections::BTreeSet::new();
    for p in &f.params {
        if let Some(t) = &p.ty {
            collect_type_vars(t, &mut tvs);
        }
    }
    if let Some(t) = &f.ret {
        collect_type_vars(t, &mut tvs);
    }
    if !tvs.is_empty() {
        return None;
    }
    let params: Option<Vec<String>> =
        f.params.iter().map(|p| p.ty.as_ref().and_then(rust_ty)).collect();
    let ret = match &f.ret {
        Some(t) => rust_ty(t)?,
        None => "()".to_string(),
    };
    Some((params?, ret))
}

fn fn_ref_value(name: &str) -> String {
    let sig = FN_SIGS.with(|m| m.borrow().get(name).cloned()).flatten();
    match sig {
        Some((params, ret)) => format!(
            "{{ let __f: std::rc::Rc<dyn Fn({}) -> {}> = std::rc::Rc::new(w_{}); __f }}",
            params.join(", "),
            ret,
            ident(name)
        ),
        None => format!("std::rc::Rc::new(w_{})", ident(name)),
    }
}

/// Is this user type one we can generate a `WShow` impl for? (All its fields
/// are showable — see the fixpoint in `compute_showable_types`.)
fn is_showable(name: &str) -> bool {
    SHOWABLE_TYPES.with(|s| s.borrow().contains(name))
}

/// Can a value of this type be shown (does it — or will it — impl `WShow`)? A
/// closure, dict, or capability can't; a user type is showable per `showable`
/// (the in-progress fixpoint set); a type variable relies on its impl bound.
fn type_showable(t: &Type, showable: &HashSet<String>) -> bool {
    match t {
        Type::Named(n, args) => match n.as_str() {
            "Int" | "Float" | "Bool" | "String" | "Duration" => true,
            "List" | "Option" => args.first().is_some_and(|a| type_showable(a, showable)),
            "Result" => args.len() == 2 && args.iter().all(|a| type_showable(a, showable)),
            // No `WShow` for dicts (order unspecified), closures, or capabilities.
            "Dict" | "Console" | "Env" | "Dir" | "Net" | "Socket" | "Listener" => false,
            other if is_type_var(other) => true,
            other => showable.contains(other),
        },
        // Only the tuple arities with a `WShow` impl (see SHOW_HELPER).
        Type::Tuple(ts) => (2..=4).contains(&ts.len()) && ts.iter().all(|a| type_showable(a, showable)),
        Type::Fn(..) => false,
    }
}

/// The user record/enum types for which every field is showable, so a `WShow`
/// impl can be generated. A least-fixpoint: a type drops out if any field is
/// unshowable or references a type that has dropped out (recursion is fine).
fn compute_showable_types(m: &Module) -> HashSet<String> {
    let mut fields_of: HashMap<String, Vec<Type>> = HashMap::new();
    for item in &m.items {
        if let Item::Type(td) = item {
            if td.name == "Option" || td.name == "Result" {
                continue; // built-in; handled by SHOW_HELPER
            }
            let fs: Vec<Type> = td.variants.iter().flat_map(|v| v.fields.iter().cloned()).collect();
            fields_of.insert(td.name.clone(), fs);
        }
    }
    let mut showable: HashSet<String> = fields_of.keys().cloned().collect();
    loop {
        let mut changed = false;
        for name in showable.iter().cloned().collect::<Vec<_>>() {
            if !fields_of[&name].iter().all(|t| type_showable(t, &showable)) {
                showable.remove(&name);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    showable
}

/// The `impl` generics (`<A: WShow, B: WShow>`) and type reference (`Foo<A, B>`)
/// for a `WShow` impl on a user type with the given type variables.
fn wshow_header(name: &str, tvs: &std::collections::BTreeSet<String>) -> (String, String) {
    if tvs.is_empty() {
        return (String::new(), name.to_string());
    }
    // `+ Clone + 'static` matches the type's own generic bound (see
    // `generic_params`), so the impl satisfies `Foo<A>`'s requirements.
    let bounds: Vec<String> = tvs.iter().map(|v| format!("{v}: WShow + Clone + 'static")).collect();
    let vars: Vec<String> = tvs.iter().cloned().collect();
    (format!("<{}>", bounds.join(", ")), format!("{name}<{}>", vars.join(", ")))
}

/// Whether a bare variable may be moved (rather than cloned) at this use.
fn is_moveable(name: &str) -> bool {
    MOVEABLE.with(|m| m.borrow().contains(name))
}

/// Whether an explicit `let` parameter of this type lowers to a borrow (`&T`) on
/// native. Restricted to the big heap types where a borrow avoids a real clone
/// and reads still work through Rust's auto-deref (`String`/`List`/`Dict`);
/// everything else stays owned.
fn borrowable_ty(t: &Type) -> bool {
    matches!(t, Type::Named(n, _) if n == "String" || n == "List" || n == "Dict")
}

/// Whether this parameter is an explicit-`let` borrow lowered to `&T`.
fn is_borrow_param(p: &crate::ast::Param) -> bool {
    p.convention == crate::ast::Convention::Borrow && p.ty.as_ref().is_some_and(borrowable_ty)
}

/// Whether `name` is one of the current function's `&T` borrow parameters.
fn is_cur_borrow(name: &str) -> bool {
    CUR_BORROW.with(|b| b.borrow().contains(name))
}

/// Whether THIS specific `Var` occurrence (by AST-node pointer) is a last use the
/// liveness pass marked moveable. Lets a binding used several times still move at
/// its final use — the per-name `is_moveable` only covers used-exactly-once.
fn is_moveable_occ(e: &Expr) -> bool {
    MOVEABLE_OCC.with(|m| m.borrow().contains(&(e as *const Expr as usize)))
}

/// Whether a variable must NOT be cloned. Closures are now `Rc<dyn Fn>` (cheaply
/// cloneable), so nothing is no-clone — kept as a hook in case a future
/// non-cloneable value type appears.
fn is_noclone(_name: &str) -> bool {
    false
}

/// Render an expression in a *value* position (a `let`/assign value, a block or
/// branch tail, a constructor argument, a list/tuple element). A bare variable
/// is cloned here — value semantics: the binding stays usable afterward — unless
/// this is its last use (then moved) or it's a closure (which can't be cloned).
fn gen_value(e: &Expr) -> Result<String, String> {
    if let Expr::Var(v) = e {
        // A function value (fn item) or a closure isn't cloned; nor is a binding
        // at its last use. Everything else clones to preserve value semantics.
        if is_fn_ref(v) {
            return Ok(fn_ref_value(v));
        }
        if is_moveable(v) || is_moveable_occ(e) || is_noclone(v) {
            return Ok(local_ident(v));
        }
        return Ok(format!("({}).clone()", local_ident(v)));
    }
    gen_expr(e, true)
}

/// witchy's `Dict` maps to a real `HashMap` with a fast, non-DoS-resistant hash
/// (FxHash — the hasher rustc itself uses; std's default SipHash is the slow,
/// DoS-resistant one). std-only, so plain `rustc` can build it. Iteration order
/// is unspecified (like Go's maps); emitted only when the program uses a Dict.
const DICT_HELPER: &str = r#"
#[derive(Default)]
struct FxHasher(u64);
impl std::hash::Hasher for FxHasher {
    fn finish(&self) -> u64 { self.0 }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ (b as u64)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
    fn write_u64(&mut self, i: u64) { self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95); }
    fn write_i64(&mut self, i: i64) { self.write_u64(i as u64); }
}
type WMap<K, V> = std::collections::HashMap<K, V, std::hash::BuildHasherDefault<FxHasher>>;
"#;

/// A `Dir` capability is a confined directory subtree. These helpers port the
/// interpreter's path confinement (reject absolute/`..`, canonicalize and check
/// the result stays under the base — symlink-safe). Emitted only when used.
const DIR_HELPER: &str = r#"
fn w_dir_resolve(base: &std::path::Path, rel: &str) -> std::path::PathBuf {
    use std::path::Component;
    let p = std::path::Path::new(rel);
    if p.is_absolute() { panic!("absolute paths are not allowed (a Dir capability is a subtree)"); }
    for comp in p.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => panic!("`..` escapes the Dir capability"),
            _ => panic!("invalid path component in a Dir-relative path"),
        }
    }
    let joined = base.join(rel);
    let real = std::fs::canonicalize(&joined).unwrap_or_else(|e| panic!("cannot access `{}`: {}", joined.display(), e));
    let real_base = std::fs::canonicalize(base).unwrap_or_else(|e| panic!("invalid Dir base `{}`: {}", base.display(), e));
    if !real.starts_with(&real_base) { panic!("path escapes the Dir capability (via symlink)"); }
    real
}
fn w_dir_resolve_write(base: &std::path::Path, rel: &str) -> std::path::PathBuf {
    use std::path::Component;
    let p = std::path::Path::new(rel);
    if p.is_absolute() { panic!("absolute paths are not allowed (a Dir capability is a subtree)"); }
    for comp in p.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => panic!("`..` escapes the Dir capability"),
            _ => panic!("invalid path component in a Dir-relative path"),
        }
    }
    let joined = base.join(rel);
    let parent = joined.parent().unwrap_or(base);
    let real_parent = std::fs::canonicalize(parent).unwrap_or_else(|e| panic!("cannot access `{}`: {}", parent.display(), e));
    let real_base = std::fs::canonicalize(base).unwrap_or_else(|e| panic!("invalid Dir base: {}", e));
    if !real_parent.starts_with(&real_base) { panic!("path escapes the Dir capability (via symlink)"); }
    if let Ok(meta) = std::fs::symlink_metadata(&joined) {
        if meta.file_type().is_symlink() { panic!("path escapes the Dir capability (the target is a symlink)"); }
    }
    joined
}
fn w_dir_read(base: &std::path::Path, rel: &str) -> String {
    std::fs::read_to_string(w_dir_resolve(base, rel)).unwrap_or_else(|e| panic!("read failed: {}", e))
}
fn w_dir_write(base: &std::path::Path, rel: &str, contents: &str) {
    std::fs::write(w_dir_resolve_write(base, rel), contents).unwrap_or_else(|e| panic!("write failed: {}", e));
}
fn w_dir_exists(base: &std::path::Path, rel: &str) -> bool {
    use std::path::Component;
    let p = std::path::Path::new(rel);
    if p.is_absolute() { return false; }
    for comp in p.components() {
        match comp { Component::Normal(_) | Component::CurDir => {}, _ => return false }
    }
    let joined = base.join(rel);
    match (std::fs::canonicalize(&joined), std::fs::canonicalize(base)) {
        (Ok(real), Ok(real_base)) => real.starts_with(&real_base),
        _ => false,
    }
}
fn w_dir_make(base: &std::path::Path, name: &str) {
    let p = w_dir_resolve_write(base, name);
    std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("make_dir failed for `{}`: {}", p.display(), e));
}
fn w_dir_list(base: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(base)
        .unwrap_or_else(|e| panic!("list failed for `{}`: {}", base.display(), e))
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}
"#;

// The Net capability: a `Net` is an allow-list of `host:port`; sockets and
// listeners are handles into thread-local tables (matching the interpreter). The
// allow-list is read from `WITCHY_NET` (comma-separated), deny-by-default like
// the interpreter's `--net` flags — a standalone binary's network authority.
const NET_HELPER: &str = r#"
thread_local! {
    static W_SOCKETS: std::cell::RefCell<Vec<std::io::BufReader<std::net::TcpStream>>> = std::cell::RefCell::new(Vec::new());
    static W_LISTENERS: std::cell::RefCell<Vec<std::net::TcpListener>> = std::cell::RefCell::new(Vec::new());
}
fn w_net_grant() -> Vec<String> {
    std::env::var("WITCHY_NET")
        .map(|v| v.split(',').filter(|a| !a.is_empty()).map(|a| a.to_string()).collect())
        .unwrap_or_default()
}
fn w_net_restrict(allow: &[String], addr: &str) -> Vec<String> {
    if !allow.iter().any(|a| a == addr) { panic!("restrict: `{}` is not in this Net capability", addr); }
    vec![addr.to_string()]
}
fn w_net_connect(allow: &[String], addr: &str) -> usize {
    if !allow.iter().any(|a| a == addr) { panic!("connect: `{}` is not permitted by this Net capability", addr); }
    let stream = std::net::TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect to `{}` failed: {}", addr, e));
    W_SOCKETS.with(|s| { let mut s = s.borrow_mut(); s.push(std::io::BufReader::new(stream)); s.len() - 1 })
}
fn w_net_send_line(id: usize, line: &str) {
    use std::io::Write;
    W_SOCKETS.with(|s| {
        let mut s = s.borrow_mut();
        let sock = s.get_mut(id).expect("invalid socket");
        sock.get_mut().write_all(line.as_bytes()).and_then(|_| sock.get_mut().write_all(b"\n")).unwrap_or_else(|e| panic!("send failed: {}", e));
    });
}
fn w_net_send_bytes(id: usize, data: &str) {
    use std::io::Write;
    W_SOCKETS.with(|s| {
        let mut s = s.borrow_mut();
        let sock = s.get_mut(id).expect("invalid socket");
        sock.get_mut().write_all(data.as_bytes()).unwrap_or_else(|e| panic!("send failed: {}", e));
    });
}
fn w_net_recv_line(id: usize) -> String {
    use std::io::BufRead;
    W_SOCKETS.with(|s| {
        let mut s = s.borrow_mut();
        let sock = s.get_mut(id).expect("invalid socket");
        let mut line = String::new();
        sock.read_line(&mut line).unwrap_or_else(|e| panic!("recv failed: {}", e));
        line.trim_end_matches('\n').to_string()
    })
}
fn w_net_recv_all(id: usize) -> String {
    use std::io::Read;
    W_SOCKETS.with(|s| {
        let mut s = s.borrow_mut();
        let sock = s.get_mut(id).expect("invalid socket");
        let mut buf = Vec::new();
        sock.read_to_end(&mut buf).unwrap_or_else(|e| panic!("recv failed: {}", e));
        String::from_utf8_lossy(&buf).into_owned()
    })
}
fn w_net_recv_bytes(id: usize, n: i64) -> String {
    use std::io::Read;
    W_SOCKETS.with(|s| {
        let mut s = s.borrow_mut();
        let sock = s.get_mut(id).expect("invalid socket");
        let want = n.max(0) as usize;
        let mut buf = vec![0u8; want];
        let mut read = 0;
        while read < want {
            match sock.read(&mut buf[read..]) {
                Ok(0) => break,
                Ok(k) => read += k,
                Err(e) => panic!("recv failed: {}", e),
            }
        }
        buf.truncate(read);
        String::from_utf8_lossy(&buf).into_owned()
    })
}
fn w_net_listen(allow: &[String], addr: &str) -> usize {
    if !allow.iter().any(|a| a == addr) { panic!("listen: `{}` is not permitted by this Net capability", addr); }
    let listener = std::net::TcpListener::bind(addr).unwrap_or_else(|e| panic!("listen on `{}` failed: {}", addr, e));
    W_LISTENERS.with(|l| { let mut l = l.borrow_mut(); l.push(listener); l.len() - 1 })
}
fn w_net_accept(lid: usize) -> usize {
    let stream = W_LISTENERS.with(|l| {
        let l = l.borrow();
        let listener = l.get(lid).expect("invalid listener");
        listener.accept().unwrap_or_else(|e| panic!("accept failed: {}", e)).0
    });
    W_SOCKETS.with(|s| { let mut s = s.borrow_mut(); s.push(std::io::BufReader::new(stream)); s.len() - 1 })
}
fn w_net_close(id: usize) {
    W_SOCKETS.with(|s| {
        let s = s.borrow();
        if let Some(sock) = s.get(id) {
            let _ = sock.get_ref().shutdown(std::net::Shutdown::Both);
        }
    });
}
"#;

// `WShow` renders a value exactly as the interpreter's `Display` does — lists
// `[a, b]`, tuples `(a, b)`, `Some(x)`/`None`, `Ok`/`Err`, strings raw — so
// `print`/`to_string` (and f-strings) agree across backends. Dicts are omitted
// on purpose: their iteration order is unspecified, so formatting one isn't
// portable between backends.
const SHOW_HELPER: &str = r#"
// `w_fmt` writes a value into a formatter directly (no intermediate String); it
// is the primitive. `w_show()` (a provided method) builds a String from it for
// `print`/standalone `to_string`. f-string interpolation formats values through
// the `WDisplay` wrapper, so the whole string is built in ONE allocation rather
// than one per interpolated value.
trait WShow {
    fn w_fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result;
    fn w_show(&self) -> String { format!("{}", WDisplay(self)) }
}
struct WDisplay<'a, T: WShow + ?Sized>(&'a T);
impl<'a, T: WShow + ?Sized> std::fmt::Display for WDisplay<'a, T> {
    fn fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.0.w_fmt(__f) }
}
impl WShow for i64 { fn w_fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(__f, "{}", self) } }
impl WShow for f64 { fn w_fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(__f, "{}", self) } }
impl WShow for bool { fn w_fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(__f, "{}", self) } }
impl WShow for String { fn w_fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(__f, "{}", self) } }
impl<T: WShow> WShow for Vec<T> {
    fn w_fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(__f, "[")?;
        for (i, v) in self.iter().enumerate() { if i > 0 { write!(__f, ", ")?; } v.w_fmt(__f)?; }
        write!(__f, "]")
    }
}
impl<T: WShow> WShow for Option<T> {
    fn w_fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Some(v) => { write!(__f, "Some(")?; v.w_fmt(__f)?; write!(__f, ")") } None => write!(__f, "None") }
    }
}
impl<T: WShow, E: WShow> WShow for Result<T, E> {
    fn w_fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Ok(v) => { write!(__f, "Ok(")?; v.w_fmt(__f)?; write!(__f, ")") } Err(e) => { write!(__f, "Err(")?; e.w_fmt(__f)?; write!(__f, ")") } }
    }
}
impl<A: WShow, B: WShow> WShow for (A, B) {
    fn w_fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(__f, "(")?; self.0.w_fmt(__f)?; write!(__f, ", ")?; self.1.w_fmt(__f)?; write!(__f, ")") }
}
impl<A: WShow, B: WShow, C: WShow> WShow for (A, B, C) {
    fn w_fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(__f, "(")?; self.0.w_fmt(__f)?; write!(__f, ", ")?; self.1.w_fmt(__f)?; write!(__f, ", ")?; self.2.w_fmt(__f)?; write!(__f, ")") }
}
impl<A: WShow, B: WShow, C: WShow, D: WShow> WShow for (A, B, C, D) {
    fn w_fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(__f, "(")?; self.0.w_fmt(__f)?; write!(__f, ", ")?; self.1.w_fmt(__f)?; write!(__f, ", ")?; self.2.w_fmt(__f)?; write!(__f, ", ")?; self.3.w_fmt(__f)?; write!(__f, ")") }
}
"#;

/// The Rust grant for a capability parameter on `main` (Console is a no-op unit;
/// a Dir is rooted at the current directory `.`, matching the interpreter).
fn capability_grant(ty: &Type) -> Option<&'static str> {
    match ty {
        // Console/Env/Clock carry no data (their host fns reach ambient authority).
        Type::Named(n, _) if n == "Console" || n == "Env" || n == "Clock" => Some("()"),
        Type::Named(n, _) if n == "Dir" => {
            USES_DIR.with(|f| f.set(true));
            Some("std::path::PathBuf::from(\".\")")
        }
        Type::Named(n, _) if n == "Net" => {
            USES_NET.with(|f| f.set(true));
            Some("w_net_grant()")
        }
        // `main(args: List(String))` receives the command-line arguments.
        Type::Named(n, targs)
            if n == "List"
                && matches!(targs.as_slice(), [Type::Named(s, _)] if s == "String") =>
        {
            Some("std::env::args().skip(1).collect::<Vec<String>>()")
        }
        _ => None,
    }
}

fn fn_rust_name(f: &Function) -> String {
    if f.name == "main" {
        "main".to_string()
    } else {
        format!("w_{}", ident(&f.name))
    }
}

/// Whether an argument of this parameter type is cloned at the call site to
/// preserve value semantics (the caller keeps its value). Everything except
/// plain `Copy` scalars and function types — i.e. collections, strings, records,
/// enums, Option/Result, tuples, and type variables — is cloned. Cloning a
/// `Copy` type compiles to a copy, so over-cloning is harmless; closures aren't
/// cloned (they aren't `Clone`).
fn needs_clone_at_call(t: &Option<Type>) -> bool {
    match t {
        Some(Type::Named(n, _)) => {
            !matches!(
                n.as_str(),
                "Int" | "Float" | "Bool" | "Duration" | "Console" | "Env" | "Clock" | "Socket"
                    | "Listener"
            )
        }
        Some(Type::Tuple(_)) => true,
        // A function is now `Rc<dyn Fn>` — cheap to clone, so a reused closure
        // argument is cloned (the refcount) rather than moved.
        Some(Type::Fn(..)) => true,
        None => false,
    }
}

/// Clone an argument for value semantics only if it's a reused binding (a bare
/// variable); a temporary is a fresh value and is moved.
fn clone_if_var(arg: &Expr, rendered: String) -> String {
    if matches!(arg, Expr::Var(v) if !is_moveable(v) && !is_noclone(v)) && !is_moveable_occ(arg) {
        format!("({rendered}).clone()")
    } else {
        rendered
    }
}

/// The variable names a pattern binds (so they can be considered for last-use
/// move elision in the arm body). Wildcards/literals bind nothing.
fn pattern_bindings(p: &crate::ast::Pattern, out: &mut Vec<String>) {
    use crate::ast::Pattern;
    match p {
        Pattern::Var(n) => out.push(n.clone()),
        Pattern::Ctor { args, .. } | Pattern::Tuple(args) => {
            args.iter().for_each(|a| pattern_bindings(a, out))
        }
        _ => {}
    }
}

/// The free variables a block captures from an enclosing scope: names used but
/// not bound here (params/lets/loop-vars/match-bindings) and not a top-level
/// function. Used to clone captures into a `move` closure, so it can escape
/// while leaving the originals usable.
fn collect_free_vars(body: &Block, bound: &HashSet<String>, out: &mut HashSet<String>) {
    let mut local = bound.clone();
    for stmt in &body.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                fv_expr(value, &local, out);
                local.insert(name.clone());
            }
            Stmt::LetTuple { names, value } => {
                fv_expr(value, &local, out);
                for n in names {
                    local.insert(n.clone());
                }
            }
            Stmt::Assign { name, value } => {
                fv_expr(value, &local, out);
                if !local.contains(name) && !is_fn_ref(name) {
                    out.insert(name.clone());
                }
            }
            Stmt::Return(Some(e)) | Stmt::Yield(e) | Stmt::Expr(e) => fv_expr(e, &local, out),
            _ => {}
        }
    }
}

fn fv_expr(e: &Expr, bound: &HashSet<String>, out: &mut HashSet<String>) {
    use crate::ast::Expr as E;
    match e {
        E::Var(v) => {
            if !bound.contains(v) && !is_fn_ref(v) {
                out.insert(v.clone());
            }
        }
        E::Int(_) | E::Float(_) | E::Duration(_) | E::Str(_) | E::Bool(_) => {}
        E::List(xs) | E::Tuple(xs) | E::Call { args: xs, .. } | E::Ctor { args: xs, .. }
        | E::Spawn { args: xs, .. } => xs.iter().for_each(|x| fv_expr(x, bound, out)),
        E::MethodCall { receiver, args, .. } => {
            fv_expr(receiver, bound, out);
            args.iter().for_each(|x| fv_expr(x, bound, out));
        }
        E::Apply { func, args } => {
            fv_expr(func, bound, out);
            args.iter().for_each(|x| fv_expr(x, bound, out));
        }
        E::Unary { expr, .. } | E::Try(expr) | E::As { expr, .. } | E::Field { base: expr, .. } => {
            fv_expr(expr, bound, out)
        }
        E::Binary { lhs, rhs, .. } => {
            fv_expr(lhs, bound, out);
            fv_expr(rhs, bound, out);
        }
        E::Range { lo, hi, .. } => {
            fv_expr(lo, bound, out);
            fv_expr(hi, bound, out);
        }
        E::Index { base, index } => {
            fv_expr(base, bound, out);
            fv_expr(index, bound, out);
        }
        E::Lambda { params, body } => {
            let mut b = bound.clone();
            for p in params {
                b.insert(p.name.clone());
            }
            collect_free_vars(body, &b, out);
        }
        E::Block(b) => collect_free_vars(b, bound, out),
        E::If { cond, then_block, else_block } => {
            fv_expr(cond, bound, out);
            collect_free_vars(then_block, bound, out);
            if let Some(eb) = else_block {
                collect_free_vars(eb, bound, out);
            }
        }
        E::While { cond, body } => {
            fv_expr(cond, bound, out);
            collect_free_vars(body, bound, out);
        }
        E::WhileLet { pattern, scrutinee, body } => {
            fv_expr(scrutinee, bound, out);
            let mut b = bound.clone();
            let mut pb = Vec::new();
            pattern_bindings(pattern, &mut pb);
            b.extend(pb);
            collect_free_vars(body, &b, out);
        }
        E::For { var, iter, body } => {
            fv_expr(iter, bound, out);
            let mut b = bound.clone();
            b.insert(var.clone());
            collect_free_vars(body, &b, out);
        }
        E::Match { scrutinee, arms } => {
            fv_expr(scrutinee, bound, out);
            for arm in arms {
                let mut b = bound.clone();
                let mut pb = Vec::new();
                pattern_bindings(&arm.pattern, &mut pb);
                b.extend(pb);
                if let Some(g) = &arm.guard {
                    fv_expr(g, &b, out);
                }
                fv_expr(&arm.body, &b, out);
            }
        }
        E::RecordUpdate { base, fields } => {
            fv_expr(base, bound, out);
            fields.iter().for_each(|(_, v)| fv_expr(v, bound, out));
        }
        E::Record { fields, spread, .. } => {
            fields.iter().for_each(|(_, v)| fv_expr(v, bound, out));
            if let Some(s) = spread {
                fv_expr(s, bound, out);
            }
        }
    }
}

/// Count textual uses of `name` in an expression: `total` and, separately,
/// `looped` — uses inside a loop or closure body (relative to here), where a
/// move would be unsound (the use may re-execute). Over-counting (e.g. through a
/// shadowing rebind) is safe: it only keeps the conservative clone.
fn count_uses(e: &Expr, name: &str, in_loop: bool, total: &mut usize, looped: &mut usize) {
    use crate::ast::Expr as E;
    let go = |x: &Expr, l: bool, t: &mut usize, lp: &mut usize| count_uses(x, name, l, t, lp);
    match e {
        E::Var(v) => {
            if v == name {
                *total += 1;
                if in_loop {
                    *looped += 1;
                }
            }
        }
        E::Int(_) | E::Float(_) | E::Duration(_) | E::Str(_) | E::Bool(_) | E::Spawn { .. } => {}
        E::List(xs) | E::Tuple(xs) => xs.iter().for_each(|x| go(x, in_loop, total, looped)),
        E::Call { args, .. } | E::Ctor { args, .. } => {
            args.iter().for_each(|x| go(x, in_loop, total, looped))
        }
        E::MethodCall { receiver, args, .. } => {
            go(receiver, in_loop, total, looped);
            args.iter().for_each(|x| go(x, in_loop, total, looped));
        }
        E::Apply { func, args } => {
            go(func, in_loop, total, looped);
            args.iter().for_each(|x| go(x, in_loop, total, looped));
        }
        E::Unary { expr, .. } | E::Try(expr) | E::As { expr, .. } | E::Field { base: expr, .. } => {
            go(expr, in_loop, total, looped)
        }
        E::Binary { lhs, rhs, .. } => {
            go(lhs, in_loop, total, looped);
            go(rhs, in_loop, total, looped);
        }
        E::Range { lo, hi, .. } => {
            go(lo, in_loop, total, looped);
            go(hi, in_loop, total, looped);
        }
        E::Index { base, index } => {
            go(base, in_loop, total, looped);
            go(index, in_loop, total, looped);
        }
        // A closure body may run any number of times (or escape): treat its uses
        // as looped so captured variables are cloned, not moved.
        E::Lambda { body, .. } => count_uses_block(body, name, true, total, looped),
        E::Block(b) => count_uses_block(b, name, in_loop, total, looped),
        E::If { cond, then_block, else_block } => {
            go(cond, in_loop, total, looped);
            count_uses_block(then_block, name, in_loop, total, looped);
            if let Some(eb) = else_block {
                count_uses_block(eb, name, in_loop, total, looped);
            }
        }
        E::Match { scrutinee, arms } => {
            go(scrutinee, in_loop, total, looped);
            for a in arms {
                if let Some(g) = &a.guard {
                    go(g, in_loop, total, looped);
                }
                go(&a.body, in_loop, total, looped);
            }
        }
        E::While { cond, body } => {
            go(cond, true, total, looped);
            count_uses_block(body, name, true, total, looped);
        }
        E::For { iter, body, .. } => {
            go(iter, in_loop, total, looped);
            count_uses_block(body, name, true, total, looped);
        }
        E::WhileLet { scrutinee, body, .. } => {
            go(scrutinee, true, total, looped);
            count_uses_block(body, name, true, total, looped);
        }
        E::RecordUpdate { base, fields } => {
            go(base, in_loop, total, looped);
            fields.iter().for_each(|(_, x)| go(x, in_loop, total, looped));
        }
        E::Record { fields, spread, .. } => {
            fields.iter().for_each(|(_, x)| go(x, in_loop, total, looped));
            if let Some(s) = spread {
                go(s, in_loop, total, looped);
            }
        }
    }
}

fn count_uses_block(b: &Block, name: &str, in_loop: bool, total: &mut usize, looped: &mut usize) {
    use crate::ast::Stmt;
    for s in &b.stmts {
        match s {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetTuple { value, .. } => {
                count_uses(value, name, in_loop, total, looped)
            }
            Stmt::Return(Some(e)) | Stmt::Yield(e) | Stmt::Expr(e) => {
                count_uses(e, name, in_loop, total, looped)
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

/// Is `name` used exactly once, and not inside a loop/closure, within `body`?
/// Such a use is the binding's last dynamic use, so its value can be moved.
fn last_use_in_expr(body: &Expr, name: &str) -> bool {
    let (mut total, mut looped) = (0, 0);
    count_uses(body, name, false, &mut total, &mut looped);
    total == 1 && looped == 0
}

/// Is `name` used exactly once (loop-free) across a whole block? Used for params,
/// which are in scope for the entire body — a single loop-free use is the last.
fn last_use_in_block(b: &Block, name: &str) -> bool {
    let (mut total, mut looped) = (0, 0);
    count_uses_block(b, name, false, &mut total, &mut looped);
    total == 1 && looped == 0
}

/// Same, over the remaining statements of a block — a `let` used exactly once
/// (loop-free) afterward can be moved at that use. (`stmts` is the slice after
/// the binding, so all uses there are genuinely later.)
fn last_use_in_stmts(stmts: &[Stmt], name: &str) -> bool {
    let (mut total, mut looped) = (0, 0);
    for s in stmts {
        match s {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetTuple { value, .. } => {
                count_uses(value, name, false, &mut total, &mut looped)
            }
            Stmt::Return(Some(e)) | Stmt::Yield(e) | Stmt::Expr(e) => {
                count_uses(e, name, false, &mut total, &mut looped)
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    total == 1 && looped == 0
}

/// Backward-liveness analysis: returns the set of `Var` occurrences (keyed by
/// AST-node pointer) that are a *last use* — the binding is not read again on any
/// path afterward, and the use is not inside a loop or closure. Such a use can be
/// MOVED rather than cloned. Keying by pointer makes the result independent of
/// emission order (the native backend reorders some call arguments), and it is
/// conservative by construction: branches union their live-out, loops/closures
/// mark nothing, and shadowing only ever keeps a name live longer (never shorter)
/// — so it can only *under*-mark. Any residual over-mark is caught by the Rust
/// compiler as a use-after-move, never miscompiled.
fn compute_moveable_occurrences(body: &Block) -> HashSet<usize> {
    let mut moveable = HashSet::new();
    let mut live = HashSet::new();
    mv_block(body, &mut live, &mut moveable, false);
    moveable
}

fn mv_block(b: &Block, live: &mut HashSet<String>, moveable: &mut HashSet<usize>, in_loop: bool) {
    use crate::ast::Stmt;
    for s in b.stmts.iter().rev() {
        match s {
            Stmt::Let { name, value, .. } => {
                mv_expr(value, live, moveable, in_loop);
                // The binding is introduced here; code before the `let` is outside
                // its scope, so the name is no longer this binding's live-range.
                live.remove(name);
            }
            Stmt::LetTuple { names, value } => {
                mv_expr(value, live, moveable, in_loop);
                for n in names {
                    live.remove(n);
                }
            }
            // An assignment reads `value` and writes `name`; conservatively leave
            // `name` live (don't treat the write as ending its live-range).
            Stmt::Assign { value, .. } => mv_expr(value, live, moveable, in_loop),
            Stmt::Return(Some(e)) | Stmt::Yield(e) | Stmt::Expr(e) => {
                mv_expr(e, live, moveable, in_loop)
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn mv_expr(e: &Expr, live: &mut HashSet<String>, moveable: &mut HashSet<usize>, in_loop: bool) {
    use crate::ast::Expr as E;
    let p = |x: &Expr, l: &mut HashSet<String>, m: &mut HashSet<usize>| mv_expr(x, l, m, in_loop);
    match e {
        E::Var(name) => {
            if !in_loop && !live.contains(name) {
                moveable.insert(e as *const Expr as usize);
            }
            live.insert(name.clone());
        }
        E::Int(_) | E::Float(_) | E::Duration(_) | E::Str(_) | E::Bool(_) => {}
        // Sequenced operands are processed in REVERSE evaluation order so that the
        // textually-later use is seen first (and so marked live before the earlier).
        E::Binary { lhs, rhs, .. } => {
            p(rhs, live, moveable);
            p(lhs, live, moveable);
        }
        E::Range { lo, hi, .. } => {
            p(hi, live, moveable);
            p(lo, live, moveable);
        }
        E::Index { base, index } => {
            p(index, live, moveable);
            p(base, live, moveable);
        }
        E::Call { args, .. } | E::Ctor { args, .. } | E::List(args) | E::Tuple(args)
        | E::Spawn { args, .. } => {
            for a in args.iter().rev() {
                p(a, live, moveable);
            }
        }
        E::MethodCall { receiver, args, .. } => {
            for a in args.iter().rev() {
                p(a, live, moveable);
            }
            p(receiver, live, moveable);
        }
        E::Apply { func, args } => {
            for a in args.iter().rev() {
                p(a, live, moveable);
            }
            p(func, live, moveable);
        }
        E::Unary { expr, .. } | E::Try(expr) | E::As { expr, .. } | E::Field { base: expr, .. } => {
            p(expr, live, moveable)
        }
        E::RecordUpdate { base, fields } => {
            for (_, v) in fields.iter().rev() {
                p(v, live, moveable);
            }
            p(base, live, moveable);
        }
        E::Record { fields, spread, .. } => {
            if let Some(s) = spread {
                p(s, live, moveable);
            }
            for (_, v) in fields.iter().rev() {
                p(v, live, moveable);
            }
        }
        E::Block(b) => mv_block(b, live, moveable, in_loop),
        // Branches are mutually exclusive: each gets a clone of the live-out, and
        // the live-in is their union (plus the condition's uses).
        E::If { cond, then_block, else_block } => {
            let mut lt = live.clone();
            mv_block(then_block, &mut lt, moveable, in_loop);
            let mut le = live.clone();
            if let Some(eb) = else_block {
                mv_block(eb, &mut le, moveable, in_loop);
            }
            live.extend(lt);
            live.extend(le);
            p(cond, live, moveable);
        }
        E::Match { scrutinee, arms } => {
            let mut union = live.clone();
            for arm in arms {
                let mut al = live.clone();
                p(&arm.body, &mut al, moveable);
                if let Some(g) = &arm.guard {
                    p(g, &mut al, moveable);
                }
                let mut binds = Vec::new();
                pattern_bindings(&arm.pattern, &mut binds);
                for b in binds {
                    al.remove(&b);
                }
                union.extend(al);
            }
            *live = union;
            p(scrutinee, live, moveable);
        }
        // A loop body repeats: its uses are live across iterations, and nothing in
        // it may be marked (a moved value would be gone on the next iteration).
        E::While { cond, body } => {
            let mut bl = live.clone();
            mv_block(body, &mut bl, moveable, true);
            live.extend(bl);
            mv_expr(cond, live, moveable, true);
        }
        E::WhileLet { scrutinee, body, pattern } => {
            let mut bl = live.clone();
            mv_block(body, &mut bl, moveable, true);
            let mut binds = Vec::new();
            pattern_bindings(pattern, &mut binds);
            for b in binds {
                bl.remove(&b);
            }
            live.extend(bl);
            mv_expr(scrutinee, live, moveable, true);
        }
        E::For { var, iter, body } => {
            let mut bl = live.clone();
            mv_block(body, &mut bl, moveable, true);
            bl.remove(var);
            live.extend(bl);
            p(iter, live, moveable);
        }
        // A closure may run any number of times (or escape); its captured free
        // variables are live at its creation, and nothing inside is marked.
        E::Lambda { params, body } => {
            let bound: HashSet<String> = params.iter().map(|pm| pm.name.clone()).collect();
            let mut fvs = HashSet::new();
            collect_free_vars(body, &bound, &mut fvs);
            for v in fvs {
                live.insert(v);
            }
        }
    }
}

/// A lowercase type name is a type variable (`a`, `b`, `key`); the builtin and
/// user types are all capitalized.
fn is_type_var(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
}

/// A type variable's Rust generic-parameter name (capitalize the first letter).
fn type_var_name(name: &str) -> String {
    let mut cs = name.chars();
    match cs.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + cs.as_str(),
        None => name.to_string(),
    }
}

/// The set of functions reachable from `main` (transitively). Only these are
/// transpiled, so an imported stdlib module's *unused* functions — which may use
/// constructs outside the native subset — never reach codegen.
fn reachable_functions(m: &Module) -> HashSet<String> {
    let mut bodies: HashMap<&str, &Block> = HashMap::new();
    for item in &m.items {
        if let Item::Function(f) = item {
            bodies.insert(f.name.as_str(), &f.body);
        }
    }
    let mut reached = HashSet::new();
    let mut stack = vec!["main".to_string()];
    // Actor handler bodies and field initializers are additional entry points:
    // they run in response to messages / at spawn, not from `main`'s call graph,
    // so any user function they call must be reached (and thus emitted) too.
    for item in &m.items {
        if let Item::Actor(a) = item {
            for f in &a.fields {
                if let Some(init) = &f.init {
                    let mut calls = HashSet::new();
                    collect_calls_expr(init, &mut calls);
                    stack.extend(calls);
                }
            }
            for h in &a.handlers {
                let mut calls = HashSet::new();
                collect_calls_block(&h.body, &mut calls);
                stack.extend(calls);
            }
        }
    }
    while let Some(n) = stack.pop() {
        if !reached.insert(n.clone()) {
            continue;
        }
        if let Some(b) = bodies.get(n.as_str()) {
            let mut calls = HashSet::new();
            collect_calls_block(b, &mut calls);
            stack.extend(calls);
        }
    }
    reached
}

fn collect_calls_block(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        match s {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetTuple { value, .. }
            | Stmt::Expr(value)
            | Stmt::Return(Some(value))
            | Stmt::Yield(value) => collect_calls_expr(value, out),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn collect_calls_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Call { name, args } => {
            out.insert(name.clone());
            args.iter().for_each(|a| collect_calls_expr(a, out));
        }
        Expr::Ctor { args, .. } | Expr::List(args) | Expr::Tuple(args) | Expr::Spawn { args, .. } => {
            args.iter().for_each(|a| collect_calls_expr(a, out))
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_calls_expr(receiver, out);
            args.iter().for_each(|a| collect_calls_expr(a, out));
        }
        Expr::Apply { func, args } => {
            collect_calls_expr(func, out);
            args.iter().for_each(|a| collect_calls_expr(a, out));
        }
        Expr::Unary { expr, .. }
        | Expr::Field { base: expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. } => collect_calls_expr(expr, out),
        Expr::Binary { lhs, rhs, .. } => {
            collect_calls_expr(lhs, out);
            collect_calls_expr(rhs, out);
        }
        Expr::Range { lo, hi, .. } => {
            collect_calls_expr(lo, out);
            collect_calls_expr(hi, out);
        }
        Expr::Index { base, index } => {
            collect_calls_expr(base, out);
            collect_calls_expr(index, out);
        }
        Expr::If { cond, then_block, else_block } => {
            collect_calls_expr(cond, out);
            collect_calls_block(then_block, out);
            if let Some(b) = else_block {
                collect_calls_block(b, out);
            }
        }
        Expr::While { cond, body } => {
            collect_calls_expr(cond, out);
            collect_calls_block(body, out);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            collect_calls_expr(scrutinee, out);
            collect_calls_block(body, out);
        }
        Expr::For { iter, body, .. } => {
            collect_calls_expr(iter, out);
            collect_calls_block(body, out);
        }
        Expr::Match { scrutinee, arms } => {
            collect_calls_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_calls_expr(g, out);
                }
                collect_calls_expr(&arm.body, out);
            }
        }
        Expr::Block(b) => collect_calls_block(b, out),
        Expr::Lambda { body, .. } => collect_calls_block(body, out),
        Expr::RecordUpdate { base, fields } => {
            collect_calls_expr(base, out);
            fields.iter().for_each(|(_, v)| collect_calls_expr(v, out));
        }
        Expr::Record { fields, spread, .. } => {
            fields.iter().for_each(|(_, v)| collect_calls_expr(v, out));
            if let Some(s) = spread {
                collect_calls_expr(s, out);
            }
        }
        // A bare identifier may be a first-class function value (`map(xs, dbl)`);
        // collect it so a function referenced only as a value is still reachable.
        // Over-approximates (locals too), which is harmless — non-functions match
        // no definition.
        Expr::Var(v) => {
            out.insert(v.clone());
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_) => {}
    }
}

/// Render a generic-parameter list `<A: Clone, B: Clone>` (empty if no vars).
/// `Clone` covers value-copies (for-loop elements, call-site clones).
fn generic_params(tvs: &std::collections::BTreeSet<String>) -> String {
    generic_params_eq(tvs, false)
}

/// `with_eq` adds a `PartialEq` bound — used for *function* generics so a generic
/// `==`/`!=` on a type variable compiles (witchy has structural equality on any
/// value). Type (struct/enum) generics keep just `Clone` to avoid over-
/// constraining their other impls.
fn generic_params_eq(tvs: &std::collections::BTreeSet<String>, with_eq: bool) -> String {
    if tvs.is_empty() {
        return String::new();
    }
    // `'static` because a value of the type variable may be captured into an
    // `Rc<dyn Fn>` (which is `+ 'static`); every witchy value is owned, so this
    // always holds.
    let bound = if with_eq { "Clone + PartialEq + 'static" } else { "Clone + 'static" };
    let bounded: Vec<String> = tvs.iter().map(|v| format!("{v}: {bound}")).collect();
    format!("<{}>", bounded.join(", "))
}

/// Does this function body use `==`/`!=`? If so its generic type variables get a
/// `PartialEq` bound (a generic structural comparison needs it). Conservative:
/// missing one just leaves a `==`-using generic unsupported, never miscompiled.
fn block_has_eq(b: &Block) -> bool {
    b.stmts.iter().any(|s| match s {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetTuple { value, .. } => {
            expr_has_eq(value)
        }
        Stmt::Return(Some(e)) | Stmt::Yield(e) | Stmt::Expr(e) => expr_has_eq(e),
        _ => false,
    })
}

fn expr_has_eq(e: &Expr) -> bool {
    use crate::ast::Expr as E;
    match e {
        E::Binary { op, lhs, rhs } => {
            matches!(op, BinOp::Eq | BinOp::NotEq) || expr_has_eq(lhs) || expr_has_eq(rhs)
        }
        E::List(xs) | E::Tuple(xs) | E::Call { args: xs, .. } | E::Ctor { args: xs, .. }
        | E::Spawn { args: xs, .. } => xs.iter().any(expr_has_eq),
        E::MethodCall { receiver, args, .. } => expr_has_eq(receiver) || args.iter().any(expr_has_eq),
        E::Apply { func, args } => expr_has_eq(func) || args.iter().any(expr_has_eq),
        E::Unary { expr, .. } | E::Try(expr) | E::As { expr, .. } | E::Field { base: expr, .. } => {
            expr_has_eq(expr)
        }
        E::Range { lo, hi, .. } => expr_has_eq(lo) || expr_has_eq(hi),
        E::Index { base, index } => expr_has_eq(base) || expr_has_eq(index),
        E::Lambda { body, .. } | E::Block(body) => block_has_eq(body),
        E::If { cond, then_block, else_block } => {
            expr_has_eq(cond)
                || block_has_eq(then_block)
                || else_block.as_ref().is_some_and(block_has_eq)
        }
        E::While { cond, body } => expr_has_eq(cond) || block_has_eq(body),
        E::WhileLet { scrutinee, body, .. } => expr_has_eq(scrutinee) || block_has_eq(body),
        E::For { iter, body, .. } => expr_has_eq(iter) || block_has_eq(body),
        E::Match { scrutinee, arms } => {
            expr_has_eq(scrutinee)
                || arms
                    .iter()
                    .any(|a| a.guard.as_ref().is_some_and(expr_has_eq) || expr_has_eq(&a.body))
        }
        E::RecordUpdate { base, fields } => {
            expr_has_eq(base) || fields.iter().any(|(_, v)| expr_has_eq(v))
        }
        E::Record { fields, spread, .. } => {
            fields.iter().any(|(_, v)| expr_has_eq(v)) || spread.as_deref().is_some_and(expr_has_eq)
        }
        _ => false,
    }
}

/// Gather the (capitalized) type variables appearing in a type.
fn collect_type_vars(t: &Type, out: &mut std::collections::BTreeSet<String>) {
    match t {
        Type::Named(n, args) => {
            if is_type_var(n) {
                out.insert(type_var_name(n));
            }
            args.iter().for_each(|a| collect_type_vars(a, out));
        }
        Type::Tuple(ts) => ts.iter().for_each(|t| collect_type_vars(t, out)),
        Type::Fn(args, ret) => {
            args.iter().for_each(|a| collect_type_vars(a, out));
            collect_type_vars(ret, out);
        }
    }
}

/// Transpile a whole module's record/enum types and functions to a
/// self-contained Rust program.
/// `spawn Actor(args)` -> register a new actor instance and return its `Subject`
/// (its index in the registry). Fields with an initializer default; the others
/// take the positional spawn arguments in declaration order — exactly the
/// interpreter's field-binding order.
fn gen_spawn(actor: &str, args: &[Expr]) -> Result<String, String> {
    if IN_HANDLER.with(|f| f.get()) {
        return Err(format!(
            "native backend: `spawn` inside an actor handler is not supported (it would re-enter the registry the scheduler holds); spawn `{actor}` from `main`"
        ));
    }
    let fields = ACTOR_FIELDS.with(|m| m.borrow().get(actor).cloned());
    let Some(fields) = fields else {
        return Err(format!("native backend: cannot spawn unknown actor `{actor}`"));
    };
    let mut supplied = args.iter();
    let mut parts = Vec::new();
    for f in &fields {
        let val = match &f.init {
            Some(init) => gen_expr(init, true)?,
            None => {
                let a = supplied.next().ok_or_else(|| {
                    format!("native backend: spawn `{actor}` is missing a value for field `{}`", f.name)
                })?;
                gen_value(a)?
            }
        };
        parts.push(format!("{}: {val}", f.name));
    }
    Ok(format!("w_spawn(WActor::{actor}({actor} {{ {} }}))", parts.join(", ")))
}

/// Emit the native actor runtime for a program that declares actors, mirroring
/// the interpreter's deterministic model: a single FIFO mailbox, run-to-
/// completion handlers, and per-actor isolated state. Each actor becomes a Rust
/// struct; messages a shared `WMsg` enum; instances a `WActor` enum indexed by a
/// `Subject` (usize). `spawn` registers an instance, `send` enqueues a message,
/// and `w_run_actors` drains the queue until quiescent. A handler seeds locals
/// from the actor's fields (so the body's bare field references resolve), runs,
/// then writes its mutable (`var`) fields back — exactly as the interpreter seeds
/// an env from state and persists what changed.
fn gen_actors(m: &Module) -> Result<String, String> {
    let actors: Vec<&crate::ast::ActorDef> = m
        .items
        .iter()
        .filter_map(|it| if let Item::Actor(a) = it { Some(a) } else { None })
        .collect();
    if actors.is_empty() {
        return Ok(String::new());
    }
    // Each message's payload types, from the first handler that declares it.
    let mut msgs: Vec<(String, Vec<Type>)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for a in &actors {
        for h in &a.handlers {
            if seen.insert(h.message.clone()) {
                let tys: Vec<Type> = h.params.iter().filter_map(|p| p.ty.clone()).collect();
                if tys.len() != h.params.len() {
                    return Err(format!(
                        "native backend: actor handler `{}` needs a type on every parameter",
                        h.message
                    ));
                }
                msgs.push((h.message.clone(), tys));
            }
        }
    }
    let mut out = String::new();
    // One struct per actor.
    for a in &actors {
        let mut fields = Vec::new();
        for f in &a.fields {
            let rt = rust_ty(&f.ty).ok_or_else(|| {
                format!("native backend: actor `{}` field `{}` has an unsupported type", a.name, f.name)
            })?;
            fields.push(format!("    {}: {rt},", f.name));
        }
        out.push_str(&format!("struct {} {{\n{}\n}}\n", a.name, fields.join("\n")));
    }
    // The message enum: one variant per distinct message constructor.
    let mut variants = Vec::new();
    for (name, tys) in &msgs {
        if tys.is_empty() {
            variants.push(format!("    {name}"));
        } else {
            let ts: Vec<String> = tys
                .iter()
                .map(|t| {
                    rust_ty(t).ok_or_else(|| {
                        format!("native backend: message `{name}` has an unsupported payload type")
                    })
                })
                .collect::<Result<_, _>>()?;
            variants.push(format!("    {name}({})", ts.join(", ")));
        }
    }
    out.push_str(&format!("enum WMsg {{\n{}\n}}\n", variants.join(",\n")));
    // The actor-instance enum, indexed by a Subject (its position in the registry).
    let avariants: Vec<String> =
        actors.iter().map(|a| format!("    {}({})", a.name, a.name)).collect();
    out.push_str(&format!("enum WActor {{\n{}\n}}\n", avariants.join(",\n")));
    // Registry + mailbox, and the spawn/send/drain primitives.
    out.push_str(
        "thread_local! {\n    static WACTORS: std::cell::RefCell<Vec<WActor>> = std::cell::RefCell::new(Vec::new());\n    static WQUEUE: std::cell::RefCell<std::collections::VecDeque<(usize, WMsg)>> = std::cell::RefCell::new(std::collections::VecDeque::new());\n}\n",
    );
    out.push_str("fn w_spawn(a: WActor) -> usize { WACTORS.with(|v| { let mut v = v.borrow_mut(); let id = v.len(); v.push(a); id }) }\n");
    out.push_str("fn w_send(id: usize, m: WMsg) { WQUEUE.with(|q| q.borrow_mut().push_back((id, m))); }\n");
    // The drain loop: pop a message, dispatch to the addressed actor's handler.
    // Messages process in FIFO order, each handler running to completion.
    let mut dispatch = String::new();
    for a in &actors {
        let mut arms = String::new();
        for h in &a.handlers {
            let binds: Vec<String> = (0..h.params.len()).map(|i| format!("p{i}")).collect();
            let pat = if binds.is_empty() {
                format!("WMsg::{}", h.message)
            } else {
                format!("WMsg::{}({})", h.message, binds.join(", "))
            };
            arms.push_str(&format!(
                "                {pat} => s.handle_{}({}),\n",
                h.message,
                binds.join(", ")
            ));
        }
        dispatch.push_str(&format!(
            "            Some(WActor::{}(s)) => match msg {{\n{arms}                #[allow(unreachable_patterns)] _ => {{}}\n            }},\n",
            a.name
        ));
    }
    out.push_str(&format!(
        "fn w_run_actors() {{\n    loop {{\n        let next = WQUEUE.with(|q| q.borrow_mut().pop_front());\n        let Some((id, msg)) = next else {{ break }};\n        WACTORS.with(|v| {{\n            let mut __actors = v.borrow_mut();\n            match __actors.get_mut(id) {{\n{dispatch}            None => {{}}\n            }}\n        }});\n    }}\n}}\n"
    ));
    // Handler methods: seed locals from state, run the body, write var fields back.
    IN_HANDLER.with(|f| f.set(true));
    for a in &actors {
        for h in &a.handlers {
            let param_names: HashSet<String> = h.params.iter().map(|p| p.name.clone()).collect();
            // Per-handler local environment for the body emitter: params + fields
            // are locals (not function calls); clone-on-reuse for value semantics.
            CUR_PARAMS.with(|s| {
                let mut s = s.borrow_mut();
                s.clear();
                s.extend(param_names.iter().cloned());
                s.extend(a.fields.iter().map(|f| f.name.clone()));
            });
            MOVEABLE.with(|mv| mv.borrow_mut().clear());
            NOCLONE.with(|nc| nc.borrow_mut().clear());
            let mut params = Vec::new();
            for p in &h.params {
                let ty = p.ty.as_ref().ok_or_else(|| {
                    format!("native backend: handler `{}` parameter `{}` needs a type", h.message, p.name)
                })?;
                let rt = rust_ty(ty).ok_or_else(|| {
                    format!("native backend: handler `{}` parameter `{}` has an unsupported type", h.message, p.name)
                })?;
                params.push(format!("mut {}: {rt}", local_ident(&p.name)));
            }
            // Seed each field not shadowed by a parameter (params take precedence,
            // matching the interpreter); mutable (`var`) fields are written back.
            let mut seeds = String::new();
            let mut writebacks = String::new();
            for f in &a.fields {
                if param_names.contains(&f.name) {
                    continue;
                }
                let mu = if f.mutable { "mut " } else { "" };
                seeds.push_str(&format!(
                    "        let {mu}{} = self.{}.clone();\n",
                    local_ident(&f.name),
                    f.name
                ));
                if f.mutable {
                    writebacks.push_str(&format!(
                        "        self.{} = {};\n",
                        f.name,
                        local_ident(&f.name)
                    ));
                }
            }
            let body = gen_block(&h.body, false)?;
            let sep = if params.is_empty() { "" } else { ", " };
            out.push_str(&format!(
                "impl {} {{\n    fn handle_{}(&mut self{sep}{}) {{\n{seeds}        {body};\n{writebacks}    }}\n}}\n",
                a.name,
                h.message,
                params.join(", ")
            ));
        }
    }
    IN_HANDLER.with(|f| f.set(false));
    Ok(out)
}

pub fn transpile_module(m: &Module) -> Result<String, String> {
    // Inline top-level constants at their use sites (idempotent after the
    // linker's own pass; covers unlinked single-module input).
    let inlined = crate::consts::inline(m.clone());
    let m = &inlined;
    RECORD_FIELDS.with(|r| r.borrow_mut().clear());
    VARIANT_TO_ENUM.with(|r| r.borrow_mut().clear());
    ENUM_NAMES.with(|r| r.borrow_mut().clear());
    VARIANT_BOXED.with(|r| r.borrow_mut().clear());
    USES_DIR.with(|f| f.set(false));
    USES_NET.with(|f| f.set(false));
    USES_SHOW.with(|f| f.set(false));
    FN_PARAM_CLONE.with(|map| map.borrow_mut().clear());
    FN_PARAM_BORROW.with(|map| map.borrow_mut().clear());
    FN_INOUT.with(|map| map.borrow_mut().clear());
    MOVEABLE.with(|s| s.borrow_mut().clear());
    FN_NAMES.with(|s| {
        let mut s = s.borrow_mut();
        s.clear();
        for item in &m.items {
            if let Item::Function(f) = item {
                s.insert(f.name.clone());
            }
        }
    });
    let showable = compute_showable_types(m);
    SHOWABLE_TYPES.with(|s| *s.borrow_mut() = showable);
    // Record which params are by-value collections (so calls clone them) and
    // which functions take `inout` params (so calls write the result back).
    for item in &m.items {
        if let Item::Function(f) = item {
            // A `sink`/`own` parameter takes ownership, so its argument is MOVED,
            // never cloned, at the call site. (The type checker has already marked
            // the argument consumed, so a later use is rejected on every backend —
            // the native move can't diverge from the interpreter.)
            let clones = f
                .params
                .iter()
                .map(|p| {
                    p.convention != crate::ast::Convention::Sink && needs_clone_at_call(&p.ty)
                })
                .collect();
            FN_PARAM_CLONE.with(|map| map.borrow_mut().insert(fn_rust_name(f), clones));
            // Which positions are `&T` borrows — a call site passes `&arg` there.
            let borrows: Vec<bool> = f.params.iter().map(is_borrow_param).collect();
            if borrows.iter().any(|b| *b) {
                FN_PARAM_BORROW.with(|map| map.borrow_mut().insert(fn_rust_name(f), borrows));
            }
            let inout: Vec<usize> = f
                .params
                .iter()
                .enumerate()
                .filter(|(_, p)| p.convention == crate::ast::Convention::Inout)
                .map(|(i, _)| i)
                .collect();
            if !inout.is_empty() {
                FN_INOUT.with(|map| map.borrow_mut().insert(f.name.clone(), inout));
            }
        }
    }
    USES_DICT.with(|f| f.set(false));
    // Actor program? Record the message names and each actor's fields up front, so
    // `send`/`spawn` and `main`'s mailbox drain resolve while emitting bodies.
    HAS_ACTORS.with(|f| f.set(false));
    MSG_NAMES.with(|s| s.borrow_mut().clear());
    ACTOR_FIELDS.with(|s| s.borrow_mut().clear());
    for item in &m.items {
        if let Item::Actor(a) = item {
            HAS_ACTORS.with(|f| f.set(true));
            ACTOR_FIELDS.with(|s| s.borrow_mut().insert(a.name.clone(), a.fields.clone()));
            for h in &a.handlers {
                MSG_NAMES.with(|s| s.borrow_mut().insert(h.message.clone()));
            }
        }
    }
    let mut out = String::new();
    // Pre-register every user type's name, so mutually-recursive types resolve
    // when their fields are computed (e.g. std/iter's `Step`/`Iter`, which
    // reference each other — the cycle is finite because it passes through an
    // `Rc<dyn Fn>`).
    register_type_names(m);
    // First pass: emit a Rust struct/enum for each supported user type and
    // register it, so later constructors/patterns/params resolve.
    for item in &m.items {
        if let Item::Type(td) = item {
            if let Some(s) = gen_record(td) {
                out.push_str(&s);
            } else if let Some(s) = gen_enum(td) {
                out.push_str(&s);
            }
        }
    }
    // Function signatures (now that user types are registered) — for coercing a
    // fn-reference value to `Rc<dyn Fn>`. Only fully-concrete signatures qualify.
    FN_SIGS.with(|m2| {
        let mut m2 = m2.borrow_mut();
        m2.clear();
        for item in &m.items {
            if let Item::Function(f) = item {
                m2.insert(f.name.clone(), concrete_fn_sig(f));
            }
        }
    });
    let reached = reachable_functions(m);
    let mut saw_main = false;
    for item in &m.items {
        match item {
            Item::Function(f) => {
                // Only transpile functions reachable from main — so an imported
                // module's unused functions (possibly outside the subset) don't.
                if !reached.contains(&f.name) {
                    continue;
                }
                if f.name == "main" {
                    saw_main = true;
                }
                out.push_str(&gen_fn(f)?);
                out.push('\n');
            }
            // Records are handled above; other type/trait/alias/impl decls carry
            // no runtime code the subset references. Actors are emitted separately
            // by `gen_actors` (structs + the mailbox runtime).
            Item::Type(_) | Item::Trait(_) | Item::TypeAlias { .. } | Item::Impl(_) | Item::Actor(_) => {}
            // Top-level constants were inlined at every use site (the same
            // `consts::inline` pass the linker applies), so the items carry no
            // remaining runtime code.
            Item::Const { .. } => {}
        }
    }
    if !saw_main {
        return Err("native backend: program has no `main`".into());
    }
    // Emit the actor runtime (structs + mailbox) after the functions are walked.
    // This can itself set USES_DIR/USES_SHOW (handler bodies, capability fields),
    // so it must run before the helper-emission checks below.
    let actor_code = gen_actors(m)?;
    let mut prog = String::from("// generated by `witchy native` — do not edit\n");
    prog.push_str("#![allow(unused_parens, unused_mut, unused_variables, unused_assignments, dead_code, nonstandard_style)]\n");
    // The Dict / Dir runtime helpers are emitted only when actually used.
    if USES_DICT.with(|f| f.get()) {
        prog.push_str(DICT_HELPER);
    }
    if USES_DIR.with(|f| f.get()) {
        prog.push_str(DIR_HELPER);
    }
    if USES_NET.with(|f| f.get()) {
        prog.push_str(NET_HELPER);
    }
    if USES_SHOW.with(|f| f.get()) {
        prog.push_str(SHOW_HELPER);
    }
    prog.push_str(&actor_code);
    prog.push_str(&out);
    Ok(prog)
}

/// Register every user type's NAME (records in RECORD_FIELDS, enums in
/// ENUM_NAMES/VARIANT_TO_ENUM) before any field types are computed — mirroring
/// the record-vs-enum decision in `gen_record`/`gen_enum` — so a field that
/// refers to another (or a mutually-recursive) type resolves. The emit pass then
/// fills in the rest (boxing, FN_FIELDS, the Rust definition).
fn register_type_names(m: &Module) {
    for item in &m.items {
        let Item::Type(td) = item else { continue };
        // A record: exactly one variant with named fields.
        if let [v] = td.variants.as_slice() {
            if v.field_names.len() == v.fields.len() && !v.field_names.is_empty() {
                RECORD_FIELDS.with(|r| {
                    r.borrow_mut().insert(v.name.clone(), v.field_names.clone());
                });
                continue;
            }
        }
        // Otherwise an enum (positional variants), unless it's a built-in or
        // struct-style — matching `gen_enum`'s guards.
        if td.name == "Option" || td.name == "Result" || td.variants.is_empty() {
            continue;
        }
        if td.variants.iter().any(|v| !v.field_names.is_empty()) {
            continue;
        }
        ENUM_NAMES.with(|s| s.borrow_mut().insert(td.name.clone()));
        for v in &td.variants {
            VARIANT_TO_ENUM.with(|m2| m2.borrow_mut().insert(v.name.clone(), td.name.clone()));
        }
    }
}

/// Emit a Rust struct for a single-variant, named-field record whose fields are
/// all supported, and register its fields. Returns `None` (skips) otherwise — a
/// multi-variant enum, a positional variant, or an unsupported field type — so a
/// later use of it surfaces a clear error instead.
fn gen_record(td: &crate::ast::TypeDef) -> Option<String> {
    let [v] = td.variants.as_slice() else { return None };
    if v.field_names.len() != v.fields.len() || v.field_names.is_empty() {
        return None;
    }
    // Type variables in the fields become the struct's generic parameters.
    let mut tvs = std::collections::BTreeSet::new();
    v.fields.iter().for_each(|f| collect_type_vars(f, &mut tvs));
    let mut fields = Vec::new();
    let mut all_copy = tvs.is_empty();
    for (name, ty) in v.field_names.iter().zip(&v.fields) {
        let rt = rust_ty(ty)?;
        all_copy &= matches!(ty, Type::Named(n, _) if n == "Int" || n == "Float" || n == "Bool" || n == "Duration");
        fields.push(format!("    {name}: {rt},"));
    }
    RECORD_FIELDS.with(|r| {
        r.borrow_mut().insert(v.name.clone(), v.field_names.clone());
    });
    // `PartialEq` when every field supports it (the showability set excludes
    // closures/dicts) — so the record works with a generic `==`.
    let eq = if is_showable(&v.name) { ", PartialEq" } else { "" };
    let derive = if all_copy {
        format!("#[derive(Clone, Copy{eq})]")
    } else {
        format!("#[derive(Clone{eq})]")
    };
    let generics = generic_params(&tvs);
    let mut out = format!("{derive}\nstruct {}{generics} {{\n{}\n}}\n", v.name, fields.join("\n"));
    // A `WShow` impl renders the record like the interpreter: `Point(1, 2)`
    // (positional), but only when every field is showable.
    if is_showable(&v.name) {
        USES_SHOW.with(|f| f.set(true));
        let (ig, tr) = wshow_header(&v.name, &tvs);
        // `Name(a, b)` written field-by-field into the formatter (no intermediate
        // Strings) — same bytes as the interpreter's positional Display.
        let mut body = format!("write!(__f, \"{}(\")?; ", v.name);
        for (i, fname) in v.field_names.iter().enumerate() {
            if i > 0 {
                body.push_str("write!(__f, \", \")?; ");
            }
            body.push_str(&format!("self.{fname}.w_fmt(__f)?; "));
        }
        body.push_str("write!(__f, \")\")");
        out.push_str(&format!(
            "impl{ig} WShow for {tr} {{ fn w_fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{ {body} }} }}\n"
        ));
    }
    Some(out)
}

/// Emit a Rust enum for a type with positional variants whose fields are all
/// supported concrete types (so generic types like Option/Result, whose fields
/// are type variables, are skipped — `rust_ty` returns `None`). A single-variant
/// positional type (a newtype like `type Response: Response(...)`) is also
/// handled here, as a one-variant enum (gen_record takes the named-field case).
/// Returns `None` (skips) for struct-style variants or an unsupported field type,
/// so a later use surfaces a clear error.
fn gen_enum(td: &crate::ast::TypeDef) -> Option<String> {
    if td.variants.is_empty() {
        return None;
    }
    // Option/Result map to Rust's built-ins (their constructors/patterns are
    // special-cased), so don't emit a clashing user enum.
    if td.name == "Option" || td.name == "Result" {
        return None;
    }
    // Struct-style variants aren't supported; bail before registering anything.
    if td.variants.iter().any(|v| !v.field_names.is_empty()) {
        return None;
    }
    // Register the name (and variant->enum map) up front, so a field referring
    // back to this enum — directly, or through a collection like `List(Json)` —
    // resolves while the field types are being computed below.
    ENUM_NAMES.with(|s| s.borrow_mut().insert(td.name.clone()));
    for v in &td.variants {
        VARIANT_TO_ENUM.with(|m| m.borrow_mut().insert(v.name.clone(), td.name.clone()));
    }
    // Type variables in the variants' fields become the enum's generic params.
    let mut tvs = std::collections::BTreeSet::new();
    for v in &td.variants {
        v.fields.iter().for_each(|f| collect_type_vars(f, &mut tvs));
    }
    let mut variants = Vec::new();
    let mut all_copy = tvs.is_empty();
    // `Foo<A, B>` (bare type-var names, no bounds) — for a recursive `Box<Self>`.
    let self_ref = if tvs.is_empty() {
        td.name.clone()
    } else {
        format!("{}<{}>", td.name, tvs.iter().cloned().collect::<Vec<_>>().join(", "))
    };
    let mut boxed_by_variant: Vec<(String, Vec<bool>)> = Vec::new();
    for v in &td.variants {
        let mut tys = Vec::new();
        let mut boxed = Vec::new();
        for ty in &v.fields {
            // A directly-recursive field is `Box`ed so the enum has a finite size.
            if matches!(ty, Type::Named(n, _) if n == &td.name) {
                tys.push(format!("Box<{self_ref}>"));
                boxed.push(true);
                all_copy = false;
                continue;
            }
            tys.push(rust_ty(ty)?);
            boxed.push(false);
            all_copy &= matches!(ty, Type::Named(n, _) if n == "Int" || n == "Float" || n == "Bool" || n == "Duration");
        }
        if tys.is_empty() {
            variants.push(format!("    {}", v.name));
        } else {
            variants.push(format!("    {}({})", v.name, tys.join(", ")));
        }
        boxed_by_variant.push((v.name.clone(), boxed));
    }
    for (vn, boxed) in boxed_by_variant {
        if boxed.iter().any(|b| *b) {
            VARIANT_BOXED.with(|m| m.borrow_mut().insert(vn, boxed));
        }
    }
    let eq = if is_showable(&td.name) { ", PartialEq" } else { "" };
    let derive = if all_copy {
        format!("#[derive(Clone, Copy{eq})]")
    } else {
        format!("#[derive(Clone{eq})]")
    };
    let generics = generic_params(&tvs);
    let mut out = format!("{derive}\nenum {}{generics} {{\n{}\n}}\n", td.name, variants.join(",\n"));
    // A `WShow` impl renders each variant like the interpreter: `Leaf` /
    // `Node(l, r)`, but only when every field is showable.
    if is_showable(&td.name) {
        USES_SHOW.with(|f| f.set(true));
        let (ig, tr) = wshow_header(&td.name, &tvs);
        let arms: Vec<String> = td
            .variants
            .iter()
            .map(|v| {
                if v.fields.is_empty() {
                    format!("{}::{} => write!(__f, \"{}\")", td.name, v.name, v.name)
                } else {
                    let binds: Vec<String> = (0..v.fields.len()).map(|i| format!("f{i}")).collect();
                    let mut body = format!("write!(__f, \"{}(\")?; ", v.name);
                    for (i, b) in binds.iter().enumerate() {
                        if i > 0 {
                            body.push_str("write!(__f, \", \")?; ");
                        }
                        body.push_str(&format!("{b}.w_fmt(__f)?; "));
                    }
                    body.push_str("write!(__f, \")\")");
                    format!("{}::{}({}) => {{ {body} }}", td.name, v.name, binds.join(", "))
                }
            })
            .collect();
        out.push_str(&format!(
            "impl{ig} WShow for {tr} {{ fn w_fmt(&self, __f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{ match self {{ {} }} }} }}\n",
            arms.join(", ")
        ));
    }
    Some(out)
}

fn gen_fn(f: &Function) -> Result<String, String> {
    if f.is_gen {
        return Err(format!("native backend: generator `{}` is not supported", f.name));
    }
    if !f.bounds.is_empty() {
        return Err(format!(
            "native backend: generic function `{}` (`where` bounds) is not supported",
            f.name
        ));
    }
    let is_main = f.name == "main";
    // A function with `inout` params returns its inout values (Hylo write-back,
    // mirrored from the wasm backend). Combining that with an ordinary return is
    // not modeled yet — erroring keeps native from silently diverging.
    let inout: Vec<&Param> = f
        .params
        .iter()
        .filter(|p| p.convention == crate::ast::Convention::Inout)
        .collect();
    if !inout.is_empty() && f.ret.is_some() {
        return Err(format!(
            "native backend: function `{}` with both `inout` params and a return value is not supported yet",
            f.name
        ));
    }
    let mut params = Vec::new();
    let mut cap_lets = String::new();
    for p in &f.params {
        let ty = p
            .ty
            .as_ref()
            .ok_or_else(|| format!("native backend: parameter `{}` needs a type", p.name))?;
        // A capability granted to main (Console, Dir) is bound as a local from
        // its grant; in any other function it's an ordinary value parameter that
        // can be passed along.
        if is_main {
            if let Some(grant) = capability_grant(ty) {
                cap_lets.push_str(&format!("    let {} = {grant};\n", p.name));
                continue;
            }
        }
        match rust_ty(ty) {
            // An explicit `let` borrow of a heap type is passed by reference
            // (`&T`) — no clone at the call site. Otherwise `mut` lets the body
            // reassign the owned value parameter.
            Some(rt) if is_borrow_param(p) => {
                params.push(format!("{}: &{rt}", local_ident(&p.name)))
            }
            Some(rt) => params.push(format!("mut {}: {rt}", local_ident(&p.name))),
            // An unsupported capability param (Dir/Net/...) on main is dropped;
            // the program errors only if it actually calls that host function.
            None if is_main => {}
            None => {
                return Err(format!(
                    "native backend: unsupported type for parameter `{}`",
                    p.name
                ))
            }
        }
    }
    let name = fn_rust_name(f);
    let ret = match &f.ret {
        Some(t) => {
            let rt = rust_ty(t)
                .ok_or_else(|| format!("native backend: unsupported return type of `{}`", f.name))?;
            format!(" -> {rt}")
        }
        None => String::new(),
    };
    // Type variables in the signature become Rust generic parameters. (Bound
    // `Clone` so for-loop element copies and call-site collection clones work;
    // a fn-type param already lowered to `impl Fn(..)` adds its own hidden one.)
    let mut tvs = std::collections::BTreeSet::new();
    for p in &f.params {
        if let Some(t) = &p.ty {
            collect_type_vars(t, &mut tvs);
        }
    }
    if let Some(t) = &f.ret {
        collect_type_vars(t, &mut tvs);
    }
    // Function generics get a `PartialEq` bound only when the body uses `==`/`!=`
    // (so a generic structural comparison compiles), to avoid over-constraining
    // type variables instantiated with non-equatable types (e.g. closures).
    let generics = generic_params_eq(&tvs, block_has_eq(&f.body));
    CUR_PARAMS.with(|s| {
        *s.borrow_mut() = f.params.iter().map(|p| p.name.clone()).collect();
    });
    // The `&T` borrow parameters of this function — used when forwarding one to
    // another borrow (pass the reference straight through) or iterating it.
    CUR_BORROW.with(|b| {
        *b.borrow_mut() =
            f.params.iter().filter(|p| is_borrow_param(p)).map(|p| p.name.clone()).collect();
    });
    // Per-function last-use state. A param used exactly once (loop-free) can be
    // moved at that use; a closure param can't be cloned, so it's always moved.
    MOVEABLE.with(|m| m.borrow_mut().clear());
    NOCLONE.with(|m| m.borrow_mut().clear());
    for p in &f.params {
        if matches!(&p.ty, Some(Type::Fn(..))) {
            NOCLONE.with(|m| {
                m.borrow_mut().insert(p.name.clone());
            });
        } else if last_use_in_block(&f.body, &p.name) {
            MOVEABLE.with(|m| {
                m.borrow_mut().insert(p.name.clone());
            });
        }
    }
    // Per-occurrence last-use set (liveness): lets a binding used more than once
    // still move at its final read. Computed on `&f.body`, the same AST emission
    // walks, so the pointer keys line up regardless of emission's arg reordering.
    MOVEABLE_OCC.with(|m| *m.borrow_mut() = compute_moveable_occurrences(&f.body));
    let inner = gen_block(&f.body, f.ret.is_some())?;
    if is_main {
        // Rust's `main` returns (); a witchy `Int` return is the process exit
        // code. Capabilities granted to main are bound as locals first.
        // An actor program drains its mailbox after `main`'s body runs the initial
        // spawns/sends — the deterministic FIFO scheduler, run to quiescence.
        let has_actors = HAS_ACTORS.with(|f| f.get());
        let body = if f.ret.is_some() {
            if has_actors {
                return Err("native backend: an actor program's `main` must return unit (exit-code main with actors is not modeled)".into());
            }
            format!("{{\n{cap_lets}    std::process::exit(({inner}) as i32);\n}}")
        } else if has_actors {
            format!("{{\n{cap_lets}{inner}\n    w_run_actors();\n}}")
        } else {
            format!("{{\n{cap_lets}{inner}\n}}")
        };
        return Ok(format!("fn main() {body}\n"));
    }
    if !inout.is_empty() {
        // Return the inout params' final values (this fn has no ordinary return —
        // enforced above). A statement-position call writes them back to its
        // arguments; see gen_stmt.
        let tys: Vec<String> = inout
            .iter()
            .map(|p| {
                rust_ty(p.ty.as_ref().unwrap())
                    .ok_or_else(|| format!("native backend: unsupported `inout` type for `{}`", p.name))
            })
            .collect::<Result<_, _>>()?;
        let names: Vec<String> = inout.iter().map(|p| p.name.clone()).collect();
        let (rty, rexpr) = if tys.len() == 1 {
            (tys[0].clone(), names[0].clone())
        } else {
            (format!("({})", tys.join(", ")), format!("({})", names.join(", ")))
        };
        return Ok(format!(
            "fn {name}{generics}({}) -> {rty} {{\n{inner};\n    {rexpr}\n}}\n",
            params.join(", ")
        ));
    }
    Ok(format!("fn {name}{generics}({}){ret} {inner}\n", params.join(", ")))
}

/// Emit a block. In value position (`value`), the final `Expr` statement is the
/// block's value (no trailing `;`); otherwise every statement is terminated.
fn gen_block(b: &Block, value: bool) -> Result<String, String> {
    let mut out = String::from("{\n");
    let n = b.stmts.len();
    let mut added: Vec<(String, bool)> = Vec::new();
    for (i, s) in b.stmts.iter().enumerate() {
        // A `let` used exactly once (loop-free) in the rest of this block can be
        // moved at that use instead of cloned — last-use elision (see MOVEABLE).
        if let Stmt::Let { name, .. } = s {
            if last_use_in_stmts(&b.stmts[i + 1..], name) {
                let was = MOVEABLE.with(|m| m.borrow().contains(name));
                added.push((name.clone(), was));
                MOVEABLE.with(|m| {
                    m.borrow_mut().insert(name.clone());
                });
            }
        }
        let tail = value && i + 1 == n;
        out.push_str("    ");
        out.push_str(&gen_stmt(s, tail)?);
        out.push('\n');
    }
    // Restore MOVEABLE to its prior state for the names this block introduced.
    for (name, was) in added.into_iter().rev() {
        MOVEABLE.with(|m| {
            if was {
                m.borrow_mut().insert(name);
            } else {
                m.borrow_mut().remove(&name);
            }
        });
    }
    out.push('}');
    Ok(out)
}

fn gen_stmt(s: &Stmt, tail: bool) -> Result<String, String> {
    Ok(match s {
        Stmt::Let { name, mutable, value } => {
            let m = if *mutable { "mut " } else { "" };
            format!("let {m}{name} = {};", gen_value(value)?)
        }
        Stmt::Assign { name, value } => format!("{name} = {};", gen_value(value)?),
        Stmt::Return(Some(e)) => format!("return {};", gen_value(e)?),
        Stmt::Return(None) => "return;".into(),
        Stmt::Break => "break;".into(),
        Stmt::Continue => "continue;".into(),
        // A call to an `inout` function in statement position: write the
        // returned value(s) back into the (variable) arguments at the inout
        // positions. The call's witchy value is unit, so it's only valid here.
        Stmt::Expr(Expr::Call { name, args })
            if FN_INOUT.with(|m| m.borrow().contains_key(name)) =>
        {
            let pos = FN_INOUT.with(|m| m.borrow().get(name).cloned()).unwrap();
            let mut lhs = Vec::new();
            for &i in &pos {
                match args.get(i) {
                    Some(Expr::Var(v)) => lhs.push(v.clone()),
                    _ => {
                        return Err(format!(
                            "native backend: `inout` argument {} to `{name}` must be a variable",
                            i + 1
                        ))
                    }
                }
            }
            let call = gen_call(name, args)?;
            let target = if lhs.len() == 1 {
                lhs[0].clone()
            } else {
                format!("({})", lhs.join(", "))
            };
            format!("{target} = {call};")
        }
        Stmt::Expr(e) if tail => gen_value(e)?,
        Stmt::Expr(e) => format!("{};", gen_expr(e, false)?),
        Stmt::LetTuple { names, value } => {
            format!("let ({}) = {};", names.join(", "), gen_value(value)?)
        }
        Stmt::Yield(_) => return Err("native backend: `yield` is not supported".into()),
    })
}

/// Collect a left/right-nested `<>` concatenation chain into its flat operand
/// list, in left-to-right order. A non-concat expression is a single operand.
fn flatten_concat<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    if let Expr::Binary { op: BinOp::Concat, lhs, rhs } = e {
        flatten_concat(lhs, out);
        flatten_concat(rhs, out);
    } else {
        out.push(e);
    }
}

/// Emit a `<>` concatenation as a SINGLE `format!`, instead of one nested
/// `format!("{}{}", ..)` per `<>`. String literals are baked into the format
/// string (no placeholder, no allocation); the other operands — always
/// `String`-typed in witchy (a `String` value or a `to_string(..)` result) —
/// become `{}` placeholders formatted by value. This matters because f-strings
/// desugar to a `<>` chain (often with a trailing `<> ""`), so the naive
/// lowering of `f"word{n}"` was five allocations of nested `format!`s and an
/// empty-string concat; this collapses it to one `format!` call.
fn gen_concat(e: &Expr) -> Result<String, String> {
    let mut parts = Vec::new();
    flatten_concat(e, &mut parts);
    let mut fmt = String::new();
    let mut args: Vec<String> = Vec::new();
    for p in parts {
        match p {
            // A string literal: inline its text, escaping `{`/`}` so it isn't read
            // as a placeholder (Rust-string escaping of quotes/etc. is handled by
            // emitting the whole format string via `{:?}` below).
            Expr::Str(s) => {
                for c in s.chars() {
                    match c {
                        '{' => fmt.push_str("{{"),
                        '}' => fmt.push_str("}}"),
                        _ => fmt.push(c),
                    }
                }
            }
            // A show-call operand (`to_string(x)` — what f-strings desugar `{x}`
            // to — or `int_to_string(x)`): format the value directly through the
            // `WDisplay` wrapper instead of `w_show()`/`.to_string()`, so it writes
            // into the result buffer rather than allocating an intermediate String.
            Expr::Call { name, args: cargs }
                if cargs.len() == 1 && (name == "to_string" || name == "int_to_string") =>
            {
                USES_SHOW.with(|f| f.set(true));
                fmt.push_str("{}");
                args.push(format!("WDisplay(&({}))", gen_expr(&cargs[0], true)?));
            }
            other => {
                fmt.push_str("{}");
                args.push(gen_expr(other, true)?);
            }
        }
    }
    if args.is_empty() {
        Ok(format!("{fmt:?}.to_string()"))
    } else {
        Ok(format!("format!({fmt:?}, {})", args.join(", ")))
    }
}

fn gen_expr(e: &Expr, value: bool) -> Result<String, String> {
    Ok(match e {
        // Pin integer/float literals so locals infer i64/f64 (Rust would default
        // an integer literal to i32 and break arithmetic with i64 params).
        Expr::Int(n) => format!("{n}i64"),
        Expr::Duration(n) => format!("{n}i64"),
        Expr::Float(x) => format!("{x:?}f64"),
        Expr::Bool(b) => b.to_string(),
        Expr::Str(s) => format!("{s:?}.to_string()"),
        // A bare identifier that names a top-level function is a first-class
        // function value -> its Rust `w_*` item (a fn item, which is `impl Fn`).
        Expr::Var(v) if is_fn_ref(v) => fn_ref_value(v),
        Expr::Var(v) => local_ident(v),
        Expr::Unary { op, expr } => {
            let inner = gen_expr(expr, true)?;
            match op {
                UnOp::Neg => format!("(-({inner}))"),
                UnOp::Not => format!("(!({inner}))"),
                UnOp::BitNot => format!("(!({inner}))"),
                // `move x` emits the operand as-is. For a variable that's the bare
                // name (a Rust move, no `.clone()`); since a `move` expression is
                // not a bare `Var`, the value-position/arg cloners skip it, so the
                // value is genuinely moved out of the caller's binding.
                UnOp::Move => inner,
            }
        }
        Expr::Binary { op: BinOp::Concat, .. } => gen_concat(e)?,
        Expr::Binary { op, lhs, rhs } => {
            let l = gen_expr(lhs, true)?;
            let r = gen_expr(rhs, true)?;
            format!("({l} {} {r})", bin_op(*op))
        }
        Expr::If { cond, then_block, else_block } => {
            let c = gen_expr(cond, true)?;
            let t = gen_block(then_block, value)?;
            match else_block {
                Some(eb) => format!("if {c} {t} else {}", gen_block(eb, value)?),
                None => format!("if {c} {t}"),
            }
        }
        Expr::While { cond, body } => {
            format!("while {} {}", gen_expr(cond, true)?, gen_block(body, false)?)
        }
        // `while let PAT = scrut: body` — loop while the scrutinee keeps matching.
        // Lowered to `loop { match scrut { PAT => body, _ => break } }`; the
        // scrutinee is re-evaluated each iteration.
        Expr::WhileLet { pattern, scrutinee, body } => {
            let derefs = boxed_binding_derefs(pattern);
            format!(
                "loop {{ match {} {{ {} => {{ {derefs}{} }} _ => break, }} }}",
                gen_expr(scrutinee, true)?,
                gen_pattern(pattern)?,
                gen_block(body, false)?
            )
        }
        Expr::For { var, iter, body } => match iter.as_ref() {
            Expr::Range { lo, hi, inclusive } => {
                let op = if *inclusive { "..=" } else { ".." };
                format!(
                    "for {var} in {}{op}{} {}",
                    gen_expr(lo, true)?,
                    gen_expr(hi, true)?,
                    gen_block(body, false)?
                )
            }
            // Iterate a list by shared borrow (so the list survives the loop) and
            // bind each element as a value copy, matching value semantics. A
            // borrow parameter is already `&T`, so iterate it directly (adding
            // another `&` would make `&&Vec`, which isn't iterable).
            _ => {
                let it = gen_expr(iter, true)?;
                let iter_ref = match iter.as_ref() {
                    Expr::Var(v) if is_cur_borrow(v) => it,
                    _ => format!("&({it})"),
                };
                format!(
                    "for {var}__item in {iter_ref} {{ let {var} = ({var}__item).clone(); {} }}",
                    gen_block(body, false)?
                )
            }
        },
        Expr::Block(b) => gen_block(b, value)?,
        Expr::Call { name, args } => {
            // An `inout` call's write-back is handled only in statement position
            // (gen_stmt). Used as a value it would yield the inout result instead
            // of witchy's unit — reject rather than silently diverge.
            if FN_INOUT.with(|m| m.borrow().contains_key(name)) {
                return Err(format!(
                    "native backend: a call to `inout` function `{name}` must be a statement"
                ));
            }
            gen_call(name, args)?
        }
        // `match` maps to a Rust match; non-exhaustive matches (no catch-all) are
        // rejected by rustc, which is safe (the program just stays wasm-only).
        Expr::Match { scrutinee, arms }
            if arms.iter().any(|a| matches!(a.pattern, crate::ast::Pattern::List { .. })) =>
        {
            // A list match lowers to an if-else chain on the length (see helper).
            return gen_list_match(gen_value(scrutinee)?, arms, value);
        }
        Expr::Match { scrutinee, arms }
            if arms.iter().any(|a| matches!(a.pattern, crate::ast::Pattern::Str(_))) =>
        {
            // A string match lowers to an if-else chain comparing literals.
            return gen_str_match(gen_value(scrutinee)?, arms, value);
        }
        Expr::Match { scrutinee, arms } => {
            // The scrutinee goes through `gen_value`: a variable reused in an arm
            // body is cloned (so destructuring it — moving boxed fields out —
            // leaves the original usable), while a last-use scrutinee is moved.
            let mut out = format!("match {} {{\n", gen_value(scrutinee)?);
            for arm in arms {
                let guard = match &arm.guard {
                    Some(g) => format!(" if {}", gen_expr(g, true)?),
                    None => String::new(),
                };
                // A `Box`ed (recursive) field binding is moved out of its box at
                // the top of the arm, so the body sees the plain value.
                let derefs = boxed_binding_derefs(&arm.pattern);
                // A pattern binding used exactly once (loop-free) in the arm may
                // be moved at that use instead of cloned — so a recursive match
                // (`Node(l, r) -> check(l) + check(r)`) passes subtrees by move.
                let mut binds = Vec::new();
                pattern_bindings(&arm.pattern, &mut binds);
                let mut saved = Vec::new();
                for b in &binds {
                    let was = MOVEABLE.with(|m| m.borrow().contains(b));
                    saved.push((b.clone(), was));
                    let moveable = last_use_in_expr(&arm.body, b);
                    MOVEABLE.with(|m| {
                        if moveable {
                            m.borrow_mut().insert(b.clone());
                        } else {
                            m.borrow_mut().remove(b);
                        }
                    });
                }
                let body = if value {
                    gen_value(&arm.body)?
                } else {
                    gen_expr(&arm.body, false)?
                };
                for (b, was) in saved {
                    MOVEABLE.with(|m| {
                        if was {
                            m.borrow_mut().insert(b);
                        } else {
                            m.borrow_mut().remove(&b);
                        }
                    });
                }
                let body = if derefs.is_empty() {
                    body
                } else {
                    format!("{{ {derefs}{body} }}")
                };
                out.push_str(&format!("    {}{guard} => {body},\n", gen_pattern(&arm.pattern)?));
            }
            out.push('}');
            out
        }
        // Record construction (a positional `Ctor` after `records::lower`) ->
        // a named Rust struct literal, using the registered field order.
        // Built-in Option/Result constructors map straight to Rust's.
        Expr::Ctor { name, args } if matches!((name.as_str(), args.len()), ("None", 0) | ("Some", 1) | ("Ok", 1) | ("Err", 1)) => {
            if name == "None" {
                "None".to_string()
            } else {
                format!("{name}({})", gen_value(&args[0])?)
            }
        }
        Expr::Ctor { name, args } => {
            if let Some(fnames) = RECORD_FIELDS.with(|r| r.borrow().get(name).cloned()) {
                if fnames.len() != args.len() {
                    return Err(format!("native backend: wrong field count for record `{name}`"));
                }
                let parts: Result<Vec<String>, String> = fnames
                    .iter()
                    .zip(args)
                    .map(|(fname, a)| Ok(format!("{fname}: {}", gen_value(a)?)))
                    .collect();
                format!("{name} {{ {} }}", parts?.join(", "))
            } else if let Some(en) = VARIANT_TO_ENUM.with(|m| m.borrow().get(name).cloned()) {
                if args.is_empty() {
                    format!("{en}::{name}")
                } else {
                    let boxed = VARIANT_BOXED.with(|m| m.borrow().get(name).cloned());
                    let parts: Result<Vec<String>, String> = args
                        .iter()
                        .enumerate()
                        .map(|(i, a)| {
                            let s = gen_value(a)?;
                            // A recursive field is `Box`ed.
                            let b = boxed.as_ref().is_some_and(|v| v.get(i) == Some(&true));
                            Ok(if b { format!("Box::new({s})") } else { s })
                        })
                        .collect();
                    format!("{en}::{name}({})", parts?.join(", "))
                }
            } else {
                return Err(format!(
                    "native backend: `{name}` is not a supported record/enum constructor (generic types like Option/Result aren't supported yet)"
                ));
            }
        }
        // Field read yields a value copy (clone), preserving value semantics.
        Expr::Field { base, field } => format!("({}).{field}.clone()", gen_expr(base, true)?),
        // Record update `Point(x: 5, ..p)`: clone the base (so it's unchanged),
        // reassign the named fields, return it. No struct-type name needed.
        Expr::RecordUpdate { base, fields } => {
            let mut out = format!("{{ let mut __r = ({}).clone();", gen_expr(base, true)?);
            for (f, v) in fields {
                out.push_str(&format!(" __r.{f} = {};", gen_value(v)?));
            }
            out.push_str(" __r }");
            out
        }
        // `base[index]` — list subscript (a value copy, like `at`).
        Expr::Index { base, index } => {
            format!("({})[({}) as usize].clone()", gen_expr(base, true)?, gen_expr(index, true)?)
        }
        // `e?` propagates an Option/Result exactly as Rust's `?`.
        Expr::Try(inner) => format!("({})?", gen_expr(inner, true)?),
        // `e as T` is a capability narrowing — checked statically, identity at
        // runtime — so it lowers to the inner expression.
        Expr::As { expr, .. } => gen_expr(expr, value)?,
        // A lambda -> a Rust closure (parameter types inferred from the call).
        Expr::Lambda { params, body } => {
            let ps: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
            let body_str = gen_block(body, true)?;
            // A `move` closure owns its captures, so it can escape (be returned or
            // stored). To keep the originals usable afterward — and to preserve
            // value semantics — each captured binding is cloned into a shadow that
            // the closure then moves. (Closures themselves aren't `Clone`, so a
            // captured closure is moved directly.)
            let bound: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
            let mut fvs = HashSet::new();
            collect_free_vars(body, &bound, &mut fvs);
            let mut captures: Vec<String> = fvs.into_iter().filter(|v| !is_noclone(v)).collect();
            captures.sort();
            let clones: String = captures
                .iter()
                .map(|v| format!("let {v} = ({v}).clone(); "))
                .collect();
            // A lambda is wrapped in `Rc::new` so it is the uniform `Rc<dyn Fn>`
            // closure value (cloneable, escapable). It coerces to `dyn Fn` where
            // the slot type is known (a call argument, field, return).
            if clones.is_empty() {
                format!("std::rc::Rc::new(move |{}| {})", ps.join(", "), body_str)
            } else {
                format!("std::rc::Rc::new({{ {clones}move |{}| {} }})", ps.join(", "), body_str)
            }
        }
        // Applying a function value: `f(args)`. Reused-binding args cloned.
        Expr::Apply { func, args } => {
            let a: Vec<String> = args
                .iter()
                .map(|x| Ok(clone_if_var(x, gen_expr(x, true)?)))
                .collect::<Result<Vec<String>, String>>()?;
            format!("({})({})", gen_expr(func, true)?, a.join(", "))
        }
        // A list literal -> a Rust `vec![..]` (element type inferred by rustc).
        Expr::List(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(gen_value)
                .collect::<Result<_, _>>()?;
            format!("vec![{}]", parts.join(", "))
        }
        // A tuple literal: `(a, b)`. A 1-tuple needs the trailing comma.
        Expr::Tuple(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(gen_value)
                .collect::<Result<_, _>>()?;
            if parts.len() == 1 {
                format!("({},)", parts[0])
            } else {
                format!("({})", parts.join(", "))
            }
        }
        // `spawn Actor(args)` -> register the actor, yielding its Subject handle.
        Expr::Spawn { actor, args } => gen_spawn(actor, args)?,
        other => {
            return Err(format!(
                "native backend: unsupported expression `{}`",
                expr_kind(other)
            ))
        }
    })
}

fn gen_call(name: &str, args: &[Expr]) -> Result<String, String> {
    // Host primitives (compiler introspection, crypto) have stub witchy bodies
    // intercepted by a Rust impl at runtime (see `crate::native`). A standalone
    // native binary has no embedded compiler, so transpiling the stub would
    // silently misbehave — error instead.
    if crate::native::lookup(name).is_some() {
        return Err(format!(
            "native backend: `{name}` is a host primitive (compiler/crypto introspection), not available in a standalone native binary"
        ));
    }
    // `send(target, Msg(..))` — enqueue a message to an actor's mailbox. The
    // message is a constructor whose name is one of the actors' handlers.
    if name == "send" && args.len() == 2 {
        let target = gen_value(&args[0])?;
        let (msg_name, margs) = match &args[1] {
            Expr::Ctor { name, args } => (name.clone(), args.clone()),
            _ => {
                return Err(
                    "native backend: `send` expects a message constructor, e.g. send(a, Msg(..))"
                        .into(),
                )
            }
        };
        if !MSG_NAMES.with(|s| s.borrow().contains(&msg_name)) {
            return Err(format!(
                "native backend: `{msg_name}` is not an actor message (no handler declares it)"
            ));
        }
        let parts: Vec<String> = margs.iter().map(gen_value).collect::<Result<_, _>>()?;
        let payload = if parts.is_empty() {
            format!("WMsg::{msg_name}")
        } else {
            format!("WMsg::{msg_name}({})", parts.join(", "))
        };
        return Ok(format!("w_send({target}, {payload})"));
    }
    let arg = |i: usize| gen_expr(&args[i], true);
    Ok(match name {
        // `print(console, x)` — drop the console; print x to stdout. Strip one
        // trailing newline to match the interpreter/wasm host (each print is one
        // line; a trailing `\n` in the value is the terminator, not a blank line).
        "print" if args.len() == 2 => {
            // print's argument is always String-typed (the type checker requires
            // `String`), so format it directly — `trim_end_matches` borrows it (no
            // clone, unlike the old `w_show()`), stripping a host-style trailing
            // newline so each print is one line (matching interp/wasm).
            format!("println!(\"{{}}\", ({}).trim_end_matches('\\n'))", arg(1)?)
        }
        "int_to_string" if args.len() == 1 => format!("({}).to_string()", arg(0)?),
        // Generic `to_string` (what f-strings desugar to) shows any value exactly
        // as the interpreter's Display — lists, tuples, options, etc.
        "to_string" if args.len() == 1 => {
            USES_SHOW.with(|f| f.set(true));
            format!("({}).w_show()", arg(0)?)
        }
        "string_to_int" if args.len() == 1 => {
            format!("({}).parse::<i64>().unwrap()", arg(0)?)
        }
        // Dir capability: confined file I/O (the first arg is the Dir = a PathBuf).
        "read" if args.len() == 2 => {
            USES_DIR.with(|f| f.set(true));
            format!("w_dir_read(&({}), ({}).as_str())", arg(0)?, arg(1)?)
        }
        "write" if args.len() == 3 => {
            USES_DIR.with(|f| f.set(true));
            format!("w_dir_write(&({}), ({}).as_str(), ({}).as_str())", arg(0)?, arg(1)?, arg(2)?)
        }
        "subdir" if args.len() == 2 => {
            USES_DIR.with(|f| f.set(true));
            format!("w_dir_resolve(&({}), ({}).as_str())", arg(0)?, arg(1)?)
        }
        "exists" if args.len() == 2 => {
            USES_DIR.with(|f| f.set(true));
            format!("w_dir_exists(&({}), ({}).as_str())", arg(0)?, arg(1)?)
        }
        "make_dir" if args.len() == 2 => {
            USES_DIR.with(|f| f.set(true));
            format!("w_dir_make(&({}), ({}).as_str())", arg(0)?, arg(1)?)
        }
        "list" if args.len() == 1 => {
            USES_DIR.with(|f| f.set(true));
            format!("w_dir_list(&({}))", arg(0)?)
        }
        // Env capability: `get_env(env, name) -> Option(String)` (the env arg, a
        // unit, is dropped — the authority was the capability itself).
        "get_env" if args.len() == 2 => {
            format!("std::env::var(({}).as_str()).ok()", arg(1)?)
        }
        // Net capability: confined TCP (see NET_HELPER). The Net allow-list is
        // borrowed; sockets/listeners are usize handles into thread-local tables.
        "restrict" if args.len() == 2 => {
            USES_NET.with(|f| f.set(true));
            format!("w_net_restrict(&({}), ({}).as_str())", arg(0)?, arg(1)?)
        }
        "connect" if args.len() == 2 => {
            USES_NET.with(|f| f.set(true));
            format!("w_net_connect(&({}), ({}).as_str())", arg(0)?, arg(1)?)
        }
        "listen" if args.len() == 2 => {
            USES_NET.with(|f| f.set(true));
            format!("w_net_listen(&({}), ({}).as_str())", arg(0)?, arg(1)?)
        }
        "accept" if args.len() == 1 => {
            USES_NET.with(|f| f.set(true));
            format!("w_net_accept({})", arg(0)?)
        }
        "send_line" if args.len() == 2 => {
            USES_NET.with(|f| f.set(true));
            format!("w_net_send_line({}, ({}).as_str())", arg(0)?, arg(1)?)
        }
        "send_bytes" if args.len() == 2 => {
            USES_NET.with(|f| f.set(true));
            format!("w_net_send_bytes({}, ({}).as_str())", arg(0)?, arg(1)?)
        }
        "recv_line" if args.len() == 1 => {
            USES_NET.with(|f| f.set(true));
            format!("w_net_recv_line({})", arg(0)?)
        }
        "recv_all" if args.len() == 1 => {
            USES_NET.with(|f| f.set(true));
            format!("w_net_recv_all({})", arg(0)?)
        }
        "recv_bytes" if args.len() == 2 => {
            USES_NET.with(|f| f.set(true));
            format!("w_net_recv_bytes({}, {})", arg(0)?, arg(1)?)
        }
        "close" if args.len() == 1 => {
            USES_NET.with(|f| f.set(true));
            format!("w_net_close({})", arg(0)?)
        }
        // List builtins. `push`/`concat` consume the list and return it (so the
        // common `acc = push(acc, x)` is an O(1) in-place append, and Rust's
        // move-checking enforces value semantics on any reuse).
        "length" if args.len() == 1 => format!("(({}).len() as i64)", arg(0)?),
        "at" if args.len() == 2 => format!("({})[({}) as usize].clone()", arg(0)?, arg(1)?),
        "push" if args.len() == 2 => {
            format!("{{ let mut __v = {}; __v.push({}); __v }}", arg(0)?, arg(1)?)
        }
        "concat" if args.len() == 2 => {
            format!("{{ let mut __v = {}; __v.extend({}); __v }}", arg(0)?, arg(1)?)
        }
        // Dict builtins (a HashMap with a fast hasher; iteration order is
        // unspecified). insert/remove consume and return the dict.
        "dict_new" if args.is_empty() => {
            USES_DICT.with(|f| f.set(true));
            "WMap::default()".to_string()
        }
        "insert" if args.len() == 3 => {
            USES_DICT.with(|f| f.set(true));
            // Evaluate key+value (which may read the dict) BEFORE moving the dict
            // in. `gen_value` moves a last-use argument and clones a reused one, so
            // value semantics hold while a fresh key/value is moved in, not copied.
            format!(
                "{{ let __k = {}; let __val = {}; let mut __m = {}; __m.insert(__k, __val); __m }}",
                gen_value(&args[1])?,
                gen_value(&args[2])?,
                arg(0)?
            )
        }
        "get_or" if args.len() == 3 => {
            format!("({}).get(&({})).cloned().unwrap_or({})", arg(0)?, arg(1)?, arg(2)?)
        }
        // Single-hash upsert: `d.entry(k).or_insert(default)`, then apply the
        // updater to the slot. A literal updater lambda is INLINED (no `Rc`
        // allocation per call); any other function value is called. This is one
        // hash + (for a literal lambda) zero allocation, vs the `insert(d, k,
        // f(get_or(d, k, default)))` idiom's two hashes and a closure per call.
        "update" if args.len() == 4 => {
            USES_DICT.with(|f| f.set(true));
            let dict = arg(0)?;
            // The key goes through `gen_value`: a last-use key variable is MOVED
            // into the map (no clone) — which the `insert(d, k, get_or(d, k, ..))`
            // idiom never could, since it used the key twice. A reused key clones,
            // preserving value semantics.
            let key = gen_value(&args[1])?;
            let default = arg(2)?;
            let apply = match &args[3] {
                Expr::Lambda { params, body } if params.len() == 1 => {
                    let p = local_ident(&params[0].name);
                    let b = gen_block(body, true)?;
                    format!("let {p} = (*__e).clone(); *__e = {b};")
                }
                other => {
                    let f = gen_value(other)?;
                    format!("let __cur = (*__e).clone(); *__e = ({f})(__cur);")
                }
            };
            format!(
                "{{ let __k = {key}; let __def = {default}; let mut __m = {dict}; {{ let __e = __m.entry(__k).or_insert(__def); {apply} }} __m }}"
            )
        }
        "has" if args.len() == 2 => format!("({}).contains_key(&({}))", arg(0)?, arg(1)?),
        "remove" if args.len() == 2 => {
            format!(
                "{{ let __k = {}; let mut __m = {}; __m.remove(&__k); __m }}",
                gen_value(&args[1])?,
                arg(0)?
            )
        }
        "keys" if args.len() == 1 => {
            format!("({}).keys().cloned().collect::<Vec<_>>()", arg(0)?)
        }
        "values" if args.len() == 1 => {
            format!("({}).values().cloned().collect::<Vec<_>>()", arg(0)?)
        }
        "pairs" if args.len() == 1 => format!(
            "({}).iter().map(|(k, v)| (k.clone(), v.clone())).collect::<Vec<_>>()",
            arg(0)?
        ),
        "size" if args.len() == 1 => format!("(({}).len() as i64)", arg(0)?),
        // Numeric conversions.
        "int_to_float" if args.len() == 1 => format!("(({}) as f64)", arg(0)?),
        "float_to_int" if args.len() == 1 => format!("(({}) as i64)", arg(0)?),
        // Int and Duration share the i64 representation, so the conversions are
        // the identity (matching the interpreter).
        "int_to_duration" | "duration_to_int" if args.len() == 1 => format!("({})", arg(0)?),
        // Clock capability: `now(clock) -> Int`, milliseconds since the Unix
        // epoch (the clock arg, a unit, is dropped — the authority was the cap).
        "now" if args.len() == 1 => "std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)".to_string(),
        // `sqrt` is a primitive (it can't be written in witchy); f64 has it native,
        // matching the interpreter (`x.sqrt()`) and wasm (`f64.sqrt`).
        "sqrt" if args.len() == 1 => format!("({}).sqrt()", arg(0)?),
        // String builtins (deterministic; char-based ops match the interpreter).
        "string_length" if args.len() == 1 => format!("(({}).len() as i64)", arg(0)?),
        "char_count" if args.len() == 1 => format!("(({}).chars().count() as i64)", arg(0)?),
        "to_upper" if args.len() == 1 => format!("({}).to_uppercase()", arg(0)?),
        "to_lower" if args.len() == 1 => format!("({}).to_lowercase()", arg(0)?),
        "trim" if args.len() == 1 => format!("({}).trim().to_string()", arg(0)?),
        "starts_with" if args.len() == 2 => {
            format!("({}).starts_with(({}).as_str())", arg(0)?, arg(1)?)
        }
        "ends_with" if args.len() == 2 => {
            format!("({}).ends_with(({}).as_str())", arg(0)?, arg(1)?)
        }
        "contains" if args.len() == 2 => {
            format!("({}).contains(({}).as_str())", arg(0)?, arg(1)?)
        }
        "replace" if args.len() == 3 => format!(
            "({}).replace(({}).as_str(), ({}).as_str())",
            arg(0)?,
            arg(1)?,
            arg(2)?
        ),
        "split" if args.len() == 2 => format!(
            "{{ let __s = {}; let __sep = {}; if __sep.is_empty() {{ vec![__s] }} else {{ __s.split(__sep.as_str()).map(|p| p.to_string()).collect::<Vec<String>>() }} }}",
            clone_if_var(&args[0], arg(0)?),
            clone_if_var(&args[1], arg(1)?)
        ),
        // The characters of a string, each a single-char String, in one O(n) pass.
        "string_chars" if args.len() == 1 => {
            format!("({}).chars().map(|c| c.to_string()).collect::<Vec<String>>()", arg(0)?)
        }
        "index_of" if args.len() == 2 => format!(
            "{{ let __s = {}; let __sub = {}; __s.find(__sub.as_str()).map(|b| __s[..b].chars().count() as i64).unwrap_or(-1) }}",
            clone_if_var(&args[0], arg(0)?),
            clone_if_var(&args[1], arg(1)?)
        ),
        // Half-open character range [start, end), clamped. Walk only to `end`
        // (skip/take) instead of collecting the WHOLE string into a Vec<char> on
        // every call — the old form made each char access O(string length) and
        // allocated a full char vector per call, which is brutal for parsers that
        // scan a string one char at a time.
        "substring" if args.len() == 3 => format!(
            "{{ let __lo = (({}).max(0)) as usize; let __hi = (({}).max(0)) as usize; if __hi > __lo {{ ({}).chars().skip(__lo).take(__hi - __lo).collect::<String>() }} else {{ String::new() }} }}",
            arg(1)?,
            arg(2)?,
            arg(0)?
        ),
        "print" | "int_to_string" | "string_to_int" => {
            return Err(format!("native backend: wrong arity for `{name}`"))
        }
        // A call whose name is a closure value — a function-valued parameter or a
        // local binding (`let add5 = make_adder(5); add5(10)`) — is called
        // directly. Anything in FN_NAMES is a top-level function (handled below);
        // a name that's neither a builtin nor a known function is such a local.
        _ if CUR_PARAMS.with(|s| s.borrow().contains(name)) || !is_fn_ref(name) => {
            // Reused-binding args are cloned (value semantics: `keep(x)` then
            // `push(.., x)`); temporaries are moved.
            let a: Vec<String> = args
                .iter()
                .map(|x| Ok(clone_if_var(x, gen_expr(x, true)?)))
                .collect::<Result<Vec<String>, String>>()?;
            format!("({name})({})", a.join(", "))
        }
        // Any other call is a user function (capability/builtin calls outside the
        // subset will surface as an undefined `w_*` at rustc time, which is loud).
        _ => {
            let fname = format!("w_{}", ident(name));
            let clones = FN_PARAM_CLONE.with(|m| m.borrow().get(&fname).cloned());
            let borrows = FN_PARAM_BORROW.with(|m| m.borrow().get(&fname).cloned());
            let a: Vec<String> = args
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    let s = gen_expr(x, true)?;
                    // A `let`/borrow parameter takes `&T`: pass a reference (no
                    // clone). An argument that is itself a borrow parameter is
                    // already `&T`, so forward it as-is.
                    if borrows.as_ref().is_some_and(|b| b.get(i) == Some(&true)) {
                        return Ok(match x {
                            Expr::Var(v) if is_cur_borrow(v) => s,
                            _ => format!("&({s})"),
                        });
                    }
                    // Forwarding a borrow (`&T`) to an OWNED parameter needs an
                    // owned value — and you can't move out of a borrow, so it is
                    // deref-cloned.
                    if matches!(x, Expr::Var(v) if is_cur_borrow(v)) {
                        return Ok(format!("(*{s}).clone()"));
                    }
                    // Clone only a reused binding (a bare variable); a temporary
                    // (call result, literal, field read — already a fresh value)
                    // is moved, as is a binding at its last use (see MOVEABLE).
                    let clone = clones.as_ref().is_some_and(|c| c.get(i) == Some(&true))
                        && matches!(x, Expr::Var(v) if !is_moveable(v) && !is_noclone(v))
                        && !is_moveable_occ(x);
                    Ok(if clone { format!("({s}).clone()") } else { s })
                })
                .collect::<Result<Vec<String>, String>>()?;
            format!("{fname}({})", a.join(", "))
        }
    })
}

/// For a constructor pattern with `Box`ed (recursive) fields, the `let x = *x;`
/// statements that move each boxed binding out of its box (so the arm body uses
/// the plain value). Only simple variable bindings are handled.
/// Lower a `match` with list patterns to a guarded if-chain over an Option
/// result: each arm tests its length condition, binds elements by index
/// (cloned), then applies its guard — a failing guard falls through to the
/// NEXT arm, exactly the interpreter's top-to-bottom rule.
fn gen_list_match(scrutinee: String, arms: &[crate::ast::MatchArm], value: bool) -> Result<String, String> {
    use crate::ast::Pattern;
    let mut out = format!("{{ let __m = {scrutinee};\n    let mut __r = None;\n");
    for arm in arms {
        let guard = match &arm.guard {
            Some(g) => gen_value(g)?,
            None => "true".to_string(),
        };
        let (cond, binds) = match &arm.pattern {
            Pattern::List { elems, rest } => {
                let mut binds = String::new();
                for (i, e) in elems.iter().enumerate() {
                    match e {
                        Pattern::Var(n) => binds.push_str(&format!("let {n} = __m[{i}].clone(); ")),
                        Pattern::Wildcard => {}
                        _ => return Err("native backend: a nested pattern inside a list pattern is not supported yet".into()),
                    }
                }
                let cond = match rest {
                    None => format!("__m.len() == {}", elems.len()),
                    Some(tail) => {
                        if let Some(name) = tail {
                            binds.push_str(&format!("let {name} = __m[{}..].to_vec(); ", elems.len()));
                        }
                        format!("__m.len() >= {}", elems.len())
                    }
                };
                (cond, binds)
            }
            Pattern::Var(n) => ("true".to_string(), format!("let {n} = __m.clone(); ")),
            Pattern::Wildcard => ("true".to_string(), String::new()),
            _ => {
                return Err("native backend: a list match's arms must be list, variable, or wildcard patterns".into())
            }
        };
        let body = if value { gen_value(&arm.body)? } else { gen_expr(&arm.body, false)? };
        let assign = if value {
            format!("__r = Some({body});")
        } else {
            format!("{body} __r = Some(());")
        };
        out.push_str(&format!(
            "    if __r.is_none() && {cond} {{ {binds}if {guard} {{ {assign} }} }}\n"
        ));
        // An unguarded variable/wildcard arm matches unconditionally — arms
        // after it can never run.
        if arm.guard.is_none() && matches!(&arm.pattern, Pattern::Var(_) | Pattern::Wildcard) {
            break;
        }
    }
    if value {
        out.push_str("    __r.unwrap_or_else(|| unreachable!(\"non-exhaustive list match\")) }");
    } else {
        out.push_str("    if __r.is_none() { unreachable!(\"non-exhaustive list match\") } }");
    }
    Ok(out)
}

/// Lower a `match` with string patterns to a guarded if-chain over an Option
/// result, comparing the scrutinee to each literal — exact top-to-bottom
/// semantics (a failing guard falls through to the next arm), and a variable
/// arm binds an owned `String`, sidestepping `String`/`&str` mismatches.
fn gen_str_match(scrutinee: String, arms: &[crate::ast::MatchArm], value: bool) -> Result<String, String> {
    use crate::ast::Pattern;
    let mut out = format!("{{ let __m = {scrutinee};\n    let mut __r = None;\n");
    for arm in arms {
        let guard = match &arm.guard {
            Some(g) => gen_value(g)?,
            None => "true".to_string(),
        };
        let (cond, binds) = match &arm.pattern {
            Pattern::Str(s) => (format!("__m == {s:?}"), String::new()),
            Pattern::Var(n) => ("true".to_string(), format!("let {n} = __m.clone(); ")),
            Pattern::Wildcard => ("true".to_string(), String::new()),
            _ => {
                return Err("native backend: a string match's arms must be string, variable, or wildcard patterns".into())
            }
        };
        let body = if value { gen_value(&arm.body)? } else { gen_expr(&arm.body, false)? };
        let assign = if value {
            format!("__r = Some({body});")
        } else {
            format!("{body} __r = Some(());")
        };
        out.push_str(&format!(
            "    if __r.is_none() && {cond} {{ {binds}if {guard} {{ {assign} }} }}\n"
        ));
        if arm.guard.is_none() && matches!(&arm.pattern, Pattern::Var(_) | Pattern::Wildcard) {
            break;
        }
    }
    if value {
        out.push_str("    __r.unwrap_or_else(|| unreachable!(\"non-exhaustive string match\")) }");
    } else {
        out.push_str("    if __r.is_none() { unreachable!(\"non-exhaustive string match\") } }");
    }
    Ok(out)
}

fn boxed_binding_derefs(p: &crate::ast::Pattern) -> String {
    use crate::ast::Pattern;
    let Pattern::Ctor { name, args } = p else {
        return String::new();
    };
    let Some(boxed) = VARIANT_BOXED.with(|m| m.borrow().get(name).cloned()) else {
        return String::new();
    };
    let mut out = String::new();
    for (i, arg) in args.iter().enumerate() {
        if boxed.get(i) == Some(&true) {
            if let Pattern::Var(v) = arg {
                out.push_str(&format!("let {v} = *{v}; "));
            }
        }
    }
    out
}

fn gen_pattern(p: &crate::ast::Pattern) -> Result<String, String> {
    use crate::ast::Pattern;
    Ok(match p {
        Pattern::Wildcard => "_".into(),
        Pattern::Var(n) => n.clone(),
        Pattern::Int(n) => format!("{n}"),
        Pattern::Bool(b) => b.to_string(),
        Pattern::Tuple(ps) => {
            let parts: Vec<String> = ps.iter().map(gen_pattern).collect::<Result<_, _>>()?;
            format!("({})", parts.join(", "))
        }
        Pattern::Str(_) => {
            return Err("native backend: string patterns in `match` are not supported yet".into())
        }
        // Built-in Option/Result patterns map straight to Rust's.
        Pattern::Ctor { name, args }
            if matches!((name.as_str(), args.len()), ("None", 0) | ("Some", 1) | ("Ok", 1) | ("Err", 1)) =>
        {
            if name == "None" {
                "None".to_string()
            } else {
                format!("{name}({})", gen_pattern(&args[0])?)
            }
        }
        Pattern::Ctor { name, args } => {
            let en = VARIANT_TO_ENUM
                .with(|m| m.borrow().get(name).cloned())
                .ok_or_else(|| {
                    format!(
                        "native backend: constructor pattern `{name}` (a generic user type's variants aren't supported yet)"
                    )
                })?;
            if args.is_empty() {
                format!("{en}::{name}")
            } else {
                let parts: Vec<String> = args.iter().map(gen_pattern).collect::<Result<_, _>>()?;
                format!("{en}::{name}({})", parts.join(", "))
            }
        }
        Pattern::List { .. } => {
            return Err("native backend: list patterns in `match` are not supported yet".into())
        }
    })
}

/// Make a witchy name a valid Rust identifier: the linker module-qualifies
/// functions as `stem.fn`, and `.` isn't legal in an identifier.
fn ident(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

/// Emit a witchy local name (parameter, variable) as a valid Rust identifier.
/// Witchy spells trait-method receivers `self`, which `traits::lower` turns into
/// an ordinary parameter of a free function — but Rust reserves `self` (and a few
/// other words) for associated functions, so the name must be remapped. Words
/// that can't be raw identifiers (`self`/`Self`/`crate`/`super`) are renamed;
/// any other Rust keyword a witchy identifier might collide with becomes a raw
/// identifier (`r#name`), preserving the name verbatim. Applied only at the
/// point a name is printed — all move/clone analysis stays keyed on the witchy
/// name, so emission and analysis can't drift.
fn local_ident(name: &str) -> String {
    match name {
        "self" | "Self" | "crate" | "super" => format!("w_{name}"),
        "as" | "async" | "await" | "become" | "box" | "break" | "const" | "continue"
        | "dyn" | "else" | "enum" | "extern" | "false" | "fn" | "for" | "if" | "impl"
        | "in" | "let" | "loop" | "macro" | "match" | "mod" | "move" | "mut" | "priv"
        | "pub" | "ref" | "return" | "static" | "struct" | "trait" | "true" | "type"
        | "typeof" | "unsafe" | "unsized" | "use" | "virtual" | "where" | "while"
        | "yield" | "abstract" | "do" | "final" | "override" => format!("r#{name}"),
        _ => name.to_string(),
    }
}

fn rust_ty(t: &Type) -> Option<String> {
    match t {
        Type::Named(n, args) => match n.as_str() {
            "Int" | "Duration" => Some("i64".into()),
            "Float" => Some("f64".into()),
            "Bool" => Some("bool".into()),
            "String" => Some("String".into()),
            // Console/Env/Clock are no-op capability handles (a unit value passed
            // around); their host functions reach ambient authority. `Nil` is the
            // unit type (e.g. an explicit `-> Nil` return).
            "Console" | "Env" | "Clock" | "Nil" => Some("()".into()),
            // A Dir capability is a confined directory root.
            "Dir" => {
                USES_DIR.with(|f| f.set(true));
                Some("std::path::PathBuf".into())
            }
            // A Net capability is an allow-list of `host:port`; sockets and
            // listeners are handles into the thread-local tables (see NET_HELPER).
            "Net" => Some("Vec<String>".into()),
            "Socket" | "Listener" => Some("usize".into()),
            // A Subject is a handle (index) into the actor registry — the
            // authority to send an actor messages.
            "Subject" => Some("usize".into()),
            // A list maps to a Rust Vec of its element type.
            "List" => Some(format!("Vec<{}>", rust_ty(args.first()?)?)),
            // A dict maps to a fast HashMap (see DICT_HELPER).
            "Dict" => {
                USES_DICT.with(|f| f.set(true));
                Some(format!(
                    "WMap<{}, {}>",
                    rust_ty(args.first()?)?,
                    rust_ty(args.get(1)?)?
                ))
            }
            // Option/Result map onto Rust's, with their element types.
            "Option" => Some(format!("Option<{}>", rust_ty(args.first()?)?)),
            "Result" => Some(format!(
                "Result<{}, {}>",
                rust_ty(args.first()?)?,
                rust_ty(args.get(1)?)?
            )),
            // A user record/enum type maps to its Rust struct/enum, carrying any
            // type arguments (`Pair(Int, String)` -> `Pair<i64, String>`).
            other
                if RECORD_FIELDS.with(|r| r.borrow().contains_key(other))
                    || ENUM_NAMES.with(|s| s.borrow().contains(other)) =>
            {
                if args.is_empty() {
                    Some(other.to_string())
                } else {
                    let a: Option<Vec<String>> = args.iter().map(rust_ty).collect();
                    Some(format!("{other}<{}>", a?.join(", ")))
                }
            }
            // A type variable -> a Rust generic parameter.
            other if is_type_var(other) => Some(type_var_name(other)),
            _ => None,
        },
        // A tuple maps to a Rust tuple, supported when all elements are.
        Type::Tuple(ts) => {
            let parts: Option<Vec<String>> = ts.iter().map(rust_ty).collect();
            parts.map(|p| format!("({})", p.join(", ")))
        }
        // A function type -> a reference-counted trait object, uniformly in every
        // position (param, return, field, collection element). `Rc<dyn Fn>` is
        // cheaply `Clone`able, so a closure can be reused (passed to several
        // calls), stored in a value type, and escape — unlike a bare `impl Fn`.
        Type::Fn(args, ret) => {
            let ps: Option<Vec<String>> = args.iter().map(rust_ty).collect();
            Some(format!("std::rc::Rc<dyn Fn({}) -> {}>", ps?.join(", "), rust_ty(ret)?))
        }
    }
}

fn bin_op(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Eq => "==",
        BinOp::NotEq => "!=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Concat => "+",
    }
}

fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::List(_) => "list",
        Expr::Tuple(_) => "tuple",
        Expr::MethodCall { .. } => "method call",
        Expr::Apply { .. } => "function-value application",
        Expr::Ctor { .. } => "constructor",
        Expr::Field { .. } => "field access",
        Expr::Lambda { .. } => "closure",
        Expr::Record { .. } | Expr::RecordUpdate { .. } => "record",
        Expr::Try(_) => "`?`",
        Expr::As { .. } => "`as`",
        Expr::Match { .. } => "match",
        Expr::Range { .. } => "range value",
        Expr::Index { .. } => "index",
        Expr::WhileLet { .. } => "while-let",
        Expr::Spawn { .. } => "spawn",
        _ => "expression",
    }
}
