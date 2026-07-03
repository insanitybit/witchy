//! A type checker for witchy.
//!
//! Annotation-driven checking with light Hindley-Milner-style unification for
//! the bits that aren't annotated (let bindings, match arms). It is deliberately
//! lenient where it lacks information (e.g. an unknown constructor's arguments)
//! so it never rejects a valid program — it tightens as the type system grows.
//!
//! Capability safety is not a special case: `print` has type
//! `(Console, String) -> Nil`, and the only way to obtain a `Console` is to
//! receive one as a parameter — ultimately from `main`. So "this code may
//! perform output" is simply visible in its type, and code that never received
//! the capability cannot type-check a call that needs it.

use std::collections::{HashMap, HashSet};
use std::fmt;

use witchy_syntax::ast::{
    self, Block, Convention, Expr, Function, Item, MatchArm, Module, Pattern, Stmt, UnOp,
};
use witchy_syntax::build_entry::{build_entrypoint, is_build_capability_type};

/// The operations a `Dir` capability permits. Decomposing the capability by
/// right makes the footprint distinguish read-only from writing code, and an op
/// that needs a right it wasn't granted is a compile-time error. Bare `Dir` is
/// the full set; `Dir[Read]`/`Dir[Write]` narrow it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DirRights {
    pub read: bool,
    pub write: bool,
}

impl DirRights {
    pub fn full() -> Self {
        DirRights { read: true, write: true }
    }
}

impl fmt::Display for DirRights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.read, self.write) {
            (true, true) => write!(f, "Dir"),
            (true, false) => write!(f, "Dir[Read]"),
            (false, true) => write!(f, "Dir[Write]"),
            (false, false) => write!(f, "Dir[]"),
        }
    }
}

/// Interpret a `Dir`'s type arguments as its rights. Bare `Dir` (no args) is the
/// full set; `Dir[Read]`/`Dir[Write]`/`Dir[Read, Write]` narrow it.
fn dir_rights(args: &[ast::Type]) -> DirRights {
    if args.is_empty() {
        return DirRights::full();
    }
    let mut r = DirRights { read: false, write: false };
    for a in args {
        if let ast::Type::Named(n, _) = a {
            match n.as_str() {
                "Read" => r.read = true,
                "Write" => r.write = true,
                _ => {}
            }
        }
    }
    r
}

/// The operations a `File` capability permits — the *leaf* of the same hierarchy
/// as `Dir` (authority to one file vs. one subtree, RFC-0012). Mirrors `DirRights`:
/// a `File` carries no path-scope to refine (it is already a leaf), so its only
/// refinement axis is its rights. (`Exec` — folding ambient `Exec` into
/// `File[Exec]` — is a later addition; today a `File` is read/write.)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FileRights {
    pub read: bool,
    pub write: bool,
}

impl FileRights {
    pub fn full() -> Self {
        FileRights { read: true, write: true }
    }
}

impl fmt::Display for FileRights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.read, self.write) {
            (true, true) => write!(f, "File"),
            (true, false) => write!(f, "File[Read]"),
            (false, true) => write!(f, "File[Write]"),
            (false, false) => write!(f, "File[]"),
        }
    }
}

/// Interpret a `File`'s type arguments as its rights (bare `File` is the full set).
fn file_rights(args: &[ast::Type]) -> FileRights {
    if args.is_empty() {
        return FileRights::full();
    }
    let mut r = FileRights { read: false, write: false };
    for a in args {
        if let ast::Type::Named(n, _) = a {
            match n.as_str() {
                "Read" => r.read = true,
                "Write" => r.write = true,
                _ => {}
            }
        }
    }
    r
}

/// The rights a `Net` capability permits, on two independent axes. **Verbs**:
/// `Connect` lets code dial out (`connect`, `restrict`); `Listen` lets it accept
/// inbound (`listen`) — distinguishing a client from a server. **Transports**:
/// `Tcp`/`Udp`/`Uds` — though only TCP is implemented at runtime, so `connect`/
/// `listen` require `Tcp`; `Udp`/`Uds` are type-level markers that keep the
/// taxonomy expressible (and auditable) even though the transport isn't.
///
/// Each axis defaults independently: an unmentioned axis is *full*. Bare `Net` is
/// full verbs + full transports; `Net[Connect]` is connect-only over all
/// transports; `Net[Tcp]` is all verbs over TCP only; `Net[Connect, Tcp]` is both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetRights {
    pub connect: bool,
    pub listen: bool,
    pub tcp: bool,
    pub udp: bool,
    pub uds: bool,
}

impl NetRights {
    pub fn full() -> Self {
        NetRights { connect: true, listen: true, tcp: true, udp: true, uds: true }
    }

    fn verbs_full(&self) -> bool {
        self.connect && self.listen
    }

    fn transports_full(&self) -> bool {
        self.tcp && self.udp && self.uds
    }
}

impl fmt::Display for NetRights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.verbs_full() && self.transports_full() {
            return write!(f, "Net");
        }
        // List only the narrowed axes; an axis at its full set is omitted (so
        // `Net[Connect]` reads as "connect-only, any transport").
        let mut parts: Vec<&str> = Vec::new();
        if !self.verbs_full() {
            if self.connect {
                parts.push("Connect");
            }
            if self.listen {
                parts.push("Listen");
            }
        }
        if !self.transports_full() {
            if self.tcp {
                parts.push("Tcp");
            }
            if self.udp {
                parts.push("Udp");
            }
            if self.uds {
                parts.push("Uds");
            }
        }
        write!(f, "Net[{}]", parts.join(", "))
    }
}

/// Interpret a `Net`'s type arguments as its rights. Bare `Net` (no args) is the
/// full set. Each axis defaults to full independently: `Net[Connect]` keeps all
/// transports, `Net[Tcp]` keeps all verbs. Unrecognized markers are ignored.
fn net_rights(args: &[ast::Type]) -> NetRights {
    if args.is_empty() {
        return NetRights::full();
    }
    let mut r = NetRights { connect: false, listen: false, tcp: false, udp: false, uds: false };
    let (mut saw_verb, mut saw_transport) = (false, false);
    for a in args {
        if let ast::Type::Named(n, _) = a {
            match n.as_str() {
                "Connect" => (r.connect, saw_verb) = (true, true),
                "Listen" => (r.listen, saw_verb) = (true, true),
                "Tcp" => (r.tcp, saw_transport) = (true, true),
                "Udp" => (r.udp, saw_transport) = (true, true),
                "Uds" => (r.uds, saw_transport) = (true, true),
                _ => {}
            }
        }
    }
    if !saw_verb {
        (r.connect, r.listen) = (true, true);
    }
    if !saw_transport {
        (r.tcp, r.udp, r.uds) = (true, true, true);
    }
    r
}

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Int,
    Float,
    /// A length of time (from a duration literal like `30s`). A distinct type:
    /// it does not mix with `Int` under arithmetic.
    Duration,
    String,
    Bytes,
    Bool,
    Nil,
    Console,
    Clock,
    /// The runtime authority to draw cryptographic randomness (`rand_u64`).
    Rand,
    Env,
    Secret,
    /// The runtime authority to spawn a native subprocess. Right-less (one op):
    /// the executable is named and confined through a `Dir[Read]` argument, so
    /// "you can only execute a file you can read". See rfcs/0004-self-hosted-cli.md.
    Exec,
    Dir(DirRights),
    File(FileRights),
    Net(NetRights),
    Socket,
    Listener,
    /// Build-time capabilities — a parallel set to the runtime caps, granted only
    /// to a rune's `build` entrypoint and enforced in a zero-ambient build
    /// sandbox. Kind-only (the specific tool/host/dir/var is the consumer's grant,
    /// not the type); see rfcs/build-time-execution-plan.md.
    BuildOut,
    BuildRead,
    BuildEnv,
    BuildNet,
    BuildExec,
    List(Box<Ty>),
    Tuple(Vec<Ty>),
    /// A user-declared type, possibly with type arguments: `Option(Int)`,
    /// `Result(String, Error)`. Non-generic types carry an empty argument list.
    Named(String, Vec<Ty>),
    /// A function type: parameter types and a return type.
    Fn(Vec<Ty>, Box<Ty>),
    Var(u32),
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Int => write!(f, "Int"),
            Ty::Float => write!(f, "Float"),
            Ty::Duration => write!(f, "Duration"),
            Ty::String => write!(f, "String"),
            Ty::Bytes => write!(f, "Bytes"),
            Ty::Bool => write!(f, "Bool"),
            Ty::Nil => write!(f, "Nil"),
            Ty::Console => write!(f, "Console"),
            Ty::Clock => write!(f, "Clock"),
            Ty::Rand => write!(f, "Rand"),
            Ty::Env => write!(f, "Env"),
            Ty::Secret => write!(f, "Secret"),
            Ty::Exec => write!(f, "Exec"),
            Ty::Dir(r) => write!(f, "{r}"),
            Ty::File(r) => write!(f, "{r}"),
            Ty::Net(r) => write!(f, "{r}"),
            Ty::Socket => write!(f, "Socket"),
            Ty::Listener => write!(f, "Listener"),
            Ty::BuildOut => write!(f, "BuildOut"),
            Ty::BuildRead => write!(f, "BuildRead"),
            Ty::BuildEnv => write!(f, "BuildEnv"),
            Ty::BuildNet => write!(f, "BuildNet"),
            Ty::BuildExec => write!(f, "BuildExec"),
            Ty::List(e) => write!(f, "List({e})"),
            Ty::Tuple(ts) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ")")
            }
            Ty::Named(n, args) => {
                write!(f, "{n}")?;
                if !args.is_empty() {
                    write!(f, "(")?;
                    for (i, t) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{t}")?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            Ty::Fn(params, ret) => {
                write!(f, "fn(")?;
                for (i, t) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ") -> {ret}")
            }
            Ty::Var(_) => write!(f, "?"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub message: String,
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "type error: {}", self.message)
    }
}

impl std::error::Error for TypeError {}

fn terr<T>(message: impl Into<String>) -> Result<T, TypeError> {
    Err(TypeError {
        message: message.into(),
    })
}

/// Prefix a type error with where it occurred — the enclosing function (after
/// linking this is `module.func`, which also names the file) and source line.
/// `line == 0` means no line is available; an empty `func` omits the name.
fn at_loc(e: TypeError, line: u32, func: &str) -> TypeError {
    if line == 0 {
        return e;
    }
    let where_ = if func.is_empty() {
        format!("line {line}")
    } else {
        format!("`{func}`, line {line}")
    };
    TypeError {
        message: format!("{where_}: {}", e.message),
    }
}

/// Reject two top-level functions with the same name. Witchy has no
/// free-function overloading — a second definition silently overwrites the first
/// (in both the linker's and the checker's name tables), so the duplicate is
/// always a bug (a typo or a copy/paste). Methods live in `impl` blocks and are
/// dispatched by receiver type, so they are not affected. Names may be
/// module-qualified (`main.f`) after linking; the message shows the bare name.
fn check_unique_functions(module: &Module) -> Result<(), TypeError> {
    let mut seen: HashMap<&str, u32> = HashMap::new();
    for (idx, item) in module.items.iter().enumerate() {
        if let Item::Function(f) = item {
            let line = module.item_lines.get(idx).copied().unwrap_or(0);
            if let Some(&first) = seen.get(f.name.as_str()) {
                let bare = f.name.rsplit('.').next().unwrap_or(&f.name);
                let where_ = if first != 0 && line != 0 {
                    format!(" (lines {first} and {line})")
                } else {
                    String::new()
                };
                return terr(format!(
                    "function `{bare}` is defined more than once{where_}; \
                     top-level function names must be unique"
                ));
            }
            seen.insert(f.name.as_str(), line);
        }
    }
    Ok(())
}

/// The type names the checker knows without a declaration: primitives, host
/// capabilities, and the built-in generics. Mirrors the named arms of
/// `to_ty_generic` plus the opaque generics the checker itself produces
/// (`Option`/`Result`/`Dict`). Any other named type must be declared (a `type`
/// or be a lowercase generic parameter.
const BUILTIN_TYPE_NAMES: &[&str] = &[
    "Int", "Float", "Duration", "String", "Bytes", "Bool", "Nil", "Console", "Clock", "Rand", "Env", "Secret",
    "SecretStore", "Dir", "File", "Net", "Exec", "Socket", "Listener", "List", "Option", "Result",
    "Dict", "BuildOut", "BuildRead", "BuildEnv", "BuildNet", "BuildExec",
];

/// Validate that every named type in `t` is known — a builtin, a declared type,
/// or a lowercase generic parameter — so a typo like `fn f(x: Flarb)` is a clear
/// "unknown type" error rather than an opaque type that mis-unifies later.
/// Whether a declared type carries a `frozen` qualifier at the top (`frozen T`,
/// possibly under other stacked qualifiers) — used to enforce that a `frozen`
/// (deeply immutable) value is never declared mutable (RFC-0025).
fn is_frozen_type(t: &ast::Type) -> bool {
    match t {
        ast::Type::Qualified(ast::TypeQual::Frozen, _) => true,
        ast::Type::Qualified(_, inner) => is_frozen_type(inner),
        _ => false,
    }
}

/// Whether a declared type carries a `local unique` qualifier at the top — used to
/// reject it in escaping positions (a return type), per RFC-0026.
fn is_local_unique_type(t: &ast::Type) -> bool {
    match t {
        ast::Type::Qualified(ast::TypeQual::LocalUnique, _) => true,
        ast::Type::Qualified(_, inner) => is_local_unique_type(inner),
        _ => false,
    }
}

fn validate_type(t: &ast::Type, known: &HashSet<&str>) -> Result<(), TypeError> {
    match t {
        ast::Type::Qualified(_, inner) => validate_type(inner, known),
        ast::Type::Tuple(ts) => ts.iter().try_for_each(|x| validate_type(x, known)),
        ast::Type::Fn(params, ret) => {
            params.iter().try_for_each(|p| validate_type(p, known))?;
            validate_type(ret, known)
        }
        ast::Type::Named(n, args) => {
            // `Dir`/`File`/`Net` carry capability *rights* (`Dir[Read]`,
            // `Net[Connect]`) in their arguments, not types — checked elsewhere.
            if n == "Dir" || n == "File" || n == "Net" {
                return Ok(());
            }
            if known.contains(n.as_str()) {
                args.iter().try_for_each(|a| validate_type(a, known))
            } else if args.is_empty() && n.chars().next().is_some_and(|c| c.is_lowercase()) {
                // A lowercase, argument-less name is a generic type parameter.
                Ok(())
            } else {
                terr(format!("unknown type `{n}`"))
            }
        }
    }
}

/// Reject references to undeclared types in function signatures. The
/// set of known names is the builtins plus every `type` declared in the
/// module; lowercase argument-less names are generic parameters.
fn check_type_names(module: &Module) -> Result<(), TypeError> {
    let mut known: HashSet<&str> = BUILTIN_TYPE_NAMES.iter().copied().collect();
    let mut packed_names: HashSet<&str> = HashSet::new();
    for item in &module.items {
        if let Item::Type(t) = item {
            known.insert(t.name.as_str());
            if t.packed {
                packed_names.insert(t.name.as_str());
            }
        }
    }
    for item in &module.items {
        let in_ctx = |e: TypeError, ctx: &str| TypeError {
            message: format!("in `{}`: {}", ctx.rsplit('.').next().unwrap_or(ctx), e.message),
        };
        match item {
            Item::Function(f) => {
                for p in &f.params {
                    if let Some(t) = &p.ty {
                        validate_type(t, &known).map_err(|e| in_ctx(e, &f.name))?;
                        reject_packed_list_boundary(t, &packed_names, &f.name, "a parameter")?;
                    }
                }
                if let Some(t) = &f.ret {
                    validate_type(t, &known).map_err(|e| in_ctx(e, &f.name))?;
                    reject_packed_list_boundary(t, &packed_names, &f.name, "a return type")?;
                    // (RFC-0026) `local unique` is valid only WITHIN the call — it may
                    // not escape — so it cannot be a return type (a return escapes).
                    if is_local_unique_type(t) {
                        return Err(TypeError {
                            message: format!(
                                "`{}`: a `local unique` value cannot escape, so it cannot be a return type — use `unique` (returnable) or drop `local`",
                                f.name
                            ),
                        });
                    }
                }
            }
            // A type's variant field types must also be known. The type's own
            // name (and any sibling type) is already in `known`, so recursive and
            // mutually-recursive types check out; lowercase fields are its params.
            Item::Type(t) => {
                for variant in &t.variants {
                    for field in &variant.fields {
                        validate_type(field, &known).map_err(|e| in_ctx(e, &t.name))?;
                        reject_packed_list_boundary(field, &packed_names, &t.name, "a field")?;
                    }
                }
                // (RFC-0027) A `packed` type's every field must be packable — a
                // scalar (Int/Float/Bool/Duration) or another `packed` type — so the
                // inline layout has a fixed, statically-known size. A variable-size
                // field (String, List, a non-packed record, …) is a check-time error
                // naming the offending field.
                if t.packed {
                    for variant in &t.variants {
                        for (i, field) in variant.fields.iter().enumerate() {
                            if !is_packable_type(field, &packed_names) {
                                let fname = variant
                                    .field_names
                                    .get(i)
                                    .map(String::as_str)
                                    .unwrap_or("(positional field)");
                                return Err(TypeError {
                                    message: format!(
                                        "`packed` type `{}` has a non-packable field `{}`: \
                                         a packed type's fields must be scalars \
                                         (Int/Float/Bool/Duration) or other `packed` types",
                                        t.name, fname
                                    ),
                                });
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// (RFC-0027) Whether `t` is a packable field type for a `packed` layout: a
/// statically-fixed-size scalar (`Int`/`Float`/`Bool`/`Duration`) or another
/// `packed` type. Variable-size types (`String`, `List`, non-packed records, sum
/// types with differing payloads) are not packable.
fn is_packable_type(t: &ast::Type, packed_names: &HashSet<&str>) -> bool {
    matches!(t, ast::Type::Named(n, args)
        if args.is_empty()
            && (matches!(n.as_str(), "Int" | "Float" | "Bool" | "Duration")
                || packed_names.contains(n.as_str())))
}

/// (RFC-0027 declared `packed`) Reject `t` if it contains a `List(P)` where `P` is
/// declared `packed`, in a boundary position (parameter / return / field). A
/// `packed` type's flat inline layout is a CONFINED LOCAL buffer — it has no
/// cross-function or stored-field ABI (that is the deep host-layout change the
/// confined inference deliberately sidesteps), so a `List` of a packed type may
/// only be built and read within one function. This keeps the layout whole-program
/// consistent: every site that handles a packed list either packs it (a confined
/// local) or is a clean compile error here, never a silent boxed fall-back.
fn reject_packed_list_boundary(
    t: &ast::Type,
    packed_names: &HashSet<&str>,
    ctx: &str,
    position: &str,
) -> Result<(), TypeError> {
    if let Some(p) = packed_list_in_type(t, packed_names) {
        return Err(TypeError {
            message: format!(
                "`{ctx}`: a `List({p})` cannot appear in {position} — `{p}` is declared `packed`, so its \
                 list is a confined local buffer with no cross-function or stored layout. Build and read a \
                 `packed` list within one function, or drop `packed` from `{p}` to use the uniform boxed layout"
            ),
        });
    }
    Ok(())
}

/// The name of a declared-`packed` type `P` if `t` contains a `List(P)` anywhere —
/// directly, nested in another list/tuple/function type, or under an ownership
/// qualifier (`frozen`/`unique`). `None` if no packed-typed list appears.
fn packed_list_in_type(t: &ast::Type, packed_names: &HashSet<&str>) -> Option<String> {
    match t {
        ast::Type::Named(n, args) => {
            if n == "List" {
                if let Some(ast::Type::Named(en, eargs)) = args.first().map(ast::Type::unqualified) {
                    if eargs.is_empty() && packed_names.contains(en.as_str()) {
                        return Some(en.clone());
                    }
                }
            }
            args.iter().find_map(|a| packed_list_in_type(a, packed_names))
        }
        ast::Type::Tuple(items) => items.iter().find_map(|a| packed_list_in_type(a, packed_names)),
        ast::Type::Fn(args, ret) => args
            .iter()
            .find_map(|a| packed_list_in_type(a, packed_names))
            .or_else(|| packed_list_in_type(ret, packed_names)),
        ast::Type::Qualified(_, inner) => packed_list_in_type(inner, packed_names),
    }
}

/// Whether `t` names a host capability that `main` may receive as a root
/// authority (the rights of `Dir`/`Net` don't matter here — any are grantable).
pub(crate) fn is_capability_type(t: &ast::Type) -> bool {
    matches!(t, ast::Type::Named(n, _)
        if matches!(n.as_str(), "Console" | "Clock" | "Rand" | "Env" | "Dir" | "File" | "Net" | "Exec" | "Secret" | "SecretStore"))
}

/// (RFC-0047) The kind of an un-comparable component of a type: `==`/`!=` reject
/// function and capability types at every depth (top level or nested inside a
/// container). Returns the FIRST such component found (searching the shared type
/// of an equality's operands), or `None` if the whole type is comparable.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Uncomparable {
    Function,
    Capability,
}

/// Whether `t` (a resolved [`Ty`]) contains a function or capability type at any
/// depth — the two kinds `==` refuses. Containers (List/Tuple/Dict/Result/Option/
/// records-as-Named) are transparent: a `List(fn(Int) -> Int)` is as un-comparable
/// as a bare function. A bare type variable is comparable here (a bounded generic
/// resolves after monomorphization; an unbounded one is caught elsewhere).
fn uncomparable_kind(t: &Ty) -> Option<Uncomparable> {
    match t {
        Ty::Fn(_, _) => Some(Uncomparable::Function),
        Ty::Console | Ty::Clock | Ty::Rand | Ty::Env | Ty::Secret | Ty::Exec | Ty::Socket
        | Ty::Listener | Ty::Dir(_) | Ty::File(_) | Ty::Net(_) | Ty::BuildOut | Ty::BuildRead
        | Ty::BuildEnv | Ty::BuildNet | Ty::BuildExec => Some(Uncomparable::Capability),
        Ty::List(e) => uncomparable_kind(e),
        Ty::Tuple(ts) => ts.iter().find_map(uncomparable_kind),
        Ty::Named(n, args) => {
            // `SecretStore` is a capability the type checker models as a Named type.
            if n == "SecretStore" {
                return Some(Uncomparable::Capability);
            }
            args.iter().find_map(uncomparable_kind)
        }
        _ => None,
    }
}

/// (RFC-0047) Which key/member position a `Float` occupies — used for the teaching
/// error suggesting the right escape.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FloatKeyKind {
    DictKey,
    SetMember,
}

/// Whether a resolved [`Ty`] has a concrete `Float` in a `Dict` KEY position or a
/// `Set` MEMBER position, at any depth (a `Dict(k, Dict(Float, v))` counts). Value
/// positions and list elements are fine — only the hashed/compared key set needs
/// `Eq`. `None` when there is no such Float.
fn float_key_position(t: &Ty) -> Option<FloatKeyKind> {
    match t {
        Ty::Named(n, args) if n == "Dict" => {
            // args[0] = key, args[1] = value. A Float directly as the key is the
            // reject; otherwise recurse into BOTH (a nested dict/set anywhere).
            if matches!(args.first(), Some(Ty::Float)) {
                return Some(FloatKeyKind::DictKey);
            }
            args.iter().find_map(float_key_position)
        }
        Ty::Named(n, args) if n == "Set" => {
            if matches!(args.first(), Some(Ty::Float)) {
                return Some(FloatKeyKind::SetMember);
            }
            args.iter().find_map(float_key_position)
        }
        Ty::Named(_, args) => args.iter().find_map(float_key_position),
        Ty::List(e) => float_key_position(e),
        Ty::Tuple(ts) => ts.iter().find_map(float_key_position),
        Ty::Fn(ps, r) => ps.iter().chain(std::iter::once(r.as_ref())).find_map(float_key_position),
        _ => None,
    }
}

fn float_key_reject_message(kind: FloatKeyKind) -> String {
    match kind {
        FloatKeyKind::DictKey =>
            "`Float` is not a valid `Dict` key — keys require `Eq`, but `Float` is \
             only `PartialEq` (`NaN != NaN`, so a Float key can be unretrievable and \
             `0.1 + 0.2` is a precision trap). Use an `Int` key (scale to a fixed \
             precision) or a `String` rendering of the value"
                .to_string(),
        FloatKeyKind::SetMember =>
            "`Float` is not a valid `Set` member — members require `Eq`, but `Float` \
             is only `PartialEq` (`NaN != NaN`). Use an `Int` (scaled) or a `String` \
             rendering"
                .to_string(),
    }
}

/// The teaching error for an `==`/`!=` on an un-comparable type. `ty` is the whole
/// operand type so the message can point at the container when the offender nests.
fn equality_reject_message(kind: Uncomparable, ty: &Ty) -> String {
    // The offender is nested when the whole operand type is a container (a List,
    // Tuple, or a generic like `Option`/`Result`) rather than the fn/capability
    // itself. `SecretStore` is the one capability spelled as a `Named` type.
    let nested = matches!(ty, Ty::List(_) | Ty::Tuple(_))
        || matches!(ty, Ty::Named(n, _) if n != "SecretStore");
    let where_ = if nested { format!(" (found nested in `{ty}`)") } else { String::new() };
    match kind {
        Uncomparable::Function => format!(
            "`==` is not defined on function types{where_} — there is no meaningful \
             equality for functions (identity is not stable across compilation). \
             Compare the values functions *produce*, not the functions"
        ),
        Uncomparable::Capability => format!(
            "`==` is not defined on capability types{where_} — capabilities are \
             authority, not data; there is no meaningful equality between two \
             authorities"
        ),
    }
}

/// Whether `t` is `List(String)` — the command-line-arguments parameter `main`
/// may declare.
pub fn is_args_type(t: &ast::Type) -> bool {
    matches!(t, ast::Type::Named(n, args)
        if n == "List"
            && matches!(args.as_slice(), [ast::Type::Named(s, inner)] if s == "String" && inner.is_empty()))
}

/// Validate `main`'s signature: every parameter must be a host capability or the
/// `List(String)` args parameter, since `main` is the program's root entrypoint and
/// only the host's granted authority can enter there. Catches at check time what
/// would otherwise be a runtime error when the root capabilities are minted. A
/// module without `main` is a library and passes.
fn check_main_signature(module: &Module) -> Result<(), TypeError> {
    let Some(main) = module.items.iter().find_map(|it| match it {
        Item::Function(f) if f.name == "main" => Some(f),
        _ => None,
    }) else {
        return Ok(());
    };
    // RFC-0038: a library-defined capability marked `grantable` may also enter at
    // the root (the host mints it from a `[user_caps]` grant). Bareness is enforced
    // separately by `check_grantable_caps`, so here we only recognize the name.
    let grantable: std::collections::HashSet<&str> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => Some(t.name.as_str()),
            _ => None,
        })
        .collect();
    let is_grantable_cap = |t: &ast::Type| {
        matches!(t, ast::Type::Named(n, _) if grantable.contains(n.as_str()))
    };
    for p in &main.params {
        if matches!(&p.ty, Some(t) if is_capability_type(t) || is_args_type(t) || is_grantable_cap(t)) {
            continue;
        }
        let found = match &p.ty {
            Some(t) => format!("has type `{}`", witchy_syntax::format::type_str(t)),
            None => "has no type annotation".to_string(),
        };
        return terr(format!(
            "`main` parameter `{}` {found}, but `main` may only take host capabilities \
             (Console, Clock, Env, Dir, Net, Exec, Secret, SecretStore), a `grantable` \
             library capability, or `List(String)` for command-line args",
            p.name
        ));
    }
    // The runtime's value sink surfaces a plain `main` return — an `Int` becomes
    // the process exit code, a `Float`/`String`/… is printed — but it does NOT
    // surface a `Result` or `Option`: their wrapper is dropped, so an `Err`/`None`
    // is silently swallowed (no message, exit stays 0). That makes the Rust
    // `fn main() -> Result(...)` habit a quiet-failure trap. Reject those two
    // specifically and point at handling the outcome in `main`.
    if let Some(ast::Type::Named(n, _)) = &main.ret {
        if n == "Result" || n == "Option" {
            return terr(format!(
                "`main` returns `{}`, but a `Result`/`Option` returned from `main` is \
                 silently discarded — its `Err`/`None` never surfaces and the exit code \
                 stays 0. Handle the outcome inside `main` and return an exit code (or \
                 print it), e.g. `match r: Ok(_) -> 0; Err(e) -> ... ; 1`.",
                witchy_syntax::format::type_str(main.ret.as_ref().unwrap())
            ));
        }
    }
    Ok(())
}

/// RFC-0038: a `grantable capability` may be granted to a root entrypoint, so it
/// must be **bare** — carrying zero transitive built-in host authority. Otherwise
/// granting it would be an invisible `Net`/`Dir`/`Secret` grant (and a later
/// version adding a host-cap field would silently widen root authority with no
/// signature change). Reject any grantable cap that reaches a host capability
/// through its fields, directly or through nested user types.
fn check_grantable_caps(module: &Module) -> Result<(), TypeError> {
    let types: std::collections::HashMap<&str, &ast::TypeDef> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) => Some((t.name.as_str(), t)),
            _ => None,
        })
        .collect();
    for it in &module.items {
        let Item::Type(t) = it else { continue };
        if !t.grantable {
            continue;
        }
        let mut seen = std::collections::HashSet::new();
        if let Some(host) = grantable_host_taint(t, &types, &mut seen) {
            return terr(format!(
                "`grantable capability {}` carries host capability `{host}`; a \
                 root-grantable capability must be BARE (zero transitive host \
                 authority), so it cannot disguise a built-in capability. Construct \
                 it inside the program from an explicit `{host}` root instead.",
                t.name
            ));
        }
    }
    Ok(())
}

/// The first built-in host capability reachable from a type def's fields
/// (transitively through user-type fields), or `None` if the type is bare.
fn grantable_host_taint<'a>(
    t: &'a ast::TypeDef,
    types: &std::collections::HashMap<&'a str, &'a ast::TypeDef>,
    seen: &mut std::collections::HashSet<&'a str>,
) -> Option<String> {
    if !seen.insert(t.name.as_str()) {
        return None; // cycle guard
    }
    for v in &t.variants {
        for fty in &v.fields {
            if let Some(h) = type_host_taint(fty, types, seen) {
                return Some(h);
            }
        }
    }
    None
}

fn type_host_taint<'a>(
    ty: &'a ast::Type,
    types: &std::collections::HashMap<&'a str, &'a ast::TypeDef>,
    seen: &mut std::collections::HashSet<&'a str>,
) -> Option<String> {
    match ty {
        ast::Type::Named(n, args) => {
            if is_capability_type(ty) {
                return Some(n.clone());
            }
            if let Some(inner) = types.get(n.as_str()) {
                if let Some(h) = grantable_host_taint(inner, types, seen) {
                    return Some(h);
                }
            }
            args.iter().find_map(|a| type_host_taint(a, types, seen))
        }
        ast::Type::Qualified(_, inner) => type_host_taint(inner, types, seen),
        ast::Type::Tuple(ts) => ts.iter().find_map(|t| type_host_taint(t, types, seen)),
        ast::Type::Fn(params, ret) => params
            .iter()
            .chain(std::iter::once(ret.as_ref()))
            .find_map(|t| type_host_taint(t, types, seen)),
    }
}

/// (RFC-0040) A cap-gated string export — `pub fn export_*(cap, String) -> String`,
/// a browser app root — must lead with a BARE grantable capability, since the host
/// mints it at that entry (like `main`). Guard the intended-but-wrong shape (a
/// 2-param `[Named, String] -> String` export whose leading type isn't grantable)
/// with a clear error instead of silently not exporting the function.
fn check_export_signatures(module: &Module) -> Result<(), TypeError> {
    let grantable: std::collections::HashSet<&str> = module
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Type(t) if t.grantable => Some(t.name.as_str()),
            _ => None,
        })
        .collect();
    let is_string = |t: &Option<ast::Type>| {
        matches!(t, Some(ast::Type::Named(n, a)) if n == "String" && a.is_empty())
    };
    for it in &module.items {
        let Item::Function(f) = it else { continue };
        if !f.public {
            continue;
        }
        let unqual = f.name.rsplit('.').next().unwrap_or(&f.name);
        if !unqual.starts_with("export_") {
            continue;
        }
        if let [p0, p1] = f.params.as_slice() {
            let ret_string = matches!(&f.ret, Some(ast::Type::Named(n, _)) if n == "String");
            if is_string(&p1.ty) && ret_string {
                if let Some(ast::Type::Named(n, _)) = &p0.ty {
                    if n != "String" && !grantable.contains(n.as_str()) {
                        return terr(format!(
                            "the leading parameter of the exported entrypoint `{unqual}` is `{n}`, but a \
                             cap-gated export may lead only with a bare `grantable capability` (RFC-0040) — \
                             `{n}` is not grantable"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Validate a rune's build entrypoint. The build step is the top-level `fn build`
/// whose first parameter is `BuildOut`; it runs in a zero-ambient build sandbox,
/// so it may take *only* build-time capabilities — a runtime capability (or
/// anything else) in its signature is an error. A `build` function that does not
/// take `BuildOut` is treated as an ordinary function, not the entrypoint, so
/// existing code that happens to define `fn build(...)` is unaffected.
fn check_build_signature(module: &Module) -> Result<(), TypeError> {
    let Some(build) = build_entrypoint(module) else {
        return Ok(());
    };
    for p in &build.params {
        if matches!(&p.ty, Some(t) if is_build_capability_type(t)) {
            continue;
        }
        let found = match &p.ty {
            Some(t) => format!("has type `{}`", witchy_syntax::format::type_str(t)),
            None => "has no type annotation".to_string(),
        };
        return terr(format!(
            "`build` parameter `{}` {found}, but a build step may only take build-time \
             capabilities (BuildOut, BuildRead, BuildEnv, BuildNet, BuildExec)",
            p.name
        ));
    }
    Ok(())
}

/// Collect the type-parameter names (lowercase, argument-less) appearing in a
/// type expression, in order of first appearance. Used to infer the parameters
/// of a generic ADT from its variant field types.
/// A `let`-borrowed parameter may not BE the function's result: every block
/// tail and `return` expression is checked for the bare parameter (through
/// if/match/block tails). Everything else copies by value semantics, so this
/// is the whole escape surface — previously enforced only by the native
/// backend's borrow checker, now a language rule.
fn borrow_escape_check(func: &Function) -> Result<(), TypeError> {
    let borrowed: Vec<&str> = func
        .params
        .iter()
        .filter(|p| p.convention == Convention::Borrow)
        .map(|p| p.name.as_str())
        .collect();
    if borrowed.is_empty() {
        return Ok(());
    }
    fn check_result_expr(e: &Expr, borrowed: &[&str], fname: &str) -> Result<(), TypeError> {
        match e {
            Expr::Var(v) if borrowed.contains(&v.as_str()) => terr(format!(
                "in `{fname}`: the `let`-borrowed parameter `{v}` cannot be returned — a borrow must not outlive the call (drop the `let`, or take it `own`)"
            )),
            Expr::If { then_block, else_block, .. } => {
                check_result_block(then_block, borrowed, fname)?;
                if let Some(b) = else_block {
                    check_result_block(b, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::Match { arms, .. } => {
                for arm in arms {
                    check_result_expr(&arm.body, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::Block(b) => check_result_block(b, borrowed, fname),
            _ => Ok(()),
        }
    }
    fn check_result_block(b: &Block, borrowed: &[&str], fname: &str) -> Result<(), TypeError> {
        if let Some(Stmt::Expr(tail)) = b.stmts.last() {
            check_result_expr(tail, borrowed, fname)?;
        }
        Ok(())
    }
    // Every `return` expression, anywhere in the body.
    fn scan_returns_block(b: &Block, borrowed: &[&str], fname: &str) -> Result<(), TypeError> {
        for stmt in &b.stmts {
            match stmt {
                Stmt::Return(Some(e)) => check_result_expr(e, borrowed, fname)?,
                Stmt::Let { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::LetTuple { value, .. }
                | Stmt::Expr(value)
                | Stmt::Yield(value) => scan_returns_expr(value, borrowed, fname)?,
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        Ok(())
    }
    fn scan_returns_expr(e: &Expr, borrowed: &[&str], fname: &str) -> Result<(), TypeError> {
        match e {
            Expr::If { cond, then_block, else_block } => {
                scan_returns_expr(cond, borrowed, fname)?;
                scan_returns_block(then_block, borrowed, fname)?;
                if let Some(b) = else_block {
                    scan_returns_block(b, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::Match { scrutinee, arms } => {
                scan_returns_expr(scrutinee, borrowed, fname)?;
                for arm in arms {
                    scan_returns_expr(&arm.body, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::While { cond, body } => {
                scan_returns_expr(cond, borrowed, fname)?;
                scan_returns_block(body, borrowed, fname)
            }
            Expr::For { iter, body, .. } => {
                scan_returns_expr(iter, borrowed, fname)?;
                scan_returns_block(body, borrowed, fname)
            }
            Expr::Block(b) => scan_returns_block(b, borrowed, fname),
            Expr::Binary { lhs, rhs, .. } => {
                scan_returns_expr(lhs, borrowed, fname)?;
                scan_returns_expr(rhs, borrowed, fname)
            }
            Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::As { expr, .. } => {
                scan_returns_expr(expr, borrowed, fname)
            }
            Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::List(args)
            | Expr::Tuple(args) => {
                for a in args {
                    scan_returns_expr(a, borrowed, fname)?;
                }
                Ok(())
            }
            Expr::Apply { func: f2, args } => {
                scan_returns_expr(f2, borrowed, fname)?;
                for a in args {
                    scan_returns_expr(a, borrowed, fname)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    check_result_block(&func.body, &borrowed, &func.name)?;
    scan_returns_block(&func.body, &borrowed, &func.name)
}

/// Types a region assignment may freely write through to the outer scope:
/// copied by value, never pointer-backed.
fn is_scalar_ty(t: &Ty) -> bool {
    matches!(t, Ty::Int | Ty::Float | Ty::Bool | Ty::Duration)
}

fn collect_type_params(t: &ast::Type, acc: &mut Vec<String>) {
    match t {
        ast::Type::Qualified(_, inner) => collect_type_params(inner, acc),
        ast::Type::Tuple(ts) => {
            for x in ts {
                collect_type_params(x, acc);
            }
        }
        ast::Type::Fn(params, ret) => {
            for p in params {
                collect_type_params(p, acc);
            }
            collect_type_params(ret, acc);
        }
        ast::Type::Named(name, args) => {
            if args.is_empty() && name.chars().next().is_some_and(|c| c.is_lowercase()) {
                if !acc.contains(name) {
                    acc.push(name.clone());
                }
            } else {
                for a in args {
                    collect_type_params(a, acc);
                }
            }
        }
    }
}

/// A record type's layout: its type-parameter var ids (in order) and its fields
/// as `(name, type)`. Field types may mention the parameters, instantiated with
/// the value's actual type arguments on access.
type RecordInfo = (Vec<u32>, Vec<(String, Ty)>);

struct Checker {
    /// When annotating (see `annotate`): expression identity -> inferred type,
    /// finalized against the ending substitution. Key = `&Expr as *const _`.
    type_record: Option<HashMap<usize, Ty>>,
    fn_sigs: HashMap<String, (Vec<Ty>, Ty)>,
    ctor_sigs: HashMap<String, (Vec<Ty>, Ty)>,
    /// Type-parameter var ids per constructor, so a generic ADT's constructors
    /// are instantiated fresh at each use (e.g. `Some(1)` vs `Some("x")`).
    ctor_typarams: HashMap<String, HashSet<u32>>,
    /// Record types: name -> (type-parameter var ids in order, fields). A field
    /// type may mention the parameters, which are instantiated with the value's
    /// actual type arguments on access.
    record_fields: HashMap<String, RecordInfo>,
    /// Sealed record capabilities (`capability X:` with named fields). Their
    /// fields are opaque: `.field` access is rejected so the only way to reach a
    /// carried capability is `match`, which the linker confines to the home
    /// module — otherwise an alias would leak the underlying authority.
    sealed_types: HashSet<String>,
    adt_variants: HashMap<String, Vec<String>>,
    fn_conventions: HashMap<String, Vec<Convention>>,
    /// Per-function type parameters (name, var id), from lowercase type names in
    /// signatures. Generalized: instantiated fresh at each call site.
    fn_typarams: HashMap<String, Vec<(String, u32)>>,
    subst: HashMap<u32, Ty>,
    next_var: u32,
    /// Each binding carries its type and whether it is mutable.
    scopes: Vec<HashMap<String, (Ty, bool)>>,
    /// Bindings that have been consumed (moved out via an `own` parameter) and
    /// may not be used again until reassigned. Flow-sensitive within a body.
    consumed: HashSet<String>,
    /// One entry per ACTIVE `region:` block, holding the names declared
    /// inside it — an assignment to a name outside the innermost region must
    /// be scalar (a region's only pointer-escape is its value).
    region_locals: Vec<HashSet<String>>,
    /// The declared return type of the function currently being checked, so `?`
    /// can require the enclosing function to return a matching Result/Option.
    current_ret: Option<Ty>,
    /// Source line of the statement currently being checked, attached to errors
    /// so diagnostics point at a location. 0 means "no line known".
    cur_line: u32,
}

impl Checker {
    fn fresh(&mut self) -> Ty {
        let v = self.next_var;
        self.next_var += 1;
        Ty::Var(v)
    }

    // `&mut self` because resolving an unknown type can mint fresh type vars via
    // `self.fresh()`, despite the `to_` name.
    #[allow(clippy::wrong_self_convention)]
    fn to_ty(&mut self, t: &ast::Type) -> Ty {
        let (name, args) = match t {
            // (RFC-0025/0026) Ownership/immutability qualifiers are compile-time
            // contracts with no runtime type — lower to the inner type.
            ast::Type::Qualified(_, inner) => return self.to_ty(inner),
            ast::Type::Named(name, args) => (name, args),
            ast::Type::Tuple(ts) => {
                return Ty::Tuple(ts.iter().map(|t| self.to_ty(t)).collect());
            }
            ast::Type::Fn(params, ret) => {
                return Ty::Fn(
                    params.iter().map(|t| self.to_ty(t)).collect(),
                    Box::new(self.to_ty(ret)),
                );
            }
        };
        match name.as_str() {
            "Int" => Ty::Int,
            "Float" => Ty::Float,
            "Duration" => Ty::Duration,
            "String" => Ty::String,
            "Bytes" => Ty::Bytes,
            "Bool" => Ty::Bool,
            "Nil" => Ty::Nil,
            "Console" => Ty::Console,
            "Clock" => Ty::Clock,
            "Rand" => Ty::Rand,
            "Env" => Ty::Env,
            "Secret" => Ty::Secret,
            "Exec" => Ty::Exec,
            "Dir" => Ty::Dir(dir_rights(args)),
            "File" => Ty::File(file_rights(args)),
            "Net" => Ty::Net(net_rights(args)),
            "Socket" => Ty::Socket,
            "Listener" => Ty::Listener,
            "BuildOut" => Ty::BuildOut,
            "BuildRead" => Ty::BuildRead,
            "BuildEnv" => Ty::BuildEnv,
            "BuildNet" => Ty::BuildNet,
            "BuildExec" => Ty::BuildExec,
            "List" => {
                let elem = match args.first() {
                    Some(a) => self.to_ty(a),
                    None => self.fresh(),
                };
                Ty::List(Box::new(elem))
            }
            _ => Ty::Named(name.clone(), args.iter().map(|a| self.to_ty(a)).collect()),
        }
    }

    /// Like `to_ty`, but a lowercase, argument-less type name becomes a type
    /// *variable* (a parameter), shared within one signature via `vars`.
    #[allow(clippy::wrong_self_convention)]
    fn to_ty_generic(&mut self, t: &ast::Type, vars: &mut HashMap<String, Ty>) -> Ty {
        match t {
            ast::Type::Qualified(_, inner) => self.to_ty_generic(inner, vars),
            ast::Type::Tuple(ts) => {
                Ty::Tuple(ts.iter().map(|t| self.to_ty_generic(t, vars)).collect())
            }
            ast::Type::Fn(params, ret) => Ty::Fn(
                params.iter().map(|t| self.to_ty_generic(t, vars)).collect(),
                Box::new(self.to_ty_generic(ret, vars)),
            ),
            ast::Type::Named(name, args) => match name.as_str() {
                "Int" => Ty::Int,
                "Float" => Ty::Float,
                "Duration" => Ty::Duration,
                "String" => Ty::String,
            "Bytes" => Ty::Bytes,
                "Bool" => Ty::Bool,
                "Nil" => Ty::Nil,
                "Console" => Ty::Console,
            "Clock" => Ty::Clock,
            "Rand" => Ty::Rand,
            "Env" => Ty::Env,
            "Secret" => Ty::Secret,
                "Exec" => Ty::Exec,
                "Dir" => Ty::Dir(dir_rights(args)),
                "File" => Ty::File(file_rights(args)),
                "Net" => Ty::Net(net_rights(args)),
                "Socket" => Ty::Socket,
                "Listener" => Ty::Listener,
                "BuildOut" => Ty::BuildOut,
                "BuildRead" => Ty::BuildRead,
                "BuildEnv" => Ty::BuildEnv,
                "BuildNet" => Ty::BuildNet,
                "BuildExec" => Ty::BuildExec,
                "List" => {
                    let elem = match args.first() {
                        Some(a) => self.to_ty_generic(a, vars),
                        None => self.fresh(),
                    };
                    Ty::List(Box::new(elem))
                }
                other
                    if args.is_empty()
                        && other.chars().next().is_some_and(|c| c.is_lowercase()) =>
                {
                    if let Some(v) = vars.get(other) {
                        v.clone()
                    } else {
                        let v = self.fresh();
                        vars.insert(other.to_string(), v.clone());
                        v
                    }
                }
                other => Ty::Named(
                    other.to_string(),
                    args.iter().map(|a| self.to_ty_generic(a, vars)).collect(),
                ),
            },
        }
    }

    /// Instantiate a polymorphic signature: replace its generalized type
    /// parameters with fresh vars, so each call site is independent. Other
    /// (inference) vars stay shared, keeping un-annotated functions monomorphic.
    fn instantiate(&mut self, params: &[Ty], ret: &Ty, typarams: &HashSet<u32>) -> (Vec<Ty>, Ty) {
        let mut fresh_map: HashMap<u32, Ty> = HashMap::new();
        for &v in typarams {
            // Checking the function's body may have *bound* the type-param var to
            // another (still-unbound) var — e.g. matching on the param. Key the
            // fresh substitution by that resolved representative, since
            // `subst_vars` resolves before it looks up the map; otherwise the
            // substitution would never apply and the function would behave
            // monomorphically across call sites. A param resolved to a concrete
            // type isn't generic, so skip it.
            if let Ty::Var(rv) = self.resolve(&Ty::Var(v)) {
                fresh_map.entry(rv).or_insert_with(|| self.fresh());
            }
        }
        let p = params.iter().map(|t| self.subst_vars(t, &fresh_map)).collect();
        let r = self.subst_vars(ret, &fresh_map);
        (p, r)
    }

    fn subst_vars(&self, t: &Ty, map: &HashMap<u32, Ty>) -> Ty {
        match self.resolve(t) {
            Ty::Var(v) => map.get(&v).cloned().unwrap_or(Ty::Var(v)),
            Ty::List(e) => Ty::List(Box::new(self.subst_vars(&e, map))),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|x| self.subst_vars(x, map)).collect()),
            Ty::Named(n, args) => {
                Ty::Named(n, args.iter().map(|x| self.subst_vars(x, map)).collect())
            }
            Ty::Fn(params, ret) => Ty::Fn(
                params.iter().map(|x| self.subst_vars(x, map)).collect(),
                Box::new(self.subst_vars(&ret, map)),
            ),
            other => other,
        }
    }

    fn resolve(&self, t: &Ty) -> Ty {
        match t {
            Ty::Var(v) => match self.subst.get(v) {
                Some(bound) => self.resolve(bound),
                None => t.clone(),
            },
            Ty::List(e) => Ty::List(Box::new(self.resolve(e))),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| self.resolve(t)).collect()),
            Ty::Named(n, args) => Ty::Named(n.clone(), args.iter().map(|t| self.resolve(t)).collect()),
            Ty::Fn(params, ret) => Ty::Fn(
                params.iter().map(|t| self.resolve(t)).collect(),
                Box::new(self.resolve(ret)),
            ),
            _ => t.clone(),
        }
    }

    fn unify(&mut self, a: &Ty, b: &Ty) -> Result<(), TypeError> {
        let a = self.resolve(a);
        let b = self.resolve(b);
        match (&a, &b) {
            (Ty::Var(x), Ty::Var(y)) if x == y => Ok(()),
            (Ty::Var(x), other) | (other, Ty::Var(x)) => {
                // Occurs check: binding x := T where x appears inside T would
                // build an infinite type (e.g. unifying `a` with `List(a)`).
                if self.occurs(*x, other) {
                    return terr(format!(
                        "cannot construct the infinite type: a type variable occurs within `{other}`"
                    ));
                }
                self.subst.insert(*x, other.clone());
                Ok(())
            }
            (Ty::List(x), Ty::List(y)) => self.unify(x, y),
            (Ty::Tuple(xs), Ty::Tuple(ys)) if xs.len() == ys.len() => {
                for (x, y) in xs.iter().zip(ys) {
                    self.unify(x, y)?;
                }
                Ok(())
            }
            (Ty::Named(x, xa), Ty::Named(y, ya)) if x == y && xa.len() == ya.len() => {
                for (p, q) in xa.iter().zip(ya) {
                    self.unify(p, q)?;
                }
                Ok(())
            }
            (Ty::Fn(xp, xr), Ty::Fn(yp, yr)) if xp.len() == yp.len() => {
                for (p, q) in xp.iter().zip(yp) {
                    self.unify(p, q)?;
                }
                self.unify(xr, yr)
            }
            _ if a == b => Ok(()),
            _ => terr(format!("expected `{a}`, found `{b}`")),
        }
    }

    /// Whether type variable `x` occurs anywhere inside `t` (after resolving
    /// intermediate variables) — the standard infinite-type guard.
    fn occurs(&self, x: u32, t: &Ty) -> bool {
        match self.resolve(t) {
            Ty::Var(y) => x == y,
            Ty::List(inner) => self.occurs(x, &inner),
            Ty::Tuple(items) => items.iter().any(|i| self.occurs(x, i)),
            Ty::Named(_, args) => args.iter().any(|a| self.occurs(x, a)),
            Ty::Fn(params, ret) => {
                params.iter().any(|p| self.occurs(x, p)) || self.occurs(x, &ret)
            }
            _ => false,
        }
    }

    // --- scope helpers ---
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
    }
    fn define(&mut self, name: String, ty: Ty, mutable: bool) {
        if let Some(r) = self.region_locals.last_mut() {
            r.insert(name.clone());
        }
        self.scopes.last_mut().unwrap().insert(name, (ty, mutable));
    }
    /// Walk frames inner→outer, returning the binding at the first frame that
    /// defines the name (inner bindings shadow outer ones).
    fn resolve_binding(&self, name: &str) -> Option<&(Ty, bool)> {
        for vars in self.scopes.iter().rev() {
            if let Some(b) = vars.get(name) {
                return Some(b);
            }
        }
        None
    }
    fn lookup(&self, name: &str) -> Option<Ty> {
        self.resolve_binding(name).map(|(t, _)| t.clone())
    }
    fn is_mutable(&self, name: &str) -> Option<bool> {
        self.resolve_binding(name).map(|(_, m)| *m)
    }

    fn call_sig(&mut self, name: &str) -> Option<(Vec<Ty>, Ty)> {
        match name {
            "print" => Some((vec![Ty::Console, Ty::String], Ty::Nil)),
            "now" => Some((vec![Ty::Clock], Ty::Int)),
            // Monotonic elapsed nanoseconds (a steady clock, immune to wall-clock
            // jumps) — for measuring durations. Like `now`, the Clock arg is the
            // authority; reading it is ambient nondeterminism.
            "now_monotonic" => Some((vec![Ty::Clock], Ty::Int)),
            "rand_u64" => Some((vec![Ty::Rand], Ty::Int)),
            "get_env" => Some((vec![Ty::Env, Ty::String], Ty::Named("Option".into(), vec![Ty::String]))),
            // Build-time host operations (the build sandbox provides these). Each
            // consumes a build capability; the specific tool/dir/host/var is the
            // consumer's grant, not part of the type.
            "write_out" => Some((vec![Ty::BuildOut, Ty::String, Ty::String], Ty::Nil)),
            "read_build" => Some((vec![Ty::BuildRead, Ty::String], Ty::String)),
            "get_build_env" => {
                Some((vec![Ty::BuildEnv, Ty::String], Ty::Named("Option".into(), vec![Ty::String])))
            }
            "fetch_build" => Some((vec![Ty::BuildNet, Ty::String, Ty::String], Ty::String)),
            "run_tool" => Some((vec![Ty::BuildExec, Ty::String, Ty::String], Ty::String)),
            "string.length" => Some((vec![Ty::String], Ty::Int)),
            "string.char_count" => Some((vec![Ty::String], Ty::Int)),
            // (Bytes) Primitive intrinsics behind the `std/bytes` surface. `Bytes` and
            // `String` share the flat `[len][bytes]` layout, so the representation-level
            // ops are identity/reuse on the compiled backend.
            "__bytes_from_string" => Some((vec![Ty::String], Ty::Bytes)),
            "__bytes_to_string" => Some((vec![Ty::Bytes], Ty::String)),
            "__bytes_length" => Some((vec![Ty::Bytes], Ty::Int)),
            "__bytes_at" => Some((vec![Ty::Bytes, Ty::Int], Ty::Int)),
            "__bytes_concat" => Some((vec![Ty::Bytes, Ty::Bytes], Ty::Bytes)),
            "__bytes_slice" => Some((vec![Ty::Bytes, Ty::Int, Ty::Int], Ty::Bytes)),
            "string.to_upper" | "string.to_lower" | "string.trim" => Some((vec![Ty::String], Ty::String)),
            // Abort with a message (the primitive behind std/testing).
            "fail" => Some((vec![Ty::String], Ty::Nil)),
            "string.starts_with" | "string.contains" | "string.ends_with" => {
                Some((vec![Ty::String, Ty::String], Ty::Bool))
            }
            "string.index_of" => Some((vec![Ty::String, Ty::String], Ty::Int)),
            "string.split" => Some((
                vec![Ty::String, Ty::String],
                Ty::List(Box::new(Ty::String)),
            )),
            // The characters of a string, each as a single-char String — built in
            // one O(n) pass (unlike a char_at loop), so callers can index chars in
            // O(1). The primitive behind a fast `std/string.to_chars`.
            "string.chars" => Some((vec![Ty::String], Ty::List(Box::new(Ty::String)))),
            "string.replace" => Some((vec![Ty::String, Ty::String, Ty::String], Ty::String)),
            "string.substring" => Some((vec![Ty::String, Ty::Int, Ty::Int], Ty::String)),
            "string.from_code" => Some((vec![Ty::Int], Ty::String)),
            "math.to_float" => Some((vec![Ty::Int], Ty::Float)),
            "math.to_int" => Some((vec![Ty::Float], Ty::Int)),
            // Duration <-> Int(milliseconds) bridge for the std `duration` module.
            "int_to_duration" => Some((vec![Ty::Int], Ty::Duration)),
            "duration_to_int" => Some((vec![Ty::Duration], Ty::Int)),
            "math.sqrt" => Some((vec![Ty::Float], Ty::Float)),
            "string.to_int" => Some((vec![Ty::String], Ty::Int)),
            "__render" => {
                let a = self.fresh();
                Some((vec![a], Ty::String))
            }
            "list.length" => {
                let elem = self.fresh();
                Some((vec![Ty::List(Box::new(elem))], Ty::Int))
            }
            "list.at" => {
                let elem = self.fresh();
                Some((vec![Ty::List(Box::new(elem.clone())), Ty::Int], elem))
            }
            "list.push" => {
                let elem = self.fresh();
                Some((
                    vec![Ty::List(Box::new(elem.clone())), elem.clone()],
                    Ty::List(Box::new(elem)),
                ))
            }
            "list.concat" => {
                let elem = self.fresh();
                let list = Ty::List(Box::new(elem));
                Some((vec![list.clone(), list.clone()], list))
            }
            // Dict(k, v) is an ordinary parameterized Named type; these builtins
            // are generic in its key and value types.
            "dict.new" => {
                let k = self.fresh();
                let v = self.fresh();
                Some((vec![], Ty::Named("Dict".into(), vec![k, v])))
            }
            "dict.insert" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v.clone()]);
                Some((vec![d.clone(), k, v], d))
            }
            "dict.get_or" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v.clone()]);
                Some((vec![d, k, v.clone()], v))
            }
            // dict.update(dict, key, default, f) -> dict: a single-lookup upsert. `f`
            // maps the current value (or `default` when the key is absent) to the
            // new value — like Go's `m[k]++` in one operation.
            "dict.update" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v.clone()]);
                let f = Ty::Fn(vec![v.clone()], Box::new(v.clone()));
                Some((vec![d.clone(), k, v, f], d))
            }
            "dict.contains_key" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v]);
                Some((vec![d, k], Ty::Bool))
            }
            "dict.remove" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v]);
                Some((vec![d.clone(), k], d))
            }
            "dict.keys" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v]);
                Some((vec![d], Ty::List(Box::new(k))))
            }
            "dict.values" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k, v.clone()]);
                Some((vec![d], Ty::List(Box::new(v))))
            }
            "dict.pairs" => {
                let k = self.fresh();
                let v = self.fresh();
                let d = Ty::Named("Dict".into(), vec![k.clone(), v.clone()]);
                Some((vec![d], Ty::List(Box::new(Ty::Tuple(vec![k, v])))))
            }
            "dict.length" => {
                let k = self.fresh();
                let v = self.fresh();
                Some((vec![Ty::Named("Dict".into(), vec![k, v])], Ty::Int))
            }
            // `read`/`write`/`exists`/`subdir`/`read_only`/`write_only` are handled
            // by `check_dir_op`; `connect`/`listen`/`restrict`/`connect_only`/
            // `listen_only` by `check_net_op` (their rights are enforced per-op).
            "send_line" => Some((vec![Ty::Socket, Ty::String], Ty::Nil)),
            "send_bytes" => Some((vec![Ty::Socket, Ty::String], Ty::Nil)),
            "recv_line" => Some((vec![Ty::Socket], Ty::String)),
            "recv_all" => Some((vec![Ty::Socket], Ty::String)),
            "recv_bytes" => Some((vec![Ty::Socket, Ty::Int], Ty::String)),
            "accept" => Some((vec![Ty::Listener], Ty::Socket)),
            // (RFC-0032) Spin up the `server.serve` worker pool over a bound listener.
            "serve_pool" => Some((vec![Ty::Listener], Ty::Nil)),
            "close" => Some((vec![Ty::Socket], Ty::Nil)),
            // User functions: instantiate generic type parameters fresh per call.
            _ => match self.fn_sigs.get(name).cloned() {
                Some((params, ret)) => {
                    let typarams: HashSet<u32> = self
                        .fn_typarams
                        .get(name)
                        .into_iter()
                        .flatten()
                        .map(|(_, id)| *id)
                        .collect();
                    Some(self.instantiate(&params, &ret, &typarams))
                }
                None => None,
            },
        }
    }

    /// Resolve a call's first argument as a `Dir` capability and yield its rights.
    /// An unconstrained variable defaults to the full right-set (bare `Dir`).
    fn dir_cap_rights(&mut self, name: &str, arg: &Expr) -> Result<DirRights, TypeError> {
        let cap = self.infer(arg)?;
        match self.resolve(&cap) {
            Ty::Dir(r) => Ok(r),
            Ty::Var(_) => {
                self.unify(&cap, &Ty::Dir(DirRights::full()))?;
                Ok(DirRights::full())
            }
            other => terr(format!(
                "`{name}` expects a `Dir` capability but got `{other}`"
            )),
        }
    }

    /// Resolve a call's first argument as a `File` capability and yield its rights.
    /// An unconstrained variable defaults to the full right-set (bare `File`).
    fn file_cap_rights(&mut self, name: &str, arg: &Expr) -> Result<FileRights, TypeError> {
        let cap = self.infer(arg)?;
        match self.resolve(&cap) {
            Ty::File(r) => Ok(r),
            Ty::Var(_) => {
                self.unify(&cap, &Ty::File(FileRights::full()))?;
                Ok(FileRights::full())
            }
            other => terr(format!(
                "`{name}` expects a `File` capability but got `{other}`"
            )),
        }
    }

    /// Type-check a file-capability op (RFC-0012). A `File` is a leaf, so its ops
    /// take no path: `read(f: File[Read]) -> String` (arity 1) and
    /// `write(f: File[Write], data) -> Nil` (arity 2). Returns `Ok(None)` when the
    /// name/arity isn't a File op, so the Dir forms (`read(dir, path)` etc.) fall
    /// through — `read`/`write` are disambiguated from `Dir` by arity.
    fn check_file_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        let arity = match name {
            "read" => 1,
            "write" => 2,
            _ => return Ok(None),
        };
        if args.len() != arity {
            return Ok(None);
        }
        let rights = self.file_cap_rights(name, &args[0])?;
        for arg in &args[1..] {
            let at = self.infer(arg)?;
            self.unify(&Ty::String, &at).map_err(|e| TypeError {
                message: format!("in call to `{name}`: {}", e.message),
            })?;
        }
        let ret = match name {
            "read" => {
                if !rights.read {
                    return terr(format!("`read` needs `Read` but the file is `{rights}`"));
                }
                Ty::String
            }
            "write" => {
                if !rights.write {
                    return terr(format!("`write` needs `Write` but the file is `{rights}`"));
                }
                Ty::Nil
            }
            _ => unreachable!(),
        };
        Ok(Some(ret))
    }

    /// Type-check a directory-capability op, enforcing that the `Dir`'s rights
    /// permit the verb: `read`/`exists`/`subdir`/`list` need `Read`; `write`/
    /// `append`/`make_dir` need `Write`. (Narrowing is done with the `as`
    /// ascription, not per-op builtins.) Returns `Ok(None)` when `name` is not
    /// a Dir op.
    /// `__try_ctx(value, msg)` — the `e ? "msg"` desugar. Generic over the operand:
    /// `Option(T)` or `Result(T, String)`, both yielding `Result(T, String)` so the
    /// enclosing `?` unwraps `T` and propagates an `Err(String)`. The message is a
    /// `String`; a `Result`'s error must already be `String` (the message is
    /// prepended to it, so it stays `String`).
    fn check_try_ctx(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        if name != "__try_ctx" {
            return Ok(None);
        }
        if args.len() != 2 {
            return terr(format!("`? \"msg\"` expects (value, message) but got {}", args.len()));
        }
        let mty = self.infer(&args[1])?;
        self.unify(&Ty::String, &mty).map_err(|e| TypeError {
            message: format!("a `? \"msg\"` context message must be a String: {}", e.message),
        })?;
        let oty = self.infer(&args[0])?;
        let elem = self.fresh();
        let resolved = self.resolve(&oty);
        match &resolved {
            Ty::Named(n, _) if n == "Option" => {
                self.unify(&Ty::Named("Option".into(), vec![elem.clone()]), &oty).map_err(|e| {
                    TypeError { message: format!("in `? \"msg\"`: {}", e.message) }
                })?;
            }
            Ty::Named(n, _) if n == "Result" => {
                self.unify(
                    &Ty::Named("Result".into(), vec![elem.clone(), Ty::String]),
                    &oty,
                )
                .map_err(|_| TypeError {
                    message: "`? \"msg\"` prepends to a String error, so the `Result`'s error type must be `String`".to_string(),
                })?;
            }
            other => {
                return terr(format!(
                    "`? \"msg\"` applies to an `Option` or `Result`, not `{other}`"
                ));
            }
        }
        Ok(Some(Ty::Named("Result".into(), vec![elem, Ty::String])))
    }

    fn check_dir_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        // RFC-0011: `only` is polymorphic — `dir.only(DirPolicy)` narrows a Dir's
        // entry policy (handled here); a `Net` receiver defers to `check_net_op`.
        if name == "only" {
            if args.len() != 2 {
                return terr(format!("`only` expects 2 argument(s) but got {}", args.len()));
            }
            let recv = self.infer(&args[0])?;
            let Ty::Dir(rights) = self.resolve(&recv) else {
                return Ok(None); // not a Dir.only — let check_net_op handle Net.only
            };
            if !rights.read {
                return terr(format!("`only` needs `Read` but the capability is `{rights}`"));
            }
            let pt = self.infer(&args[1])?;
            self.unify(&Ty::Named("DirPolicy".into(), Vec::new()), &pt)
                .map_err(|e| TypeError { message: format!("in call to `only`: {}", e.message) })?;
            return Ok(Some(Ty::Dir(rights)));
        }
        let arity = match name {
            "list" => 1,
            "read" | "exists" | "is_dir" | "subtree" | "make_dir" | "read_file" | "write_file" => 2,
            "write" | "append" => 3,
            _ => return Ok(None),
        };
        if args.len() != arity {
            return terr(format!(
                "`{name}` expects {arity} argument(s) but got {}",
                args.len()
            ));
        }
        let rights = self.dir_cap_rights(name, &args[0])?;
        // The trailing arguments (path, and for `write` the content) are strings.
        for arg in &args[1..] {
            let at = self.infer(arg)?;
            self.unify(&Ty::String, &at).map_err(|e| TypeError {
                message: format!("in call to `{name}`: {}", e.message),
            })?;
        }
        let ret = match name {
            "read" => {
                if !rights.read {
                    return terr(format!("`read` needs `Read` but the capability is `{rights}`"));
                }
                Ty::String
            }
            "exists" => {
                if !rights.read {
                    return terr(format!(
                        "`exists` needs `Read` but the capability is `{rights}`"
                    ));
                }
                Ty::Bool
            }
            "is_dir" => {
                if !rights.read {
                    return terr(format!(
                        "`is_dir` needs `Read` but the capability is `{rights}`"
                    ));
                }
                Ty::Bool
            }
            "subtree" => {
                if !rights.read {
                    return terr(format!(
                        "`{name}` needs `Read` but the capability is `{rights}`"
                    ));
                }
                Ty::Dir(rights)
            }
            "list" => {
                if !rights.read {
                    return terr(format!("`list` needs `Read` but the capability is `{rights}`"));
                }
                Ty::List(Box::new(Ty::String))
            }
            "write" | "append" => {
                if !rights.write {
                    return terr(format!(
                        "`{name}` needs `Write` but the capability is `{rights}`"
                    ));
                }
                Ty::Nil
            }
            "make_dir" => {
                if !rights.write {
                    return terr(format!(
                        "`make_dir` needs `Write` but the capability is `{rights}`"
                    ));
                }
                Ty::Nil
            }
            // RFC-0012 navigation: a `Dir` opens a confined `File` (the leaf). The
            // name states the conferred right: `read_file` needs `Read` and yields
            // `File[Read]`; `write_file` needs `Write` and yields `File[Write]`.
            "read_file" => {
                if !rights.read {
                    return terr(format!(
                        "`read_file` needs `Read` but the capability is `{rights}`"
                    ));
                }
                Ty::File(FileRights { read: true, write: false })
            }
            "write_file" => {
                if !rights.write {
                    return terr(format!(
                        "`write_file` needs `Write` but the capability is `{rights}`"
                    ));
                }
                Ty::File(FileRights { read: false, write: true })
            }
            _ => unreachable!(),
        };
        Ok(Some(ret))
    }

    /// Type-check the low-level `exec` op:
    /// `exec(exec, dir, path, args, stdin) -> String`.
    /// `Exec` is the right to spawn a subprocess; the executable is named through a
    /// `Dir[Read]` (the same confinement as `read`), so you can only run a file you
    /// can read. `args` is a single `\0`-joined argv string and the result is a
    /// `"<exit_code>\n<output>"` payload — the std `exec` module wraps this as
    /// `(Int, String)` over a `List(String)`. Returns `Ok(None)` when `name` is not
    /// `exec`.
    fn check_exec_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        if name != "exec" {
            return Ok(None);
        }
        if args.len() != 5 {
            return terr(format!(
                "`exec` expects (exec, dir, path, args, stdin) — 5 arguments but got {}",
                args.len()
            ));
        }
        let e = self.infer(&args[0])?;
        self.unify(&Ty::Exec, &e).map_err(|err| TypeError {
            message: format!("`exec`'s first argument must be an `Exec` capability: {}", err.message),
        })?;
        let rights = self.dir_cap_rights("exec", &args[1])?;
        if !rights.read {
            return terr(format!(
                "`exec` needs a `Dir` with `Read` to locate the executable, but the capability is `{rights}`"
            ));
        }
        for (i, what) in [(2usize, "path"), (3, "args"), (4, "stdin")] {
            let at = self.infer(&args[i])?;
            self.unify(&Ty::String, &at).map_err(|err| TypeError {
                message: format!("in call to `exec`: {what} must be a String: {}", err.message),
            })?;
        }
        Ok(Some(Ty::String))
    }

    /// Resolve a call's first argument as a `Net` capability and yield its verbs.
    /// An unconstrained variable defaults to the full set (bare `Net`).
    fn net_cap_rights(&mut self, name: &str, arg: &Expr) -> Result<NetRights, TypeError> {
        let cap = self.infer(arg)?;
        match self.resolve(&cap) {
            Ty::Net(r) => Ok(r),
            Ty::Var(_) => {
                self.unify(&cap, &Ty::Net(NetRights::full()))?;
                Ok(NetRights::full())
            }
            other => terr(format!(
                "`{name}` expects a `Net` capability but got `{other}`"
            )),
        }
    }

    /// Type-check a network-capability op, enforcing the `Net`'s rights permit it:
    /// `connect` needs `Connect` (+`Tcp`); `listen` needs `Listen` (+`Tcp`);
    /// `restrict` is verb-neutral address attenuation (preserves the rights set).
    /// (Narrowing is done with the `as` ascription, not per-verb builtins.)
    /// Returns `Ok(None)` when `name` is not a Net op.
    fn check_net_op(&mut self, name: &str, args: &[Expr]) -> Result<Option<Ty>, TypeError> {
        let arity = match name {
            "connect" | "try_connect" | "listen" | "only" | "deny" | "resolve" => 2,
            // (RFC-0020) pinned dial: (net, ip, host, port, secure).
            "connect_pinned" | "try_connect_pinned" => 5,
            _ => return Ok(None),
        };
        if args.len() != arity {
            return terr(format!(
                "`{name}` expects {arity} argument(s) but got {}",
                args.len()
            ));
        }
        let rights = self.net_cap_rights(name, &args[0])?;
        if name == "connect_pinned" || name == "try_connect_pinned" {
            // (RFC-0020) mixed trailing args: ip:String, host:String, port:Int, secure:Bool.
            for (arg, expected) in args[1..].iter().zip([Ty::String, Ty::String, Ty::Int, Ty::Bool]) {
                let at = self.infer(arg)?;
                self.unify(&expected, &at).map_err(|e| TypeError {
                    message: format!("in call to `{name}`: {}", e.message),
                })?;
            }
        } else {
            // The trailing argument: a typed `NetPolicy` for the policy verbs (`only`/`deny`,
            // RFC-0011), a `host:port` string for the address verbs (`connect`/`listen`/`restrict`),
            // a bare host string for `resolve`.
            let expected = if name == "only" || name == "deny" {
                Ty::Named("NetPolicy".into(), Vec::new())
            } else {
                Ty::String
            };
            for arg in &args[1..] {
                let at = self.infer(arg)?;
                self.unify(&expected, &at).map_err(|e| TypeError {
                    message: format!("in call to `{name}`: {}", e.message),
                })?;
            }
        }
        let ret = match name {
            "connect" => {
                if !rights.connect {
                    return terr(format!(
                        "`connect` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`connect` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Socket
            }
            // Like `connect` but total: returns `Option(Socket)` — `None` on a failed
            // dial instead of trapping. Lets a server (e.g. a proxy) survive a down
            // upstream. Same rights as `connect`.
            "try_connect" => {
                if !rights.connect {
                    return terr(format!(
                        "`try_connect` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`try_connect` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Named("Option".into(), vec![Ty::Socket])
            }
            "listen" => {
                if !rights.listen {
                    return terr(format!(
                        "`listen` needs `Listen` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`listen` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Listener
            }
            // (RFC-0020) Resolve a name to its IP literals. Gated on `Connect` alone (it
            // adds no authority — `connect_pinned` re-checks the chosen IP); no transport
            // requirement, since resolution is not itself a dial.
            "resolve" => {
                if !rights.connect {
                    return terr(format!(
                        "`resolve` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                Ty::List(Box::new(Ty::String))
            }
            // Pinned dials — same rights as `connect` (a literal-IP TCP dial), the hostname
            // carried only for SNI/Host. `try_` is total (`Option(Socket)`).
            "connect_pinned" => {
                if !rights.connect {
                    return terr(format!(
                        "`connect_pinned` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`connect_pinned` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Socket
            }
            "try_connect_pinned" => {
                if !rights.connect {
                    return terr(format!(
                        "`try_connect_pinned` needs `Connect` but the capability is `{rights}`"
                    ));
                }
                if !rights.tcp {
                    return terr(format!(
                        "`try_connect_pinned` is only implemented over `Tcp`, but the capability is `{rights}`"
                    ));
                }
                Ty::Named("Option".into(), vec![Ty::Socket])
            }
            // Attenuating the address set leaves the rights (verbs + transports) intact.
            // `only` narrows a `Net` to a `NetPolicy`'s address set; `deny` subtracts an
            // address pattern (a monotone exclusion). Both preserve the rights set.
            "only" | "deny" => Ty::Net(rights),
            _ => unreachable!(),
        };
        Ok(Some(ret))
    }

    /// Check a capability narrowing ascription (`src as target`): the target must
    /// be the *same* capability with a subset of `src`'s rights. Narrowing only
    /// drops rights — you can never ascribe a right the source doesn't hold. An
    /// unconstrained source is pinned to the target.
    fn check_narrow(&mut self, src: &Ty, target: &Ty) -> Result<(), TypeError> {
        let resolved = self.resolve(src);
        let ok = match (&resolved, target) {
            (Ty::Dir(s), Ty::Dir(t)) => (!t.read || s.read) && (!t.write || s.write),
            (Ty::File(s), Ty::File(t)) => (!t.read || s.read) && (!t.write || s.write),
            (Ty::Net(s), Ty::Net(t)) => {
                (!t.connect || s.connect)
                    && (!t.listen || s.listen)
                    && (!t.tcp || s.tcp)
                    && (!t.udp || s.udp)
                    && (!t.uds || s.uds)
            }
            (Ty::Console, Ty::Console) => true,
            (Ty::Exec, Ty::Exec) => true,
            // An unconstrained source: pin it to the ascribed capability.
            (Ty::Var(_), Ty::Dir(_) | Ty::File(_) | Ty::Net(_) | Ty::Console | Ty::Exec) => {
                return self.unify(src, target).map_err(|e| TypeError {
                    message: format!("in `as` ascription: {}", e.message),
                });
            }
            _ => {
                return terr(format!(
                    "`as` narrows a capability to a subset of its rights; cannot ascribe `{resolved}` as `{target}`"
                ));
            }
        };
        if !ok {
            return terr(format!(
                "`as` can only drop rights: `{target}` is not a subset of `{resolved}`"
            ));
        }
        Ok(())
    }

    /// Bind a call argument (`actual`) to a parameter (`expected`), allowing a
    /// broader capability to stand in for a narrower one — implicit directional
    /// narrowing, so a full `Net` satisfies a `Net[Connect]` parameter (more
    /// authority can always be used where less is asked). The callee is still
    /// type-bounded to the parameter's rights, so this stays sound: it cannot do
    /// (or re-pass) more than its declared type permits. Everything else (Vars,
    /// non-caps, exact caps, a too-narrow argument) falls back to unification.
    fn coerce_arg(&mut self, expected: &Ty, actual: &Ty) -> Result<(), TypeError> {
        let coercible = match (self.resolve(expected), self.resolve(actual)) {
            // `want`'s rights must be a subset of what the argument `has`.
            (Ty::Dir(want), Ty::Dir(has)) => (!want.read || has.read) && (!want.write || has.write),
            (Ty::File(want), Ty::File(has)) => (!want.read || has.read) && (!want.write || has.write),
            (Ty::Net(want), Ty::Net(has)) => {
                (!want.connect || has.connect)
                    && (!want.listen || has.listen)
                    && (!want.tcp || has.tcp)
                    && (!want.udp || has.udp)
                    && (!want.uds || has.uds)
            }
            _ => false,
        };
        if coercible {
            Ok(())
        } else {
            self.unify(expected, actual)
        }
    }

    // --- inference ---

    fn infer_block(&mut self, block: &Block) -> Result<Ty, TypeError> {
        let is_region = block.region.is_some();
        if is_region {
            self.region_locals.push(HashSet::new());
        }
        let result = self.infer_block_inner(block);
        if is_region {
            self.region_locals.pop();
        }
        let ty = result?;
        if let Some(ann) = &block.region {
            if let Some(want) = &ann.ty {
                let want_ty = self.to_ty(want);
                self.unify(&ty, &want_ty).map_err(|e| TypeError {
                    message: format!("region value: {}", e.message),
                })?;
            }
        }
        Ok(ty)
    }

    fn infer_block_inner(&mut self, block: &Block) -> Result<Ty, TypeError> {
        self.push();
        let mut ty = Ty::Nil;
        for (i, stmt) in block.stmts.iter().enumerate() {
            if let Some(line) = block.lines.get(i) {
                self.cur_line = *line;
            }
            match stmt {
                Stmt::Let { name, ty: decl, mutable, value } => {
                    let vt = self.infer(value)?;
                    // An ascription is a unification constraint: it pins type
                    // variables the RHS leaves open (`let xs: List(Int) = []`,
                    // a return-position type variable) and errors at THIS line
                    // when the RHS disagrees.
                    if let Some(decl) = decl {
                        // (RFC-0025) `frozen` asserts deep immutability, so a `frozen`
                        // binding cannot also be mutable — `var x: frozen T` is a
                        // contradiction the checker rejects (the contract has teeth).
                        if *mutable && is_frozen_type(decl) {
                            return Err(TypeError {
                                message: format!(
                                    "`{name}` is declared `frozen` (deeply immutable) but also `var` (mutable) — drop the `var` (use `let`), or drop `frozen`"
                                ),
                            });
                        }
                        let want = self.to_ty(decl);
                        self.unify(&want, &vt).map_err(|e| TypeError {
                            message: format!(
                                "`{name}` is declared `{want}` but the value disagrees: {}",
                                e.message
                            ),
                        })?;
                    }
                    self.define(name.clone(), vt, *mutable);
                    ty = Ty::Nil;
                }
                Stmt::Assign { name, value } => {
                    let vt = self.infer(value)?;
                    let Some(existing) = self.lookup(name) else {
                        self.pop();
                        return terr(format!("assignment to unbound variable `{name}`"));
                    };
                    if self.is_mutable(name) == Some(false) {
                        self.pop();
                        return terr(format!(
                            "cannot assign to `{name}`: it is immutable (declared with `let`)"
                        ));
                    }
                    if let Some(scope) = self.region_locals.last() {
                        if !scope.contains(name) && !is_scalar_ty(&self.resolve(&existing)) {
                            self.pop();
                            return terr(format!(
                                "cannot assign `{name}` inside `region:`: it is declared outside the region and is not a scalar — a region's only escape is its value"
                            ));
                        }
                    }
                    self.unify(&existing, &vt)?;
                    self.consumed.remove(name); // reassignment re-initializes
                    ty = Ty::Nil;
                }
                Stmt::LetTuple { names, value } => {
                    let mut seen = HashSet::new();
                    for n in names {
                        if n != "_" && !seen.insert(n.clone()) {
                            return terr(format!(
                                "tuple destructure binds `{n}` more than once — each name must be distinct"
                            ));
                        }
                    }
                    let vt = self.infer(value)?;
                    let elem_tys: Vec<Ty> = (0..names.len()).map(|_| self.fresh()).collect();
                    self.unify(&Ty::Tuple(elem_tys.clone()), &vt).map_err(|e| TypeError {
                        message: format!("tuple destructure: {}", e.message),
                    })?;
                    for (n, t) in names.iter().zip(elem_tys) {
                        self.define(n.clone(), t, false);
                    }
                    ty = Ty::Nil;
                }
                Stmt::Return(opt) => {
                    let t = match opt {
                        Some(e) => self.infer(e)?,
                        None => Ty::Nil,
                    };
                    if let Some(ret) = self.current_ret.clone() {
                        self.unify(&ret, &t).map_err(|e| TypeError {
                            message: format!("`return` value: {}", e.message),
                        })?;
                    }
                    // A return diverges: its position can satisfy any expected
                    // type, so contribute a fresh var (which unifies with anything).
                    ty = self.fresh();
                }
                Stmt::Expr(e) => {
                    ty = self.infer(e)?;
                }
                Stmt::Yield(e) => {
                    if !self.region_locals.is_empty() {
                        self.pop();
                        return terr(
                            "cannot `yield` inside `region:`: the generator frame outlives the region".to_string(),
                        );
                    }
                    ty = self.infer(e)?;
                }
                // `break`/`continue` diverge (control leaves the block), so like
                // `return` they contribute a fresh var that unifies with any
                // expected type — letting `match x { _ -> { break } ... }` work.
                // Misuse outside a loop is caught by codegen (no enclosing label).
                Stmt::Break | Stmt::Continue => {
                    ty = self.fresh();
                }
            }
        }
        self.pop();
        Ok(ty)
    }

    fn infer(&mut self, expr: &Expr) -> Result<Ty, TypeError> {
        let t = self.infer_inner(expr)?;
        // (RFC-0047) `Dict` keys and `Set` members require `Eq`; `Float` is
        // `PartialEq` but not `Eq` (NaN != NaN), so a `Float`-keyed dict has an
        // unretrievable NaN entry and a `Float` key is a precision trap. Reject
        // it at check time — once any expression's type concretely has a `Float`
        // in key position, the whole program is refused (a type-level rule, not a
        // per-NaN runtime trap). Only fires when the key type is concretely Float
        // (an unresolved key var never triggers).
        if let Some(kind) = float_key_position(&self.resolve(&t)) {
            return terr(float_key_reject_message(kind));
        }
        if let Some(rec) = &mut self.type_record {
            rec.insert(expr as *const Expr as usize, t.clone());
        }
        Ok(t)
    }

    fn infer_inner(&mut self, expr: &Expr) -> Result<Ty, TypeError> {
        match expr {
            // Expanded away by `crate::tagged` during linking, before checking.
            Expr::TaggedLit { tag, .. } => {
                unreachable!("unexpanded tagged literal `{tag}` reached the type checker")
            }
            Expr::Int(_) => Ok(Ty::Int),
            Expr::Float(_) => Ok(Ty::Float),
            Expr::Duration(_) => Ok(Ty::Duration),
            Expr::Str(_) => Ok(Ty::String),
            Expr::Bool(_) => Ok(Ty::Bool),
            // A range lowers to a list-building block; type it as that block.
            Expr::Range { lo, hi, inclusive } => {
                let d = witchy_syntax::parser::desugar_range((**lo).clone(), (**hi).clone(), *inclusive);
                self.infer(&d)
            }
            // A subscript lowers to an `list.at(base, index)` call; type it as that.
            Expr::Index { base, index } => {
                let d = witchy_syntax::parser::desugar_index((**base).clone(), (**index).clone());
                self.infer(&d)
            }
            Expr::MethodCall { method, .. } => {
                // Trait lowering resolves every method call (impl, trait
                // bound, or static); one that survives is unresolvable.
                terr(format!(
                    "cannot resolve the method call `.{method}(…)` — methods come from \
                     `impl` blocks; a plain function is called as `{method}(value, …)`"
                ))
            }
            // Named-field record construction is lowered by `witchy_syntax::records`
            // before type-checking.
            Expr::Record { .. } => {
                unreachable!("Expr::Record is lowered by witchy_syntax::records before typeck")
            }
            // `while let` lowers to a `while true` over a match; type that.
            Expr::WhileLet { pattern, scrutinee, body } => {
                let d = witchy_syntax::parser::desugar_while_let(
                    pattern.clone(),
                    (**scrutinee).clone(),
                    body.clone(),
                );
                self.infer(&d)
            }
            Expr::List(items) => {
                let elem = self.fresh();
                for it in items {
                    let t = self.infer(it)?;
                    self.unify(&elem, &t)?;
                }
                Ok(Ty::List(Box::new(elem)))
            }
            Expr::Tuple(items) => {
                let tys = items
                    .iter()
                    .map(|e| self.infer(e))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Ty::Tuple(tys))
            }
            Expr::Var(name) => {
                if self.consumed.contains(name) {
                    return terr(format!(
                        "use of `{name}` after it was moved (consumed by an `own` parameter)"
                    ));
                }
                if let Some(t) = self.lookup(name) {
                    return Ok(t);
                }
                // A bare top-level function name used as a value is a first-class
                // function. Reject `var`/`own` functions, whose move-in/out
                // calling convention can't be expressed as a plain value.
                if let Some((params, ret)) = self.fn_sigs.get(name).cloned() {
                    if let Some(convs) = self.fn_conventions.get(name) {
                        if convs.iter().any(|c| *c != Convention::Let) {
                            return terr(format!(
                                "`{name}` takes a `var`/`own` parameter, so it can't be used as a function value"
                            ));
                        }
                    }
                    let typarams: HashSet<u32> = self
                        .fn_typarams
                        .get(name)
                        .into_iter()
                        .flatten()
                        .map(|(_, id)| *id)
                        .collect();
                    let (params, ret) = self.instantiate(&params, &ret, &typarams);
                    return Ok(Ty::Fn(params, Box::new(ret)));
                }
                terr(format!("unbound variable `{name}`"))
            }
            Expr::Lambda { params, body, ret } => {
                // Closures capture by value, so an assignment to a captured
                // (outer) variable cannot propagate out: the interpreter would
                // silently mutate a private copy while the compiled backends can't
                // express it at all. Reject it uniformly here (using the shared
                // AST capture/assignment scan) so every backend agrees.
                let outer = witchy_syntax::lambda_scan::lambda_outer_assigns(params, body);
                if !outer.is_empty() {
                    return terr(format!(
                        "a closure cannot assign to the captured variable `{}` (captures are by value, so the write would be lost) — return the new value or use a `var` parameter instead",
                        outer.join("`, `")
                    ));
                }
                self.push();
                let param_tys: Vec<Ty> = params
                    .iter()
                    .map(|p| match &p.ty {
                        Some(t) => self.to_ty(t),
                        None => self.fresh(),
                    })
                    .collect();
                for (p, ty) in params.iter().zip(&param_tys) {
                    self.define(p.name.clone(), ty.clone(), p.convention.binds_mutable());
                }
                // The closure is its OWN `?` boundary: a `?` in its body propagates
                // to the closure's return type, not the enclosing function's. Use
                // the declared return type if given (`fn(x) -> Result(..): ...`),
                // else a fresh var the body pins. Save/restore the outer return so
                // a `?` after the closure still targets the enclosing function.
                let declared = ret.as_ref().map(|t| self.to_ty(t));
                let lambda_ret = declared.clone().unwrap_or_else(|| self.fresh());
                let saved_ret = self.current_ret.replace(lambda_ret.clone());
                let body_ty = self.infer_block(body)?;
                self.unify(&lambda_ret, &body_ty).map_err(|e| TypeError {
                    message: format!("closure body type does not match its declared return type: {}", e.message),
                })?;
                self.current_ret = saved_ret;
                self.pop();
                Ok(Ty::Fn(param_tys, Box::new(lambda_ret)))
            }
            Expr::Call { name, args } => {
                // A local binding (parameter or `let`) holding a function value:
                // apply it. Handles both an explicit `fn(..)->..` type and an as
                // yet unconstrained variable (which we pin to a function type).
                if let Some(vty) = self.lookup(name) {
                    match self.resolve(&vty) {
                        Ty::Fn(param_tys, ret) => {
                            if param_tys.len() != args.len() {
                                return terr(format!(
                                    "`{name}` expects {} argument(s) but got {}",
                                    param_tys.len(),
                                    args.len()
                                ));
                            }
                            for (arg, pty) in args.iter().zip(&param_tys) {
                                let at = self.infer(arg)?;
                                self.coerce_arg(pty, &at).map_err(|e| TypeError {
                                    message: format!("in call to `{name}`: {}", e.message),
                                })?;
                            }
                            return Ok(*ret);
                        }
                        Ty::Var(_) => {
                            let mut argtys = Vec::new();
                            for arg in args {
                                argtys.push(self.infer(arg)?);
                            }
                            let ret = self.fresh();
                            self.unify(&vty, &Ty::Fn(argtys, Box::new(ret.clone())))?;
                            return Ok(ret);
                        }
                        _ => {} // a non-function local with this name: fall through
                    }
                }
                if let Some(t) = self.check_file_op(name, args)? {
                    return Ok(t);
                }
                if let Some(t) = self.check_dir_op(name, args)? {
                    return Ok(t);
                }
                if let Some(t) = self.check_exec_op(name, args)? {
                    return Ok(t);
                }
                if let Some(t) = self.check_net_op(name, args)? {
                    return Ok(t);
                }
                if let Some(t) = self.check_try_ctx(name, args)? {
                    return Ok(t);
                }
                let Some((params, ret)) = self.call_sig(name) else {
                    // `to_string` was removed from the surface: interpolation
                    // IS the rendering (it desugars to the internal __render).
                    if name == "to_string" || name == "int_to_string" {
                        return terr(format!(
                            "`{name}` was removed — render with `\"${{x}}\"` \
                             interpolation (it works on every value), or \
                             `say(console, x)` to print a `Show` value"
                        ));
                    }
                    // `show` is the `Show` trait method, not a free function: a
                    // bare `show(x)` resolves only when x's concrete type is
                    // statically known here. Point at the renderers that always
                    // work, rather than the misleading `import set` near-miss
                    // (`set.show` is an unrelated same-named module function).
                    if name == "show" && args.len() == 1 {
                        return terr(
                            "could not resolve the `Show` method `show` on this \
                             value — a bare `show(x)` needs x's concrete type to be \
                             statically known. Render any value with `\"${x}\"` \
                             interpolation or `say(console, x)`, or bind x via a \
                             `for` loop or a typed parameter so dispatch resolves",
                        );
                    }
                    // A retired global builtin: name the module-qualified
                    // spelling that replaced it (the one-cut migration).
                    if let Some(moved) = witchy_syntax::aliases::moved_builtin(name) {
                        return terr(format!(
                            "`{name}` moved to `{moved}` — pure data operations are \
                             module-qualified now (no import needed; the core modules \
                             are always available)"
                        ));
                    }
                    // `<` `<=` `>` `>=` desugar to these trait-method calls; an
                    // unresolved one means the operand's type lacks `Ord`, so name
                    // the operator and the fix rather than leaking the desugar name
                    // (and suggesting an unrelated `list` function as a "typo").
                    if let Some(op) = match name.as_str() {
                        "less" => Some("<"),
                        "greater" => Some(">"),
                        "less_equal" => Some("<="),
                        "greater_equal" => Some(">="),
                        _ => None,
                    } {
                        return terr(format!(
                            "`{op}` is not defined for this type — it requires `Ord`; derive it \
                             with `derive(PartialEq, Eq, PartialOrd, Ord)` or implement it"
                        ));
                    }
                    // If the name is an unimported stdlib function, point the way;
                    // otherwise suggest a near-miss stdlib name (a likely typo).
                    let hint = match witchy_syntax::linker::std_modules_for_function(name).as_slice() {
                        [m] => format!(" — did you forget `import {m}`?"),
                        many if !many.is_empty() => {
                            format!(" — did you forget to import one of: {}?", many.join(", "))
                        }
                        _ => match witchy_syntax::linker::closest_std_function(name) {
                            Some((cand, m)) => format!(" — did you mean `{cand}` (`import {m}`)?"),
                            None => String::new(),
                        },
                    };
                    return terr(format!("call to unknown function `{name}`{hint}"));
                };
                if params.len() != args.len() {
                    return terr(format!(
                        "`{name}` expects {} argument(s) but got {}",
                        params.len(),
                        args.len()
                    ));
                }
                for (arg, param_ty) in args.iter().zip(&params) {
                    let at = self.infer(arg)?;
                    self.coerce_arg(param_ty, &at)
                        .map_err(|e| TypeError { message: format!("in call to `{name}`: {}", e.message) })?;
                }
                // Enforce conventions: a `var` parameter needs a mutable variable;
                // `own` consumes its argument (use-after-move becomes an error).
                if let Some(convs) = self.fn_conventions.get(name).cloned() {
                    for (arg, conv) in args.iter().zip(&convs) {
                        match conv {
                            Convention::Var => match arg {
                                Expr::Var(v) if self.is_mutable(v) == Some(true) => {}
                                Expr::Var(v) => {
                                    return terr(format!(
                                        "argument `{v}` to `{name}` is passed to a `var` parameter, so it must be a mutable `var`"
                                    ))
                                }
                                _ => {
                                    return terr(format!(
                                        "the argument to a `var` parameter of `{name}` must be a mutable variable"
                                    ))
                                }
                            },
                            Convention::Own => {
                                if let Expr::Var(v) = arg {
                                    self.consumed.insert(v.clone());
                                }
                            }
                            // An owned value or an immutable borrow: no call-site
                            // obligation (a borrow's no-escape rule is enforced at
                            // native-lowering time by Rust's borrow checker).
                            Convention::Let | Convention::Borrow => {}
                        }
                    }
                }
                Ok(ret)
            }
            Expr::Apply { func, args } => {
                // The callee is an arbitrary expression of function type; unify
                // it with `fn(argtys) -> r` and yield `r`.
                let fty = self.infer(func)?;
                let mut argtys = Vec::new();
                for arg in args {
                    argtys.push(self.infer(arg)?);
                }
                let ret = self.fresh();
                self.unify(&fty, &Ty::Fn(argtys, Box::new(ret.clone())))
                    .map_err(|e| TypeError {
                        message: format!("in function application: {}", e.message),
                    })?;
                Ok(ret)
            }
            Expr::Ctor { name, args } => {
                if let Some((fields, result)) = self.ctor_sigs.get(name).cloned() {
                    let typarams = self.ctor_typarams.get(name).cloned().unwrap_or_default();
                    let (fields, result) = self.instantiate(&fields, &result, &typarams);
                    if fields.len() != args.len() {
                        return terr(format!(
                            "constructor `{name}` takes {} field(s) but got {}",
                            fields.len(),
                            args.len()
                        ));
                    }
                    for (arg, fty) in args.iter().zip(&fields) {
                        let at = self.infer(arg)?;
                        // A capability field accepts a broader argument (a full
                        // `Net` into a `Net[Connect]` field), like a call boundary.
                        self.coerce_arg(fty, &at).map_err(|e| TypeError {
                            message: format!("in constructor `{name}`: {}", e.message),
                        })?;
                    }
                    Ok(result)
                } else {
                    // Unknown constructor: still check its arguments, but don't
                    // constrain the result type.
                    for arg in args {
                        self.infer(arg)?;
                    }
                    Ok(self.fresh())
                }
            }
            Expr::Unary { op, expr } => {
                let t = self.infer(expr)?;
                match op {
                    // `move x` has the type of `x`, and consumes it: it's a
                    // use-site ownership transfer, so any later use of the moved
                    // variable is an error on every backend (matching native's
                    // actual move). `infer(expr)` above already rejected moving an
                    // already-consumed binding.
                    UnOp::Move => {
                        if let Expr::Var(v) = expr.as_ref() {
                            self.consumed.insert(v.clone());
                        }
                        Ok(t)
                    }
                    // `await e` has the type of `e` (Phase 1: the awaited value is
                    // the value itself; suspension is invisible to the type).
                    UnOp::Await => Ok(t),
                    UnOp::Not => {
                        self.unify(&Ty::Bool, &t)?;
                        Ok(Ty::Bool)
                    }
                    UnOp::Neg => match self.resolve(&t) {
                        Ty::Float => {
                            self.unify(&t, &Ty::Float)?;
                            Ok(Ty::Float)
                        }
                        _ => {
                            self.unify(&t, &Ty::Int)?;
                            Ok(Ty::Int)
                        }
                    },
                    UnOp::BitNot => {
                        self.unify(&Ty::Int, &t)?;
                        Ok(Ty::Int)
                    }
                }
            }
            Expr::Field { base, field } => {
                let bt = self.infer(base)?;
                let resolved = self.resolve(&bt);
                // `pair.0` — a tuple element, by position.
                if let Ok(i) = field.parse::<usize>() {
                    let Ty::Tuple(ts) = &resolved else {
                        return terr(format!(
                            "element access `.{field}` requires a tuple, found `{resolved}`"
                        ));
                    };
                    let Some(t) = ts.get(i) else {
                        return terr(format!(
                            "tuple `{resolved}` has no element `.{i}` (it has {})",
                            ts.len()
                        ));
                    };
                    return Ok(t.clone());
                }
                let Ty::Named(tyname, args) = &resolved else {
                    return terr(format!(
                        "field access `.{field}` requires a record, found `{resolved}`"
                    ));
                };
                if self.sealed_types.contains(tyname) {
                    return terr(format!(
                        "`{tyname}` is a sealed capability — its fields are private; \
                         destructure it with `match` inside its own module"
                    ));
                }
                let Some((params, fields)) = self.record_fields.get(tyname).cloned() else {
                    return terr(format!("type `{tyname}` is not a record, so it has no field `{field}`"));
                };
                let Some((_, fty)) = fields.iter().find(|(n, _)| n == field) else {
                    return terr(format!("record `{tyname}` has no field `{field}`"));
                };
                // Instantiate the field type with the value's actual type args.
                let map: HashMap<u32, Ty> =
                    params.iter().cloned().zip(args.iter().cloned()).collect();
                Ok(self.subst_vars(fty, &map))
            }
            Expr::RecordUpdate { base, fields } => {
                let bt = self.infer(base)?;
                let resolved = self.resolve(&bt);
                let (tyname, args) = match &resolved {
                    Ty::Named(n, a) => (n.clone(), a.clone()),
                    other => {
                        return terr(format!("`update` requires a record, found `{other}`"))
                    }
                };
                if self.sealed_types.contains(&tyname) {
                    return terr(format!(
                        "`{tyname}` is a sealed capability and cannot be `update`d — \
                         only its own module may construct it"
                    ));
                }
                let Some((params, rec_fields)) = self.record_fields.get(&tyname).cloned() else {
                    return terr(format!("type `{tyname}` is not a record"));
                };
                let map: HashMap<u32, Ty> =
                    params.into_iter().zip(args).collect();
                for (fname, vexpr) in fields {
                    let Some((_, fty)) = rec_fields.iter().find(|(n, _)| n == fname) else {
                        return terr(format!("record `{tyname}` has no field `{fname}`"));
                    };
                    let expected = self.subst_vars(fty, &map);
                    let vt = self.infer(vexpr)?;
                    self.unify(&expected, &vt).map_err(|e| TypeError {
                        message: format!("`update` of field `{fname}`: {}", e.message),
                    })?;
                }
                // The result is a record of the same type as the base.
                Ok(resolved)
            }
            Expr::Try(inner) => {
                let it = self.infer(inner)?;
                let resolved = self.resolve(&it);
                let (value_ty, expected_ret) = match &resolved {
                    Ty::Named(n, args) if n == "Result" && args.len() == 2 => {
                        let r = self.fresh();
                        (
                            args[0].clone(),
                            Ty::Named("Result".into(), vec![r, args[1].clone()]),
                        )
                    }
                    Ty::Named(n, args) if n == "Option" && args.len() == 1 => {
                        let r = self.fresh();
                        (args[0].clone(), Ty::Named("Option".into(), vec![r]))
                    }
                    other => {
                        return terr(format!(
                            "`?` expects a Result or Option, found `{other}`"
                        ))
                    }
                };
                let Some(ret) = self.current_ret.clone() else {
                    return terr("`?` can only be used inside a function returning Result or Option");
                };
                self.unify(&ret, &expected_ret).map_err(|e| TypeError {
                    message: format!(
                        "`?` propagates from a `{resolved}`, but the enclosing function returns a different type: {}",
                        e.message
                    ),
                })?;
                Ok(value_ty)
            }
            Expr::As { expr, ty } => {
                let src = self.infer(expr)?;
                let target = self.to_ty(ty);
                self.check_narrow(&src, &target)?;
                Ok(target)
            }
            Expr::Binary { op, lhs, rhs } => self.infer_binary(*op, lhs, rhs),
            Expr::If {
                cond,
                then_block,
                else_block,
            } => {
                let ct = self.infer(cond)?;
                self.unify(&Ty::Bool, &ct)
                    .map_err(|e| TypeError { message: format!("`if` condition: {}", e.message) })?;
                let before = self.consumed.clone();
                let tt = self.infer_block(then_block)?;
                let consumed_then = std::mem::replace(&mut self.consumed, before.clone());
                match else_block {
                    Some(eb) => {
                        let et = self.infer_block(eb)?;
                        self.unify(&tt, &et).map_err(|e| TypeError {
                            message: format!("`if` branches disagree: {}", e.message),
                        })?;
                    }
                    None => {
                        self.unify(&tt, &Ty::Nil)?;
                    }
                }
                // A binding consumed on either path is treated as consumed after.
                self.consumed = &consumed_then | &self.consumed;
                Ok(tt)
            }
            Expr::Block(b) => self.infer_block(b),
            Expr::While { cond, body } => {
                let ct = self.infer(cond)?;
                self.unify(&Ty::Bool, &ct).map_err(|e| TypeError {
                    message: format!("`while` condition: {}", e.message),
                })?;
                self.infer_block(body)?;
                Ok(Ty::Nil)
            }
            Expr::For { var, iter, body } => {
                let it = self.infer(iter)?;
                let elem = self.fresh();
                self.unify(&Ty::List(Box::new(elem.clone())), &it).map_err(|e| TypeError {
                    message: format!("`for` expects a List to iterate: {}", e.message),
                })?;
                self.push();
                self.define(var.clone(), elem, false);
                self.infer_block(body)?;
                self.pop();
                Ok(Ty::Nil)
            }
            Expr::Match { scrutinee, arms } => self.infer_match(scrutinee, arms),
        }
    }

    fn infer_binary(&mut self, op: ast::BinOp, lhs: &Expr, rhs: &Expr) -> Result<Ty, TypeError> {
        use ast::BinOp::*;
        let lt = self.infer(lhs)?;
        let rt = self.infer(rhs)?;
        match op {
            Add | Sub | Mul | Div => {
                let lr = self.resolve(&lt);
                let rr = self.resolve(&rt);
                // `+` concatenates strings — type-directed, never coercing: a
                // String operand demands a String on the other side, and a
                // mixed `+` names interpolation as the fix.
                if matches!(lr, Ty::String) || matches!(rr, Ty::String) {
                    if !matches!(op, Add) {
                        return terr("only `+` is defined on String (it concatenates)");
                    }
                    for other in [&lr, &rr] {
                        if !matches!(other, Ty::String | Ty::Var(_)) {
                            return terr(format!(
                                "cannot `+` a String and `{other}` — render the value \
                                 into the string instead: `\"${{a}}${{b}}\"`"
                            ));
                        }
                    }
                    self.unify(&Ty::String, &lt)?;
                    self.unify(&Ty::String, &rt)?;
                    return Ok(Ty::String);
                }
                // Duration arithmetic: durations add/subtract with each other,
                // scale by an Int (multiply / divide), and Duration / Duration is
                // their Int ratio. Mixing a Duration with a bare Int under +/-
                // is a type error.
                if matches!(lr, Ty::Duration) || matches!(rr, Ty::Duration) {
                    return match op {
                        Add | Sub => {
                            self.unify(&Ty::Duration, &lt)?;
                            self.unify(&Ty::Duration, &rt)?;
                            Ok(Ty::Duration)
                        }
                        Mul => {
                            if matches!(lr, Ty::Duration) {
                                self.unify(&Ty::Int, &rt)?;
                            } else {
                                self.unify(&Ty::Int, &lt)?;
                            }
                            Ok(Ty::Duration)
                        }
                        Div => {
                            if matches!(lr, Ty::Duration) && matches!(rr, Ty::Duration) {
                                Ok(Ty::Int)
                            } else if matches!(lr, Ty::Duration) {
                                self.unify(&Ty::Int, &rt)?;
                                Ok(Ty::Duration)
                            } else {
                                terr("an Int cannot be divided by a Duration")
                            }
                        }
                        _ => unreachable!(),
                    };
                }
                let either_float = matches!(lr, Ty::Float) || matches!(rr, Ty::Float);
                let num = if either_float { Ty::Float } else { Ty::Int };
                self.unify(&lt, &num)?;
                self.unify(&rt, &num)?;
                Ok(num)
            }
            Mod | BitAnd | BitOr | BitXor | Shl | Shr => {
                self.unify(&Ty::Int, &lt)?;
                self.unify(&Ty::Int, &rt)?;
                Ok(Ty::Int)
            }
            Concat => {
                // Surface `<>` is gone; the OP survives as compiler-internal IR
                // (codegen flips string `+` to it after annotation), so the
                // annotating run types it and only the user-facing check
                // teaches.
                if self.type_record.is_some() {
                    self.unify(&Ty::String, &lt)?;
                    self.unify(&Ty::String, &rt)?;
                    return Ok(Ty::String);
                }
                terr("`<>` was removed — `+` concatenates strings, and `\"${a}${b}\"` interpolates")
            }
            Eq | NotEq => {
                self.unify(&lt, &rt)?;
                // (RFC-0047) `==`/`!=` desugar through `PartialEq` at every depth.
                // Function and capability types have no meaningful equality — a
                // function's identity is an implementation accident (monomorphization
                // /inlining), and a capability is authority, not data — so comparing
                // them (directly or nested in a container) is a compile-time error,
                // not a backend-dependent answer. This deletes the confirmed
                // `f == f` parity divergence by construction.
                let resolved = self.resolve(&lt);
                if let Some(kind) = uncomparable_kind(&resolved) {
                    return terr(equality_reject_message(kind, &resolved));
                }
                Ok(Ty::Bool)
            }
            Lt | LtEq | Gt | GtEq => {
                self.unify(&lt, &rt)?;
                // Ordering is defined only for the totally-ordered primitives.
                // Without a type-class mechanism, allowing it on arbitrary types
                // would type-check but crash at runtime, so reject it here.
                match self.resolve(&lt) {
                    Ty::Int | Ty::Float | Ty::String | Ty::Duration => Ok(Ty::Bool),
                    // A `Ty::Var` operand is an unbounded generic type parameter
                    // (it renders as `?`): `<` on it needs an `Ord` bound to
                    // dispatch through the trait, so point at that instead of a
                    // bare "found `?`".
                    other @ Ty::Var(_) => terr(format!(
                        "ordering comparison on `{other}` — if this is a generic type \
                         parameter, bound it with `where T: Ord` so `<` dispatches through \
                         the Ord trait"
                    )),
                    other => terr(format!(
                        "ordering comparison requires Int, Float, String, Duration, or a type \
                         that derives `Ord` — found `{other}` (derive it with \
                         `derive(PartialEq, Eq, PartialOrd, Ord)`)"
                    )),
                }
            }
            And => {
                self.unify(&Ty::Bool, &lt)?;
                self.unify(&Ty::Bool, &rt)?;
                Ok(Ty::Bool)
            }
            Or => {
                // `a || b`: ordinary logical-or for Bool, otherwise the truthy
                // fallback `if truthy(a): a else: b` over the emptyable built-ins
                // (falsy = "" / None / []). Both operands share a type and the
                // result is that type.
                self.unify(&lt, &rt)?;
                let t = self.resolve(&lt);
                let ok = matches!(&t, Ty::Bool | Ty::String | Ty::List(_))
                    || matches!(&t, Ty::Named(n, _) if n == "Option");
                if ok {
                    Ok(t)
                } else {
                    terr(format!(
                        "`||` needs Bool, String, Option, or List operands (the truthy fallback `a || b`), found `{t}`"
                    ))
                }
            }
        }
    }

    fn infer_match(&mut self, scrutinee: &Expr, arms: &[MatchArm]) -> Result<Ty, TypeError> {
        let st = self.infer(scrutinee)?;
        let result = self.fresh();
        let before = self.consumed.clone();
        let mut merged = before.clone();
        for arm in arms {
            self.consumed = before.clone();
            self.push();
            if let Some(dup) = pattern_dup_binding(&arm.pattern) {
                return terr(format!(
                    "pattern binds `{dup}` more than once — each binding in a pattern \
                     must have a distinct name (witchy has no equality patterns)"
                ));
            }
            self.check_pattern(&arm.pattern, &st)?;
            if let Some(guard) = &arm.guard {
                let gt = self.infer(guard)?;
                self.unify(&Ty::Bool, &gt)
                    .map_err(|e| TypeError { message: format!("match guard: {}", e.message) })?;
            }
            let bt = self.infer(&arm.body)?;
            self.unify(&result, &bt).map_err(|e| TypeError {
                message: format!("match arms produce different types: {}", e.message),
            })?;
            self.pop();
            merged = &merged | &self.consumed;
        }
        self.consumed = merged;
        self.check_unreachable(arms)?;
        self.check_exhaustive(&st, arms)?;
        Ok(result)
    }

    fn check_pattern(&mut self, pat: &Pattern, expected: &Ty) -> Result<(), TypeError> {
        match pat {
            Pattern::Wildcard => Ok(()),
            Pattern::Var(name) => {
                self.define(name.clone(), expected.clone(), false);
                Ok(())
            }
            Pattern::Int(_) => self.unify(expected, &Ty::Int),
            Pattern::Str(_) => self.unify(expected, &Ty::String),
            Pattern::Bool(_) => self.unify(expected, &Ty::Bool),
            Pattern::Tuple(pats) => {
                let elem_tys: Vec<Ty> = (0..pats.len()).map(|_| self.fresh()).collect();
                self.unify(expected, &Ty::Tuple(elem_tys.clone()))?;
                for (p, t) in pats.iter().zip(elem_tys) {
                    self.check_pattern(p, &t)?;
                }
                Ok(())
            }
            Pattern::List { elems, rest } => {
                let elem = self.fresh();
                self.unify(expected, &Ty::List(Box::new(elem.clone())))?;
                for p in elems {
                    self.check_pattern(p, &elem)?;
                }
                if let Some(Some(name)) = rest {
                    self.define(name.clone(), Ty::List(Box::new(elem)), false);
                }
                Ok(())
            }
            Pattern::Ctor { name, args } => {
                if let Some((fields, result)) = self.ctor_sigs.get(name).cloned() {
                    let typarams = self.ctor_typarams.get(name).cloned().unwrap_or_default();
                    let (fields, result) = self.instantiate(&fields, &result, &typarams);
                    self.unify(expected, &result)?;
                    if fields.len() != args.len() {
                        return terr(format!(
                            "pattern `{name}` takes {} field(s) but matched {}",
                            fields.len(),
                            args.len()
                        ));
                    }
                    for (p, fty) in args.iter().zip(&fields) {
                        self.check_pattern(p, fty)?;
                    }
                    Ok(())
                } else {
                    // Unknown constructor pattern: bind sub-patterns freely.
                    for p in args {
                        let v = self.fresh();
                        self.check_pattern(p, &v)?;
                    }
                    Ok(())
                }
            }
        }
    }

    /// Flag arms that can never match because an earlier arm already covers
    /// them — dead code that is almost always a bug (a typo'd duplicate, or arms
    /// placed after a catch-all). Conservative: a guarded arm never establishes
    /// coverage (its guard may fail at runtime), and a constructor arm only
    /// covers its variant when all its fields are irrefutable (`_`/binding), so
    /// `Some(0)` followed by `Some(n)` is correctly left reachable.
    fn check_unreachable(&self, arms: &[MatchArm]) -> Result<(), TypeError> {
        let mut saturated = false;
        let mut ctors: HashSet<&str> = HashSet::new();
        let mut ints: HashSet<i64> = HashSet::new();
        let mut strs: HashSet<&str> = HashSet::new();
        let mut bools: HashSet<bool> = HashSet::new();
        for arm in arms {
            let already = saturated
                || match &arm.pattern {
                    Pattern::Ctor { name, .. } => ctors.contains(name.as_str()),
                    Pattern::Int(n) => ints.contains(n),
                    Pattern::Str(s) => strs.contains(s.as_str()),
                    Pattern::Bool(b) => bools.contains(b),
                    _ => false,
                };
            if already {
                return terr(format!(
                    "unreachable match arm: `{}` is already covered by an earlier arm",
                    describe_pattern(&arm.pattern)
                ));
            }
            if arm.guard.is_none() {
                match &arm.pattern {
                    Pattern::Wildcard | Pattern::Var(_) => saturated = true,
                    Pattern::Ctor { name, args }
                        if args
                            .iter()
                            .all(|p| matches!(p, Pattern::Wildcard | Pattern::Var(_))) =>
                    {
                        ctors.insert(name.as_str());
                    }
                    Pattern::Int(n) => {
                        ints.insert(*n);
                    }
                    Pattern::Str(s) => {
                        strs.insert(s.as_str());
                    }
                    Pattern::Bool(b) => {
                        bools.insert(*b);
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// If the scrutinee is a known sum type, every variant must be covered (or a
    /// wildcard/variable arm must catch the rest). `Bool` is treated as a
    /// two-variant sum (`true`/`false`), so an incomplete Bool match is rejected
    /// just like an incomplete ADT match.
    fn check_exhaustive(&self, scrut: &Ty, arms: &[MatchArm]) -> Result<(), TypeError> {
        let resolved = self.resolve(scrut);
        let has_catchall = arms.iter().any(|a| {
            a.guard.is_none() && matches!(a.pattern, Pattern::Wildcard | Pattern::Var(_))
        });
        if has_catchall {
            return Ok(());
        }
        if matches!(resolved, Ty::Bool) {
            let covers = |want: bool| {
                arms.iter()
                    .any(|a| a.guard.is_none() && matches!(a.pattern, Pattern::Bool(b) if b == want))
            };
            if covers(true) && covers(false) {
                return Ok(());
            }
            return terr(
                "non-exhaustive match on `Bool`: cover both `true` and `false` (or add `_`)",
            );
        }
        // Infinite/large scalar domains can only be matched exhaustively with a
        // catch-all (an unguarded `_`/variable) — which we'd have accepted above.
        // Reaching here means there is none, so a literal-only or guard-only match
        // is non-exhaustive and would trap at runtime on an unlisted value.
        let scalar = match resolved {
            Ty::Int => Some("Int"),
            Ty::Float => Some("Float"),
            Ty::Duration => Some("Duration"),
            Ty::String => Some("String"),
            _ => None,
        };
        if let Some(kind) = scalar {
            return terr(format!(
                "non-exhaustive match on `{kind}`: it has no finite set of cases, \
                 so add a catch-all `_` arm (a guard does not make a match exhaustive)"
            ));
        }
        let Ty::Named(adt, _) = resolved else {
            return Ok(());
        };
        let Some(variants) = self.adt_variants.get(&adt) else {
            return Ok(());
        };
        // Top-level: every variant of the sum type must appear in an unguarded arm.
        let covered: HashSet<&str> = arms
            .iter()
            .filter(|a| a.guard.is_none())
            .filter_map(|a| match &a.pattern {
                Pattern::Ctor { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        let missing: Vec<&String> = variants.iter().filter(|v| !covered.contains(v.as_str())).collect();
        if !missing.is_empty() {
            let names = missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
            return terr(format!("non-exhaustive match on `{adt}`: missing {names}"));
        }
        // ...and each present variant must ALSO cover its fields, so a *nested*
        // non-exhaustive match (`Circle(Red)` without `Circle(Blue)`) is caught at
        // check time instead of trapping at runtime. `patterns_cover` reads each
        // nested type from the sub-patterns' own constructors and is permissive on
        // shapes it can't analyze, so it only ever rejects a provably-incomplete
        // match — never a valid one (which is what the earlier shortcut got wrong).
        for v in variants {
            let v_arms: Vec<&[Pattern]> = arms
                .iter()
                .filter(|a| a.guard.is_none())
                .filter_map(|a| match &a.pattern {
                    Pattern::Ctor { name, args } if name == v => Some(args.as_slice()),
                    _ => None,
                })
                .collect();
            if !v_arms.is_empty() && !self.variant_fields_covered(&v_arms) {
                return terr(format!(
                    "non-exhaustive match on `{adt}`: `{v}` is matched but its fields \
                     don't cover every case — add a wholesale `{v}(_)` arm or a `_`"
                ));
            }
        }
        Ok(())
    }

    /// Whether one variant's arms jointly cover all its field values. 0 fields →
    /// yes; 1 field → recurse on that column (the common `Some(_)`/`Circle(c)`
    /// case, checked precisely); ≥2 fields → permissive (a full product matrix is
    /// rare and error-prone, so we don't risk rejecting a valid enumeration).
    fn variant_fields_covered(&self, arms: &[&[Pattern]]) -> bool {
        match arms.first().map(|a| a.len()).unwrap_or(0) {
            0 => true,
            1 => {
                let col: Vec<&Pattern> = arms.iter().map(|a| &a[0]).collect();
                self.patterns_cover(&col)
            }
            _ => true,
        }
    }

    /// Whether a set of patterns at one position covers every value of its type.
    /// The type is read from the patterns themselves — a constructor names its
    /// ADT, a `Bool`/`Int`/`Str` literal names a scalar. Returns `true` whenever
    /// coverage cannot be DISPROVEN (lists, tuples, foreign constructors, mixed or
    /// empty rows), so it never rejects a valid match — only one it can prove is
    /// incomplete (a missing variant, or a scalar with no catch-all).
    fn patterns_cover(&self, pats: &[&Pattern]) -> bool {
        if pats.iter().any(|p| matches!(p, Pattern::Wildcard | Pattern::Var(_))) {
            return true;
        }
        if pats.iter().any(|p| matches!(p, Pattern::Bool(_))) {
            let has = |b: bool| pats.iter().any(|p| matches!(p, Pattern::Bool(x) if *x == b));
            return has(true) && has(false);
        }
        // A scalar literal column with no catch-all is an infinite domain — it can
        // never be exhaustively enumerated.
        if pats.iter().any(|p| matches!(p, Pattern::Int(_) | Pattern::Str(_))) {
            return false;
        }
        if !pats.is_empty() && pats.iter().all(|p| matches!(p, Pattern::Ctor { .. })) {
            let Pattern::Ctor { name: first, .. } = pats[0] else {
                return true;
            };
            let Some((_, result)) = self.ctor_sigs.get(first) else {
                return true;
            };
            let Ty::Named(adt, _) = result else {
                return true;
            };
            let Some(variants) = self.adt_variants.get(adt) else {
                return true;
            };
            for v in variants {
                let v_arms: Vec<&[Pattern]> = pats
                    .iter()
                    .filter_map(|p| match p {
                        Pattern::Ctor { name, args } if name == v => Some(args.as_slice()),
                        _ => None,
                    })
                    .collect();
                if v_arms.is_empty() || !self.variant_fields_covered(&v_arms) {
                    return false;
                }
            }
            return true;
        }
        true
    }

    fn check_function(&mut self, func: &Function) -> Result<(), TypeError> {
        borrow_escape_check(func)?;
        let (params, ret) = self.fn_sigs.get(&func.name).cloned().unwrap();
        self.scopes = vec![HashMap::new()];
        self.consumed.clear();
        self.current_ret = Some(ret.clone());
        self.cur_line = 0;
        for (param, ty) in func.params.iter().zip(&params) {
            // (RFC-0025) A `frozen` parameter is deeply immutable, so a mutable
            // convention (`var`/`own`, which exist to mutate/consume the argument)
            // contradicts it.
            if param.convention.binds_mutable() {
                if let Some(decl) = &param.ty {
                    if is_frozen_type(decl) {
                        return Err(TypeError {
                            message: format!(
                                "parameter `{}` of `{}` is `frozen` (deeply immutable) but its convention is mutable (`var`/`own`) — a frozen value cannot be mutated; use a plain (read-only) parameter",
                                param.name, func.name
                            ),
                        });
                    }
                }
            }
            self.define(param.name.clone(), ty.clone(), param.convention.binds_mutable());
        }
        let body = self.infer_block(&func.body)?;
        // A broader capability may be returned where a narrower one is declared
        // (`-> Net[Connect]` returning a full `Net`), mirroring call-argument
        // narrowing; `coerce_arg` falls back to unification for everything else.
        self.coerce_arg(&ret, &body).map_err(|e| TypeError {
            message: format!("function `{}` body: {}", func.name, e.message),
        })?;
        // Soundness: a declared type parameter must stay free (truly generic).
        // If the body pinned it to a concrete type, the signature is misleading.
        if let Some(typarams) = self.fn_typarams.get(&func.name).cloned() {
            for (pname, v) in typarams {
                let resolved = self.resolve(&Ty::Var(v));
                if !matches!(resolved, Ty::Var(_)) {
                    return terr(format!(
                        "function `{}`: type parameter `{pname}` is used as `{resolved}`, so it isn't generic",
                        func.name
                    ));
                }
            }
        }
        Ok(())
    }

}

/// Type-check a whole module. Returns the first error found.
pub fn check(module: &Module) -> Result<(), TypeError> {
    // Catch duplicate top-level functions before lowering, while `impl` methods
    // are still distinct from free functions (so overloaded methods aren't
    // mistaken for duplicates) and source lines are still available.
    check_unique_functions(module)?;

    // Lower named-field record construction (a no-op once the linker has done so,
    // but covers single-module paths like `check_str`).
    let recs = witchy_syntax::records::lower(module.clone()).map_err(|message| TypeError { message })?;

    // Trait/impl declarations are desugared to ordinary functions first, so the
    // checker only ever sees plain functions (a no-op for trait-free modules).
    // The checked flavor surfaces unsatisfiable dispatch ("`Float` does not
    // implement `Show`") instead of a post-lowering unknown-function error.
    let lowered = crate::traits::lower_checked(recs).map_err(|message| TypeError { message })?;
    run_check(&lowered, false).map(|_| ())
}

/// The resolved-type side table produced by `annotate`: expression identity
/// (`&Expr as *const _`) -> the concrete `Ty` the checker inferred, finalized
/// against the ending substitution. Entries exist only where the type is
/// fully concrete (no free variables) — consumers fall back where it is not.
#[derive(Default)]
pub struct TypeTable {
    types: HashMap<usize, Ty>,
}

impl TypeTable {
    pub fn type_of(&self, e: &Expr) -> Option<&Ty> {
        self.types.get(&(e as *const Expr as usize))
    }
}

/// Convert a resolved checker type to the surface `ast::Type` shape the
/// backends' type-directed machinery (eq/to_string shapes, valtypes)
/// consumes. None where no surface form exists (functions, free variables).
pub fn ty_to_ast(t: &Ty) -> Option<witchy_syntax::ast::Type> {
    use witchy_syntax::ast::Type as T;
    Some(match t {
        Ty::Int => T::Named("Int".into(), Vec::new()),
        Ty::Float => T::Named("Float".into(), Vec::new()),
        Ty::Duration => T::Named("Duration".into(), Vec::new()),
        Ty::String => T::Named("String".into(), Vec::new()),
        Ty::Bytes => T::Named("Bytes".into(), Vec::new()),
        Ty::Bool => T::Named("Bool".into(), Vec::new()),
        Ty::Nil => T::Named("Nil".into(), Vec::new()),
        Ty::Console => T::Named("Console".into(), Vec::new()),
        Ty::Clock => T::Named("Clock".into(), Vec::new()),
        Ty::Rand => T::Named("Rand".into(), Vec::new()),
        Ty::Env => T::Named("Env".into(), Vec::new()),
        Ty::Secret => T::Named("Secret".into(), Vec::new()),
        Ty::Exec => T::Named("Exec".into(), Vec::new()),
        Ty::Dir(_) => T::Named("Dir".into(), Vec::new()),
        Ty::File(_) => T::Named("File".into(), Vec::new()),
        Ty::Net(_) => T::Named("Net".into(), Vec::new()),
        Ty::Socket => T::Named("Socket".into(), Vec::new()),
        Ty::Listener => T::Named("Listener".into(), Vec::new()),
        Ty::BuildOut | Ty::BuildRead | Ty::BuildEnv | Ty::BuildNet | Ty::BuildExec => {
            return None
        }
        Ty::List(e) => T::Named("List".into(), vec![ty_to_ast(e)?]),
        Ty::Tuple(ts) => {
            T::Tuple(ts.iter().map(ty_to_ast).collect::<Option<Vec<_>>>()?)
        }
        Ty::Named(n, args) => T::Named(
            n.clone(),
            args.iter().map(ty_to_ast).collect::<Option<Vec<_>>>()?,
        ),
        Ty::Fn(..) | Ty::Var(_) => return None,
    })
}

fn ty_has_var(t: &Ty) -> bool {
    match t {
        Ty::Var(_) => true,
        Ty::List(e) => ty_has_var(e),
        Ty::Tuple(ts) => ts.iter().any(ty_has_var),
        Ty::Named(_, args) => args.iter().any(ty_has_var),
        Ty::Fn(ps, r) => ps.iter().any(ty_has_var) || ty_has_var(r),
        _ => false,
    }
}

/// Annotate an ALREADY-LOWERED module instance (the exact AST a consumer will
/// keep walking): the typed-lowering keystone (rfcs/language-evolution.md
/// Phase 0). Best-effort by contract — consumers only annotate modules that
/// already passed `check`, so any error here yields an empty table and the
/// consumer's own fallbacks apply.
pub fn annotate(module: &Module) -> TypeTable {
    match run_check(module, true) {
        Ok(Some(table)) => table,
        Err(e) => {
            if std::env::var_os("WITCHY_DEBUG_ANNOTATE").is_some() {
                eprintln!("annotate: checker error on lowered module: {e}");
            }
            TypeTable::default()
        }
        _ => TypeTable::default(),
    }
}

fn run_check(module: &Module, record: bool) -> Result<Option<TypeTable>, TypeError> {
    let module = &module;
    let mut c = Checker {
        type_record: if record { Some(HashMap::new()) } else { None },
        fn_sigs: HashMap::new(),
        fn_conventions: HashMap::new(),
        ctor_sigs: HashMap::new(),
        ctor_typarams: HashMap::new(),
        record_fields: HashMap::new(),
        sealed_types: HashSet::new(),
        adt_variants: HashMap::new(),
        fn_typarams: HashMap::new(),
        subst: HashMap::new(),
        next_var: 0,
        scopes: vec![HashMap::new()],
        consumed: HashSet::new(),
        region_locals: Vec::new(),
        current_ret: None,
        cur_line: 0,
    };

    // Pass 1: collect all signatures so definitions can refer to each other.
    for item in &module.items {
        match item {
            Item::Function(f) => {
                let mut vars: HashMap<String, Ty> = HashMap::new();
                let params: Vec<Ty> = f
                    .params
                    .iter()
                    .map(|p| match &p.ty {
                        Some(t) => c.to_ty_generic(t, &mut vars),
                        None => c.fresh(),
                    })
                    .collect();
                let ret = match &f.ret {
                    Some(t) => c.to_ty_generic(t, &mut vars),
                    None => c.fresh(),
                };
                c.fn_sigs.insert(f.name.clone(), (params, ret));
                let typarams: Vec<(String, u32)> = vars
                    .into_iter()
                    .filter_map(|(name, ty)| match ty {
                        Ty::Var(v) => Some((name, v)),
                        _ => None,
                    })
                    .collect();
                c.fn_typarams.insert(f.name.clone(), typarams);
                c.fn_conventions
                    .insert(f.name.clone(), f.params.iter().map(|p| p.convention).collect());
            }
            Item::Type(t) => {
                // A type's parameters: explicit ones (`type Step(m, a):`) FIX the
                // order; otherwise infer them from the variant field types in order
                // of first appearance (so `type Option { Some(a) None }` has one
                // param `a`). Explicit params are required when a constructor omits
                // one (e.g. `Done(a)` for `Step(m, a)`): inference would drop the
                // omitted `m` from that constructor's result type, mis-aligning it.
                let mut param_names: Vec<String> = t.params.clone();
                for variant in &t.variants {
                    for ft in &variant.fields {
                        collect_type_params(ft, &mut param_names);
                    }
                }
                let mut vars: HashMap<String, Ty> = HashMap::new();
                let mut typaram_ids: HashSet<u32> = HashSet::new();
                let mut params_in_order: Vec<u32> = Vec::new();
                let mut result_args: Vec<Ty> = Vec::new();
                for pn in &param_names {
                    let v = c.fresh();
                    if let Ty::Var(id) = v {
                        typaram_ids.insert(id);
                        params_in_order.push(id);
                    }
                    vars.insert(pn.clone(), v.clone());
                    result_args.push(v);
                }
                let result = Ty::Named(t.name.clone(), result_args);
                let mut names = Vec::new();
                for variant in &t.variants {
                    let fields: Vec<Ty> = variant
                        .fields
                        .iter()
                        .map(|ft| c.to_ty_generic(ft, &mut vars))
                        .collect();
                    // A record variant carries field names: remember them (with
                    // the type's parameters) so `value.field` can be typed.
                    if !variant.field_names.is_empty() {
                        let rec: Vec<(String, Ty)> = variant
                            .field_names
                            .iter()
                            .cloned()
                            .zip(fields.iter().cloned())
                            .collect();
                        c.record_fields
                            .insert(t.name.clone(), (params_in_order.clone(), rec));
                        if t.sealed {
                            c.sealed_types.insert(t.name.clone());
                        }
                    }
                    c.ctor_sigs
                        .insert(variant.name.clone(), (fields, result.clone()));
                    c.ctor_typarams
                        .insert(variant.name.clone(), typaram_ids.clone());
                    names.push(variant.name.clone());
                }
                c.adt_variants.insert(t.name.clone(), names);
            }
            // Desugared to functions by `traits::lower` and constants inlined by
            // `witchy_syntax::consts` before this point.
            Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }

    // Reject typo'd / undeclared type names in signatures before they become
    // opaque types that mis-unify with a confusing message later.
    check_type_names(module)?;

    // `main` is the root entrypoint: its parameters are where the host's authority
    // enters, so they must be capabilities (or the args list) — validate before
    // diving into bodies so a malformed entry point is reported up front.
    check_main_signature(module)?;
    // RFC-0038: a `grantable` capability must be BARE (no transitive host authority),
    // checked module-wide (a grantable cap is invalid regardless of `main`).
    check_grantable_caps(module)?;
    // RFC-0040: a cap-gated string export must lead with a bare grantable capability.
    check_export_signatures(module)?;
    // A rune's `build` entrypoint is the root of the build sandbox; its parameters
    // are where build-time authority enters, so they must be build capabilities.
    check_build_signature(module)?;

    // Pass 2: check bodies. `where`-bounded templates are checked through
    // their monomorphic instantiations (trait-method calls in a template
    // body only resolve once the bound variable is concrete) — the lowering
    // extracts them before `check` historically; `annotate` runs earlier in
    // the pipeline and skips them here for the same reason.
    for item in &module.items {
        match item {
            Item::Function(f) if !f.bounds.is_empty() => {}
            Item::Function(f) => {
                c.check_function(f).map_err(|e| at_loc(e, c.cur_line, &f.name))?
            }
            Item::Type(_) | Item::Trait(_) | Item::Impl(_) | Item::Const { .. } | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    if let Some(rec) = c.type_record.take() {
        let mut types = HashMap::new();
        for (k, ty) in rec {
            let resolved = c.resolve(&ty);
            if !ty_has_var(&resolved) {
                types.insert(k, resolved);
            }
        }
        return Ok(Some(TypeTable { types }));
    }
    Ok(None)
}

/// The first name a pattern binds twice, if any. A pattern like `P(x, x)` must be
/// a compile error: witchy has no non-linear patterns (binding the same name in
/// two positions would silently shadow rather than constrain the two to be
/// equal), so we reject it rather than pick a winner.
fn pattern_dup_binding(p: &Pattern) -> Option<String> {
    fn walk(p: &Pattern, seen: &mut HashSet<String>, dup: &mut Option<String>) {
        if dup.is_some() {
            return;
        }
        match p {
            Pattern::Var(n) => {
                if !seen.insert(n.clone()) {
                    *dup = Some(n.clone());
                }
            }
            Pattern::Tuple(ps) => ps.iter().for_each(|q| walk(q, seen, dup)),
            Pattern::Ctor { args, .. } => args.iter().for_each(|q| walk(q, seen, dup)),
            Pattern::List { elems, rest } => {
                elems.iter().for_each(|q| walk(q, seen, dup));
                if let Some(Some(name)) = rest {
                    if !seen.insert(name.clone()) {
                        *dup = Some(name.clone());
                    }
                }
            }
            _ => {}
        }
    }
    let mut seen = HashSet::new();
    let mut dup = None;
    walk(p, &mut seen, &mut dup);
    dup
}

/// The module-qualified NATIVE INTRINSICS: declared in std as self-recursive
/// placeholders (signatures for the checker), intercepted by name on both
/// backends, never templated by monomorphization, never compiled as bodies.
pub fn intrinsic(name: &str) -> bool {
    matches!(
        name,
        "list.push" | "list.at" | "list.length" | "list.concat"
            | "dict.new" | "dict.insert" | "dict.get_or" | "dict.contains_key" | "dict.remove"
            | "dict.update" | "dict.keys" | "dict.values" | "dict.pairs" | "dict.length"
            | "string.split" | "string.trim" | "string.contains" | "string.starts_with"
            | "string.ends_with" | "string.replace" | "string.index_of" | "string.substring"
            | "string.length" | "string.char_count" | "string.chars" | "string.to_upper"
            | "string.to_lower" | "string.to_int"
            | "math.to_float" | "math.to_int" | "math.sqrt"
    )
}


/// Convenience: parse then type-check.
pub fn check_str(src: &str) -> Result<(), String> {
    let module = witchy_syntax::parser::parse_module(src).map_err(|e| e.to_string())?;
    check(&module).map_err(|e| e.to_string())
}

/// A short, human-readable rendering of a pattern for diagnostics.
fn describe_pattern(p: &Pattern) -> String {
    match p {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Var(n) => n.clone(),
        Pattern::Int(n) => n.to_string(),
        Pattern::Str(s) => format!("\"{s}\""),
        Pattern::Bool(b) => b.to_string(),
        Pattern::Ctor { name, .. } => name.clone(),
        Pattern::Tuple(_) => "tuple pattern".to_string(),
        Pattern::List { .. } => "list pattern".to_string(),
    }
}

#[cfg(test)]
#[path = "typeck_tests.rs"]
mod tests;
