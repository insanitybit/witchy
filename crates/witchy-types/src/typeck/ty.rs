//! The checker's type representation (`Ty`), `TypeError`, and error-context helpers.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    Int,
    Float,
    /// A length of time (from a duration literal like `30s`). A distinct type:
    /// it does not mix with `Int` under arithmetic.
    Duration,
    String,
    Bytes,
    /// (RFC-0055) The erased message type of the concurrency executor. Opaque and
    /// representationally the universal slot (like a generic type variable, and
    /// like `Bytes` shares `String`'s layout): the `std/chan`/`std/task` executor
    /// buffers, `Step`, and `Slot` are monomorphic over it, while typed channel
    /// endpoints (`Sender(m)`/`Receiver(m)`) erase/unerase at the boundary via the
    /// non-inferable `__erase`/`__unerase` intrinsics, confined to `std/chan`. The
    /// spelling `__Msg` is reserved (double-underscore) so it never collides with a
    /// user `type Msg`.
    Msg,
    Bool,
    Unit,
    Console(ConsoleRights),
    Clock,
    /// The runtime authority to draw cryptographic randomness (`rand_u64`).
    Rand,
    Env,
    /// (RFC-0121) A host-granted secret, decomposed by right: `Secret[Seal]` is
    /// usable only by opaque handle, bare `Secret` additionally permits `reveal`.
    Secret(SecretRights),
    /// The runtime authority to spawn a native subprocess. Right-less (one op):
    /// the executable is named and confined through a `Dir[Read]` argument, so
    /// "you can only execute a file you can read". See rfcs/0004-self-hosted-cli.md.
    Exec,
    /// Origin-scoped authority to perform host HTTP(S) requests.
    Fetch,
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
    // RFC-0164 Phase 1: these five recursive variants used to own their
    // payload directly (`Box<Ty>`/`String`/`Vec<Ty>`), so the derived `Clone`
    // below did a full deep copy on every call -- profiled as a dominant
    // compile-time cost (`Ty::clone` and its drop-glue as hot leaves, the
    // checker's annotate/mono fixpoint re-cloning the same types across
    // rounds). `Rc` makes `.clone()` here an O(1) refcount bump; every
    // existing `.clone()` call site needed zero changes. Read-only usage
    // (`match`, iteration, indexing, `==`) is unaffected by Deref coercion;
    // a site that needs an OWNED `String`/`Vec<Ty>` uses `.to_string()`/
    // `.to_vec()`, and a construction site uses `.into()`.
    List(Rc<Ty>),
    Tuple(Rc<[Ty]>),
    /// A user-declared type, possibly with type arguments: `Option(Int)`,
    /// `Result(String, Error)`. Non-generic types carry an empty argument list.
    Named(Rc<str>, Rc<[Ty]>),
    /// (RFC-0081) A first-class existential identity: the RESOLVED bare trait
    /// name plus its fully substituted type arguments (`dyn Render`,
    /// `dyn Convert(Int)`). Never a guessed type name: the head comes from the
    /// checked `ast::Type::Dyn` whose trait was validated against the module's
    /// trait declarations, and the arguments recurse through the ordinary type
    /// resolution, so aliases and cross-module uses of the same instantiation
    /// share one identity. Two existentials unify only on the same head, same
    /// arity, and pairwise-unifying arguments — `dyn Sub` never unifies with
    /// `dyn Super` in this slice.
    Dyn(Rc<str>, Rc<[Ty]>),
    /// A function type: parameter types, return type, conventions, and explicit
    /// mutable-reference positions.  The final vector is semantic identity, not
    /// an ABI convention: `let x: &'a mut T` accepts `&mut place`, whereas a
    /// `var x: T` accepts a caller write-back place.
    Fn(Rc<[Ty]>, Rc<Ty>, Rc<[Convention]>, Rc<[bool]>, CallableQualifiers),
    /// RFC-0167 immutable rooted-view carrier.  This is deliberately distinct
    /// from its payload: a view copies a retained root-plus-projection handle,
    /// never an owned payload or a lexical loan relation.
    View(Rc<Ty>),
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
            Ty::Msg => write!(f, "__Msg"),
            Ty::Bool => write!(f, "Bool"),
            Ty::Unit => write!(f, "()"),
            Ty::Console(rights) => write!(f, "{rights}"),
            Ty::Clock => write!(f, "Clock"),
            Ty::Rand => write!(f, "Rand"),
            Ty::Env => write!(f, "Env"),
            Ty::Secret(r) => write!(f, "{r}"),
            Ty::Exec => write!(f, "Exec"),
            Ty::Fetch => write!(f, "Fetch"),
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
            // RFC-0167: `&[T]` is the only source spelling of a slice view;
            // the internal `Slice` name never reaches a diagnostic.
            Ty::Named(n, args) if n.as_ref() == "Slice" && args.len() == 1 => write!(f, "&[{}]", args[0]),
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
            // (RFC-0081) Canonical existential rendering, matching the
            // formatter: `dyn Render`, `dyn Convert(Int)`.
            Ty::Dyn(n, args) => {
                write!(f, "dyn {n}")?;
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
            Ty::Fn(params, ret, conventions, _, qualifiers) => {
                if qualifiers.pure {
                    write!(f, "pure ")?;
                }
                if qualifiers.once {
                    write!(f, "once ")?;
                }
                write!(f, "fn(")?;
                for (i, t) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    let prefix = match conventions.get(i).copied().unwrap_or_default() {
                        Convention::Let => "",
                        Convention::Borrow => "let ",
                        Convention::Var => "var ",
                        Convention::Own => "own ",
                    };
                    write!(f, "{prefix}{t}")?;
                }
                write!(f, ") -> {ret}")
            }
            Ty::View(payload) => write!(f, "&{payload}"),
            Ty::Var(_) => write!(f, "?"),
        }
    }
}

pub(super) fn collect_callable_lifetime_markers(ty: &Ty, markers: &mut HashMap<String, String>) {
    match ty {
        Ty::List(element) => collect_callable_lifetime_markers(element, markers),
        Ty::View(payload) => collect_callable_lifetime_markers(payload, markers),
        Ty::Tuple(items) | Ty::Named(_, items) | Ty::Dyn(_, items) => {
            if let Ty::Named(name, arguments) = ty
                && arguments.is_empty()
                && name.starts_with('\'')
                && !markers.contains_key(name.as_ref())
            {
                let canonical = format!("'__witchy_bound_{}", markers.len());
                markers.insert(name.to_string(), canonical);
                return;
            }
            for item in items.iter() {
                collect_callable_lifetime_markers(item, markers);
            }
        }
        // Nested function types introduce their own binders.
        Ty::Fn(..)
        | Ty::Int
        | Ty::Float
        | Ty::Duration
        | Ty::String
        | Ty::Bytes
        | Ty::Msg
        | Ty::Bool
        | Ty::Unit
        | Ty::Console(_)
        | Ty::Clock
        | Ty::Rand
        | Ty::Env
        | Ty::Secret(_)
        | Ty::Exec
        | Ty::Fetch
        | Ty::Dir(_)
        | Ty::File(_)
        | Ty::Net(_)
        | Ty::Socket
        | Ty::Listener
        | Ty::BuildOut
        | Ty::BuildRead
        | Ty::BuildEnv
        | Ty::BuildNet
        | Ty::BuildExec
        | Ty::Var(_) => {}
    }
}

pub(super) fn normalize_callable_lifetime_markers(
    ty: &Ty,
    markers: &HashMap<String, String>,
) -> Ty {
    match ty {
        Ty::List(element) => Ty::List(Box::new(normalize_callable_lifetime_markers(
            element, markers,
        )).into()),
        Ty::View(payload) => Ty::View(
            Box::new(normalize_callable_lifetime_markers(payload, markers)).into(),
        ),
        Ty::Tuple(items) => Ty::Tuple(
            items
                .iter()
                .map(|item| normalize_callable_lifetime_markers(item, markers))
                .collect(),
        ),
        Ty::Named(name, arguments) => {
            if arguments.is_empty()
                && let Some(canonical) = markers.get(name.as_ref())
            {
                return Ty::Named(canonical.clone().into(), Vec::new().into());
            }
            Ty::Named(
                name.clone(),
                arguments
                    .iter()
                    .map(|argument| normalize_callable_lifetime_markers(argument, markers))
                    .collect(),
            )
        }
        Ty::Dyn(name, arguments) => Ty::Dyn(
            name.clone(),
            arguments
                .iter()
                .map(|argument| normalize_callable_lifetime_markers(argument, markers))
                .collect(),
        ),
            Ty::Fn(parameters, result, conventions, reference_params, qualifiers) => {
            let (parameters, result) = alpha_normalize_callable(parameters, result);
                Ty::Fn(parameters.into(), Box::new(result).into(), conventions.clone(), reference_params.clone(), *qualifiers)
        }
        other => other.clone(),
    }
}

pub(super) fn alpha_normalize_callable(parameters: &[Ty], result: &Ty) -> (Vec<Ty>, Ty) {
    let mut markers = HashMap::new();
    for parameter in parameters {
        collect_callable_lifetime_markers(parameter, &mut markers);
    }
    let parameters = parameters
        .iter()
        .map(|parameter| normalize_callable_lifetime_markers(parameter, &markers))
        .collect();
    let result = normalize_callable_lifetime_markers(result, &markers);
    (parameters, result)
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

pub(super) fn terr<T>(message: impl Into<String>) -> Result<T, TypeError> {
    Err(TypeError {
        message: message.into(),
    })
}

pub(super) fn in_call_context(display: &str, e: TypeError) -> TypeError {
    if e.message.starts_with("expected `") {
        TypeError { message: format!("in call to `{display}`: {}", e.message) }
    } else {
        e
    }
}

pub(super) fn type_mismatch_context(context: impl FnOnce() -> String, e: TypeError) -> TypeError {
    if e.message.starts_with("expected `") {
        TypeError { message: format!("{}: {}", context(), e.message) }
    } else {
        e
    }
}

/// Prefix a type error with where it occurred — the enclosing function (after
/// linking this is `module.func`, which also names the file) and source line.
/// `line == 0` means no line is available; an empty `func` omits the name.
pub(super) fn at_loc(e: TypeError, line: u32, func: &str, home: &str) -> TypeError {
    if line == 0 {
        return e;
    }
    let where_ = if func.is_empty() {
        format!("line {line}")
    } else {
        format!("`{}`, line {line}", diagnostic_callable_name(func))
    };
    // The location prefix already names the home module, so render home-module
    // type/variant names bare in the body — the spelling the reader wrote — while
    // keeping cross-module qualifiers (BUG-292).
    TypeError {
        message: format!("{where_}: {}", strip_home_qualifiers(&e.message, home)),
    }
}
