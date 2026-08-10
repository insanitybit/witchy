//! The builtin-call dispatcher: `Interpreter::call_builtin`, the large match
//! over intrinsic and stdlib builtin names. Split out of interpreter.rs as an
//! impl continuation; a child module keeps full access to the interpreter's
//! private fields and helpers.

use std::io::{BufReader, Read, Write};
use std::net::TcpListener;
use std::rc::Rc;

use witchy_runtime::net::Stream;
use witchy_syntax::ast::*;
use witchy_syntax::diag::DiagTemplate;
use witchy_syntax::intrinsics;

use super::*;

impl Interpreter {
    pub(super) fn call_builtin(&mut self, name: &str, args: &[Value]) -> Result<Option<Value>, RuntimeError> {
        let catalog = intrinsics::lookup(name);
        if let Some(spec) = catalog {
            if args.len() != spec.arity {
                return err(intrinsics::arity_diagnostic(spec, args.len()));
            }
        }
        // `secret_store.get(name)` — a named lookup into the granted store. Handled
        // here (not in `native`) because a `SecretStore` is not a `NativeValue`.
        if name == intrinsics::SECRETSTORE_GET {
            return match args {
                #[cfg(feature = "test-fixtures")]
                [Value::FixtureSecretStore(store), Value::Str(key)] => {
                    match self.invoke_fixture(witchy_test_host::HostRequest::SecretStoreLookup {
                        store: *store,
                        name: key.to_string(),
                    })? {
                        witchy_test_host::HostResponse::OptionalHandle(secret) => Ok(Some(
                            match secret {
                                Some(secret) => Value::Ctor {
                                    name: "Some".into(),
                                    fields: Rc::new(vec![Value::FixtureSecret(secret)]),
                                },
                                None => Value::ctor("None", Vec::new()),
                            },
                        )),
                        response => err(format!(
                            "fixture SecretStore lookup returned unexpected response {response:?}"
                        )),
                    }
                }
                [Value::SecretStore(map), Value::Str(key)] => Ok(Some(match map.get(key.as_str()) {
                    Some((bytes, sealed)) => Value::Ctor {
                        name: "Some".into(),
                        fields: Rc::new(vec![Value::Secret(bytes.clone(), *sealed)]),
                    },
                    None => Value::ctor("None", Vec::new()),
                })),
                _ => err(format!(
                    "{} expects (SecretStore, name)",
                    intrinsics::SECRETSTORE_GET
                )),
            };
        }
        // `__try_ctx(value, msg)` — the `e ? "msg"` desugar. Turn the operand (an
        // `Option` or a `Result`) into a `Result(T, String)` carrying `msg`: `None`
        // -> `Err(msg)`, a `Result`'s `Err(e)` -> `Err("msg: e")` (e is a String),
        // and `Some(x)`/`Ok(x)` -> `Ok(x)`. The enclosing `?` then unwraps it.
        if name == intrinsics::TRY_CONTEXT {
            return match args {
                [val, Value::Str(msg)] => {
                    let out = match val {
                        Value::Ctor { name: c, fields } if &**c == "Some" || &**c == "Ok" => {
                            Value::Ctor { name: "Ok".into(), fields: fields.clone() }
                        }
                        Value::Ctor { name: c, .. } if &**c == "None" => {
                            Value::ctor("Err", vec![Value::Str(msg.clone())])
                        }
                        Value::Ctor { name: c, fields } if &**c == "Err" => {
                            let inner = match fields.first() {
                                Some(Value::Str(e)) => (**e).clone(),
                                Some(other) => format!("{other}"),
                                None => String::new(),
                            };
                            Value::Ctor {
                                name: "Err".into(),
                                fields: Rc::new(vec![Value::str(format!("{msg}: {inner}"))]),
                            }
                        }
                        _ => return err("`? \"msg\"` applies to an Option or Result"),
                    };
                    Ok(Some(out))
                }
                _ => err(format!("{} expects (value, message)", intrinsics::TRY_CONTEXT)),
            };
        }
        // `secret_store.require(name)` — a required secret: the `Secret` directly,
        // or a loud error if absent (a configuration mistake, not an `Option`).
        if name == intrinsics::SECRETSTORE_REQUIRE {
            return match args {
                #[cfg(feature = "test-fixtures")]
                [Value::FixtureSecretStore(store), Value::Str(key)] => {
                    match self.invoke_fixture(witchy_test_host::HostRequest::SecretStoreRequire {
                        store: *store,
                        name: key.to_string(),
                    })? {
                        witchy_test_host::HostResponse::Handle(secret) => {
                            Ok(Some(Value::FixtureSecret(secret)))
                        }
                        response => err(format!(
                            "fixture SecretStore require returned unexpected response {response:?}"
                        )),
                    }
                }
                [Value::SecretStore(map), Value::Str(key)] => match map.get(key.as_str()) {
                    Some((bytes, sealed)) => Ok(Some(Value::Secret(bytes.clone(), *sealed))),
                    None => err(format!("required secret `{key}` was not granted")),
                },
                _ => err(format!(
                    "{} expects (SecretStore, name)",
                    intrinsics::SECRETSTORE_REQUIRE
                )),
            };
        }
        // `crypto.reveal` is gated: a `Secret` equal to the signing key (the bare
        // `Secret` / `require("signing")`) is sign-only and must not be revealed —
        // only named value-secrets are. Mirrors the WASM host (`host_crypto_reveal_len`)
        // through the one shared identity rule so the backends can't drift.
        if name == intrinsics::CRYPTO_REVEAL {
            #[cfg(feature = "test-fixtures")]
            if let [Value::FixtureSecret(secret)] = args {
                return match self.invoke_fixture(witchy_test_host::HostRequest::SecretReveal {
                    secret: *secret,
                })? {
                    witchy_test_host::HostResponse::String(value) => Ok(Some(Value::str(value))),
                    response => err(format!(
                        "fixture secret reveal returned unexpected response {response:?}"
                    )),
                };
            }
            if let [Value::Secret(bytes, sealed)] = args {
                // (RFC-0060) A sealed secret is consumable by handle but never revealable.
                if *sealed {
                    return err(witchy_caps::capabilities::SEALED_SECRET_REVEAL_ERROR);
                }
                if witchy_caps::capabilities::secret_is_signing_key(
                    self.signing_key.as_ref().map(|s| s.as_slice()),
                    bytes,
                ) {
                    return err("the signing key is not revealable; use crypto.sign / crypto.public_key");
                }
            }
        }
        #[cfg(feature = "test-fixtures")]
        if name == intrinsics::CRYPTO_SIGN
            && let [Value::FixtureSecret(secret), Value::Str(message)] = args
        {
            return match self.invoke_fixture(witchy_test_host::HostRequest::SecretSign {
                secret: *secret,
                message: message.to_string(),
            })? {
                witchy_test_host::HostResponse::String(value) => {
                    Ok(Some(Value::str(value)))
                }
                response => err(format!(
                    "fixture secret sign returned unexpected response {response:?}"
                )),
            };
        }
        #[cfg(feature = "test-fixtures")]
        if name == intrinsics::CRYPTO_PUBLIC_KEY
            && let [Value::FixtureSecret(secret)] = args
        {
            return match self.invoke_fixture(witchy_test_host::HostRequest::SecretPublicKey {
                secret: *secret,
            })? {
                witchy_test_host::HostResponse::String(value) => {
                    Ok(Some(Value::str(value)))
                }
                response => err(format!(
                    "fixture secret public key returned unexpected response {response:?}"
                )),
            };
        }
        // Native stdlib modules (crypto, …): pure, stateless functions reached by
        // their qualified name (`crypto.sha256`). Dispatched through the registry
        // so adding one needs no change here — see `src/native.rs`.
        // Ensure the compiler-service natives (installed from this crate,
        // above the runtime kernel) are resolvable before the registry lookup.
        crate::compiler_natives::install();
        if let Some(f) = witchy_runtime::native::lookup(name) {
            // `native` speaks `NativeValue` (it doesn't depend on the interpreter);
            // bridge our `Value` across the call.
            let nargs = args
                .iter()
                .map(value_to_native)
                .collect::<Result<Vec<_>, RuntimeError>>()?;
            let nresult = f(&nargs).map_err(|e| RuntimeError { message: e.message })?;
            return Ok(Some(native_to_value(nresult)));
        }
        if catalog.is_some_and(|spec| spec.runtime == intrinsics::IntrinsicRuntime::Native) {
            return err(format!("internal error: cataloged native operation `{name}` has no runtime hook"));
        }
        let one = |args: &[Value]| -> Result<Value, RuntimeError> {
            match args {
                [v] => Ok(v.clone()),
                _ => err(format!("`{name}` expects exactly one argument")),
            }
        };
        match name {
            // Effectful: requires the Console capability as its first argument.
            "print" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Cap(Capability::Console), msg]
                    if self.fixture_host.is_some() =>
                {
                    let line = msg
                        .to_string()
                        .trim_end_matches('\n')
                        .to_owned();
                    match self.invoke_fixture(HostRequest::ConsoleWrite {
                        text: line.clone(),
                    })? {
                        HostResponse::Unit => {
                            self.output.push(line);
                            Ok(Some(Value::Unit))
                        }
                        other => err(format!(
                            "internal error: Console fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Cap(Capability::Console), msg] => {
                    // Each print is one output line; the trailing newline is the
                    // line terminator. Strip it to match the WASM host
                    // (`host_print` in runtime.rs), so the backends agree when a
                    // printed string ends in `\n` (e.g. `s + "\n"`).
                    self.output.push(msg.to_string().trim_end_matches('\n').to_string());
                    Ok(Some(Value::Unit))
                }
                [_, _] => err("print requires a Console capability as its first argument"),
                _ => err("print expects a Console capability and a message: console.print(msg)"),
            },
            "read_line" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Cap(Capability::Console)]
                    if self.fixture_host.is_some() =>
                {
                    match self.invoke_fixture(HostRequest::ConsoleRead)? {
                        HostResponse::String(line) => Ok(Some(Value::str(
                            line.trim_end_matches(['\r', '\n']).to_owned(),
                        ))),
                        other => err(format!(
                            "internal error: Console fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Cap(Capability::Console)] => {
                    let line = match self.console_input.as_mut() {
                        Some(lines) => lines.pop_front().unwrap_or_default(),
                        None => {
                            let mut line = String::new();
                            std::io::stdin()
                                .read_line(&mut line)
                                .map_err(|error| RuntimeError {
                                    message: format!("failed to read Console input: {error}"),
                                })?;
                            line
                        }
                    };
                    Ok(Some(Value::str(
                        line.trim_end_matches(['\r', '\n']).to_string(),
                    )))
                }
                [_] => err("read_line requires a Console capability as its first argument"),
                _ => err("read_line expects exactly one argument"),
            },
            name if intrinsics::is_meta_fresh_ident(name) => match one(args)? {
                Value::Str(hint) => Ok(Some(Value::str(self.next_fresh_ident(hint.as_str())?))),
                other => err(format!("meta.fresh expects a String hint, got `{other}`")),
            },
            name if intrinsics::is_meta_expr_leaf(name) => match args {
                [Value::Str(kind), Value::Str(payload)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err("meta expression builders are available only during compile-time expansion");
                    }
                    let expr = compiler_expr_leaf(kind, payload)?;
                    Ok(Some(self.store_compiler_expr_syntax(
                        "expression-leaf",
                        expr,
                        Vec::new(),
                    )?))
                }
                _ => err("meta expression builder expects a kind and scalar payload"),
            },
            name if intrinsics::is_meta_pattern_leaf(name) => match args {
                [Value::Str(kind), Value::Str(first), Value::Str(second), Value::Bool(inclusive)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err("meta pattern builders are available only during compile-time expansion");
                    }
                    let pattern = compiler_pattern_leaf(kind, first, second, *inclusive)?;
                    Ok(Some(self.store_compiler_pattern_syntax(
                        "pattern-leaf",
                        pattern,
                        Vec::new(),
                    )?))
                }
                _ => err("meta pattern builder expects a kind and scalar payloads"),
            },
            name if intrinsics::is_meta_stmt_leaf(name) => match args {
                [Value::Str(kind)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err("meta statement builders are available only during compile-time expansion");
                    }
                    let stmt = compiler_stmt_leaf(kind)?;
                    Ok(Some(self.store_compiler_stmt_syntax("statement-leaf", stmt)?))
                }
                _ => err("meta statement builder expects a statement kind"),
            },
            name if intrinsics::is_meta_call_site_expr(name) => match one(args)? {
                Value::Str(name) => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.call_site is available only during compile-time expansion",
                        );
                    }
                    let expr = witchy_syntax::linker::call_site_expr(name.as_str());
                    let handle = self.next_compiler_syntax_handle("call-site-expression")?;
                    self.compiler_expr_syntax.insert(handle.clone(), expr);
                    self.compiler_expr_origins.insert(
                        handle.clone(),
                        ComptimeSyntaxOrigin {
                            definition_line: self.cur_line,
                            hole_ancestry: Vec::new(),
                        },
                    );
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerExprSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::Str(name)]),
                    }))
                }
                other => err(format!("meta.call_site expects a String name, got `{other}`")),
            },
            name if intrinsics::is_meta_call_site_type(name) => match args {
                [Value::Str(name), Value::List(args)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.call_site is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        args,
                        SyntaxCategory::Type,
                        "CompilerTypeSyntax",
                        &self.compiler_type_origins,
                        self.cur_line,
                    );
                    let args = compiler_type_holes(args, &self.compiler_type_syntax)?;
                    let source = witchy_syntax::linker::call_site_type_source(name, &args);
                    let ty = witchy_syntax::linker::call_site_type(name, args);
                    let handle = self.next_compiler_syntax_handle("call-site-type")?;
                    self.compiler_type_syntax.insert(handle.clone(), ty);
                    self.compiler_type_origins.insert(
                        handle.clone(),
                        ComptimeSyntaxOrigin {
                            definition_line: self.cur_line,
                            hole_ancestry,
                        },
                    );
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerTypeSyntax".into(),
                        fields: Rc::new(vec![
                            Value::str(handle),
                            Value::str(source),
                        ]),
                    }))
                }
                _ => err("meta.call_site type construction expects a name and type arguments"),
            },
            name if intrinsics::is_meta_type_named(name) => match args {
                [Value::Str(name), Value::List(args)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.type_named is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        args,
                        SyntaxCategory::Type,
                        "CompilerTypeSyntax",
                        &self.compiler_type_origins,
                        self.cur_line,
                    );
                    let args = compiler_type_holes(args, &self.compiler_type_syntax)?;
                    let ty = Type::Named(name.to_string(), args);
                    Ok(Some(self.store_compiler_type_syntax(
                        "named-type",
                        ty,
                        hole_ancestry,
                    )?))
                }
                _ => err("meta.type_named expects a name and type arguments"),
            },
            name if intrinsics::is_meta_type_tuple(name) => match args {
                [Value::List(types)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.type_tuple is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        types,
                        SyntaxCategory::Type,
                        "CompilerTypeSyntax",
                        &self.compiler_type_origins,
                        self.cur_line,
                    );
                    let types = compiler_type_holes(types, &self.compiler_type_syntax)?;
                    Ok(Some(self.store_compiler_type_syntax(
                        "tuple-type",
                        Type::Tuple(types),
                        hole_ancestry,
                    )?))
                }
                _ => err("meta.type_tuple expects List(TypeSyntax)"),
            },
            name if intrinsics::is_meta_type_fn(name) => match args {
                [Value::List(params), conventions, ret] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.type_fn is available only during compile-time expansion",
                        );
                    }
                    let mut inputs = params.to_vec();
                    inputs.push(ret.clone());
                    let hole_ancestry = compiler_direct_hole_origins(
                        &inputs,
                        SyntaxCategory::Type,
                        "CompilerTypeSyntax",
                        &self.compiler_type_origins,
                        self.cur_line,
                    );
                    let params = compiler_type_holes(params, &self.compiler_type_syntax)?;
                    let conventions =
                        compiler_function_conventions(conventions, params.len(), "meta.type_fn")?;
                    let ret = compiler_type_syntax_value(ret, &self.compiler_type_syntax)?;
                    Ok(Some(self.store_compiler_type_syntax(
                        "function-type",
                        Type::Fn(params, Box::new(ret), conventions),
                        hole_ancestry,
                    )?))
                }
                _ => err(
                    "meta.type_fn expects List(TypeSyntax), List(String), and TypeSyntax",
                ),
            },
            name if intrinsics::is_meta_type_qualified(name) => match args {
                [Value::Str(qualifier), ty] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.type_qualified is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        std::slice::from_ref(ty),
                        SyntaxCategory::Type,
                        "CompilerTypeSyntax",
                        &self.compiler_type_origins,
                        self.cur_line,
                    );
                    let qualifier = match qualifier.as_str() {
                        "frozen" => TypeQual::Frozen,
                        "unique" => TypeQual::Unique,
                        "local unique" => TypeQual::LocalUnique,
                        other => {
                            return err(format!(
                                "meta.type_qualified unknown qualifier `{other}`"
                            ));
                        }
                    };
                    let ty = compiler_type_syntax_value(ty, &self.compiler_type_syntax)?;
                    Ok(Some(self.store_compiler_type_syntax(
                        "qualified-type",
                        Type::Qualified(qualifier, Box::new(ty)),
                        hole_ancestry,
                    )?))
                }
                _ => err("meta.type_qualified expects a qualifier and TypeSyntax"),
            },
            name if intrinsics::is_meta_type_expr(name) => match args {
                [ty] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.type_expr is available only during compile-time expansion",
                        );
                    }
                    let ty = compiler_reflected_type(ty)?;
                    Ok(Some(self.store_compiler_type_syntax(
                        "reflected-type",
                        ty,
                        Vec::new(),
                    )?))
                }
                _ => err("meta.type_expr expects TypeExpr"),
            },
            name if intrinsics::is_meta_type_capability(name) => match args {
                [head, Value::List(rights)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.type_capability is available only during compile-time expansion",
                        );
                    }
                    let head = compiler_ident_name(head, "meta.type_capability")?;
                    if !witchy_caps::capabilities::bears_rights_markers(&head) {
                        return err(
                            "meta.type_capability expected a rights-bearing capability \
                             (Console, Dir, File, Net, or Secret)",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        rights,
                        SyntaxCategory::Type,
                        "CompilerTypeSyntax",
                        &self.compiler_type_origins,
                        self.cur_line,
                    );
                    let rights = compiler_type_holes(rights, &self.compiler_type_syntax)?;
                    Ok(Some(self.store_compiler_type_syntax(
                        "capability-type",
                        Type::Named(head, rights),
                        hole_ancestry,
                    )?))
                }
                _ => err("meta.type_capability expects an Ident and List(TypeSyntax) rights"),
            },
            name if intrinsics::is_meta_call_site_pattern(name) => match args {
                [Value::Str(name), Value::List(args)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.call_site is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        args,
                        SyntaxCategory::Pattern,
                        "CompilerPatternSyntax",
                        &self.compiler_pattern_origins,
                        self.cur_line,
                    );
                    let args =
                        compiler_pattern_holes(args, &self.compiler_pattern_syntax)?;
                    let source =
                        witchy_syntax::linker::call_site_pattern_source(name, &args);
                    let pattern = witchy_syntax::linker::call_site_pattern(name, args);
                    let handle = self.next_compiler_syntax_handle("call-site-pattern")?;
                    self.compiler_pattern_syntax.insert(handle.clone(), pattern);
                    self.compiler_pattern_origins.insert(
                        handle.clone(),
                        ComptimeSyntaxOrigin {
                            definition_line: self.cur_line,
                            hole_ancestry,
                        },
                    );
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerPatternSyntax".into(),
                        fields: Rc::new(vec![
                            Value::str(handle),
                            Value::str(source),
                        ]),
                    }))
                }
                _ => {
                    err("meta.call_site pattern construction expects a name and pattern arguments")
                }
            },
            name if intrinsics::is_meta_pattern_ctor(name) => match args {
                [Value::Str(name), Value::List(args)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.pattern_ctor is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        args,
                        SyntaxCategory::Pattern,
                        "CompilerPatternSyntax",
                        &self.compiler_pattern_origins,
                        self.cur_line,
                    );
                    let args = compiler_pattern_holes(args, &self.compiler_pattern_syntax)?;
                    Ok(Some(self.store_compiler_pattern_syntax(
                        "constructor-pattern",
                        Pattern::Ctor { name: name.to_string(), args },
                        hole_ancestry,
                    )?))
                }
                _ => err("meta.pattern_ctor expects a name and pattern arguments"),
            },
            name if intrinsics::is_meta_pattern_tuple(name) => match args {
                [Value::List(patterns)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.pattern_tuple is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        patterns,
                        SyntaxCategory::Pattern,
                        "CompilerPatternSyntax",
                        &self.compiler_pattern_origins,
                        self.cur_line,
                    );
                    let patterns =
                        compiler_pattern_holes(patterns, &self.compiler_pattern_syntax)?;
                    Ok(Some(self.store_compiler_pattern_syntax(
                        "tuple-pattern",
                        Pattern::Tuple(patterns),
                        hole_ancestry,
                    )?))
                }
                _ => err("meta.pattern_tuple expects pattern arguments"),
            },
            name if intrinsics::is_meta_pattern_list(name) => match args {
                [Value::List(patterns)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.pattern_list is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        patterns,
                        SyntaxCategory::Pattern,
                        "CompilerPatternSyntax",
                        &self.compiler_pattern_origins,
                        self.cur_line,
                    );
                    let elems = compiler_pattern_holes(patterns, &self.compiler_pattern_syntax)?;
                    Ok(Some(self.store_compiler_pattern_syntax(
                        "list-pattern",
                        Pattern::List { elems, rest: None },
                        hole_ancestry,
                    )?))
                }
                _ => err("meta.pattern_list expects pattern arguments"),
            },
            name if intrinsics::is_meta_pattern_list_rest(name) => match args {
                [Value::List(patterns), rest] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.pattern_list_rest is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        patterns,
                        SyntaxCategory::Pattern,
                        "CompilerPatternSyntax",
                        &self.compiler_pattern_origins,
                        self.cur_line,
                    );
                    let elems = compiler_pattern_holes(patterns, &self.compiler_pattern_syntax)?;
                    let rest = match rest {
                        Value::Ctor { name, fields }
                            if compiler_ctor_tail(name) == "None" && fields.is_empty() =>
                        {
                            Some(None)
                        }
                        Value::Ctor { name, fields }
                            if compiler_ctor_tail(name) == "Some"
                                && matches!(fields.as_slice(), [Value::Ctor { .. }]) =>
                        {
                            Some(Some(compiler_binding_ident_name(
                                &fields[0],
                                "meta.pattern_list_rest",
                            )?))
                        }
                        _ => {
                            return err(
                                "meta.pattern_list_rest expected Option(Ident) rest binding",
                            );
                        }
                    };
                    Ok(Some(self.store_compiler_pattern_syntax(
                        "list-rest-pattern",
                        Pattern::List { elems, rest },
                        hole_ancestry,
                    )?))
                }
                _ => err("meta.pattern_list_rest expects patterns and a rest binding"),
            },
            name if intrinsics::is_meta_pattern_or(name) => match args {
                [Value::List(patterns)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.pattern_or is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        patterns,
                        SyntaxCategory::Pattern,
                        "CompilerPatternSyntax",
                        &self.compiler_pattern_origins,
                        self.cur_line,
                    );
                    let patterns =
                        compiler_pattern_holes(patterns, &self.compiler_pattern_syntax)?;
                    if patterns.is_empty() {
                        return err("meta.pattern_or requires at least one alternative");
                    }
                    Ok(Some(self.store_compiler_pattern_syntax(
                        "or-pattern",
                        Pattern::Or(patterns),
                        hole_ancestry,
                    )?))
                }
                _ => err("meta.pattern_or expects pattern alternatives"),
            },
            name if intrinsics::is_meta_expr_call(name) => match args {
                [callee, Value::List(args)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.expr_call is available only during compile-time expansion",
                        );
                    }
                    let mut inputs = Vec::with_capacity(args.len() + 1);
                    inputs.push(callee.clone());
                    inputs.extend(args.iter().cloned());
                    let hole_ancestry = compiler_direct_hole_origins(
                        &inputs,
                        SyntaxCategory::Expr,
                        "CompilerExprSyntax",
                        &self.compiler_expr_origins,
                        self.cur_line,
                    );
                    let callee = compiler_expr_syntax_value(callee, &self.compiler_expr_syntax)?;
                    let args = args
                        .iter()
                        .map(|arg| compiler_expr_syntax_value(arg, &self.compiler_expr_syntax))
                        .collect::<Result<Vec<_>, _>>()?;
                    let expr = match callee {
                        Expr::Ctor { name, args: existing } if existing.is_empty() => {
                            Expr::Ctor { name, args }
                        }
                        callee => Expr::Apply { func: Box::new(callee), args },
                    };
                    let source = witchy_syntax::format::expr_str(&expr);
                    let handle = self.next_compiler_syntax_handle("expression-call")?;
                    self.compiler_expr_syntax.insert(handle.clone(), expr);
                    self.compiler_expr_origins.insert(
                        handle.clone(),
                        ComptimeSyntaxOrigin { definition_line: self.cur_line, hole_ancestry },
                    );
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerExprSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
                    }))
                }
                _ => err("meta.expr_call expects an ExprSyntax callee and List(ExprSyntax) arguments"),
            },
            name if intrinsics::is_meta_expr_field(name) => match args {
                [base, field] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.expr_field is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        std::slice::from_ref(base),
                        SyntaxCategory::Expr,
                        "CompilerExprSyntax",
                        &self.compiler_expr_origins,
                        self.cur_line,
                    );
                    let base = compiler_expr_syntax_value(base, &self.compiler_expr_syntax)?;
                    let field = compiler_ident_name(field, "meta.expr_field")?;
                    let expr = Expr::Field { base: Box::new(base), field };
                    let source = witchy_syntax::format::expr_str(&expr);
                    let handle = self.next_compiler_syntax_handle("expression-field")?;
                    self.compiler_expr_syntax.insert(handle.clone(), expr);
                    self.compiler_expr_origins.insert(
                        handle.clone(),
                        ComptimeSyntaxOrigin { definition_line: self.cur_line, hole_ancestry },
                    );
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerExprSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
                    }))
                }
                _ => err("meta.expr_field expects an ExprSyntax base and Ident field"),
            },
            name if intrinsics::is_meta_match_arm(name) => match args {
                [pattern, body] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.match_arm is available only during compile-time expansion",
                        );
                    }
                    let pattern = compiler_pattern_syntax_value(
                        pattern,
                        &self.compiler_pattern_syntax,
                    )?;
                    let body = compiler_expr_syntax_value(body, &self.compiler_expr_syntax)?;
                    let source = format!(
                        "{} -> {}",
                        witchy_syntax::format::pattern_str(&pattern),
                        witchy_syntax::format::expr_str(&body),
                    );
                    let arm = MatchArm {
                        line: self.cur_line,
                        pattern,
                        guard: None,
                        body,
                    };
                    let handle = self.next_compiler_syntax_handle("match-arm")?;
                    self.compiler_match_arm_syntax.insert(handle.clone(), arm);
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerMatchArmSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
                    }))
                }
                _ => err("meta.match_arm expects PatternSyntax and ExprSyntax"),
            },
            name if intrinsics::is_meta_expr_match(name) => match args {
                [scrutinee, arms] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.expr_match is available only during compile-time expansion",
                        );
                    }
                    let hole_ancestry = compiler_direct_hole_origins(
                        std::slice::from_ref(scrutinee),
                        SyntaxCategory::Expr,
                        "CompilerExprSyntax",
                        &self.compiler_expr_origins,
                        self.cur_line,
                    );
                    let scrutinee =
                        compiler_expr_syntax_value(scrutinee, &self.compiler_expr_syntax)?;
                    let arms = compiler_match_arms(arms, &self.compiler_match_arm_syntax)?;
                    let expr = Expr::Match { scrutinee: Box::new(scrutinee), arms };
                    let canonical_source = witchy_syntax::format::expr_str(&expr);
                    let handle = self.next_compiler_syntax_handle("expression-match")?;
                    self.compiler_expr_syntax.insert(handle.clone(), expr);
                    self.compiler_expr_origins.insert(
                        handle.clone(),
                        ComptimeSyntaxOrigin { definition_line: self.cur_line, hole_ancestry },
                    );
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerExprSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::str(canonical_source)]),
                    }))
                }
                _ => err("meta.expr_match expects an ExprSyntax scrutinee and List(MatchArmSyntax) arms"),
            },
            name if intrinsics::is_meta_stmt_expr(name) => match args {
                [expr] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.stmt_expr is available only during compile-time expansion",
                        );
                    }
                    let expr =
                        compiler_expr_syntax_value(expr, &self.compiler_expr_syntax)?;
                    Ok(Some(self.store_compiler_stmt_syntax(
                        "expression-statement",
                        Stmt::Expr(expr),
                    )?))
                }
                _ => err("meta.stmt_expr expects ExprSyntax"),
            },
            name if intrinsics::is_meta_stmt_return(name) => match args {
                [expr] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.stmt_return is available only during compile-time expansion",
                        );
                    }
                    let expr =
                        compiler_expr_syntax_value(expr, &self.compiler_expr_syntax)?;
                    Ok(Some(self.store_compiler_stmt_syntax(
                        "return-statement",
                        Stmt::Return(Some(expr)),
                    )?))
                }
                _ => err("meta.stmt_return expects ExprSyntax"),
            },
            name if intrinsics::is_meta_stmt_let(name) => match args {
                [Value::Bool(mutable), binding, ty, value] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.stmt_let is available only during compile-time expansion",
                        );
                    }
                    let name = compiler_binding_ident_name(binding, "meta.stmt_let")?;
                    let ty = compiler_optional_type_syntax_value(
                        ty,
                        &self.compiler_type_syntax,
                    )?;
                    let value =
                        compiler_expr_syntax_value(value, &self.compiler_expr_syntax)?;
                    Ok(Some(self.store_compiler_stmt_syntax(
                        "let-statement",
                        Stmt::Let { name, ty, mutable: *mutable, value },
                    )?))
                }
                _ => err(
                    "meta.stmt_let expects Bool, Ident, Option(TypeSyntax), and ExprSyntax",
                ),
            },
            name if intrinsics::is_meta_block(name) => match args {
                [Value::List(stmts), tail] => {
                    if self.fresh_ident_scope.is_none() {
                        return err("meta.block is available only during compile-time expansion");
                    }
                    let mut stmts = stmts
                        .iter()
                        .map(|stmt| {
                            compiler_stmt_syntax_value(stmt, &self.compiler_stmt_syntax)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if let Some(tail) =
                        compiler_optional_expr_syntax_value(tail, &self.compiler_expr_syntax)?
                    {
                        stmts.push(Stmt::Expr(tail));
                    }
                    if stmts.is_empty() {
                        return err(
                            "meta.block body must contain at least one statement or tail expression",
                        );
                    }
                    let block = Block {
                        lines: vec![self.cur_line; stmts.len()],
                        stmts,
                        region: None,
                    };
                    let source = witchy_syntax::format::block_str(&block);
                    let handle = self.next_compiler_syntax_handle("block-builder")?;
                    self.compiler_block_syntax.insert(handle.clone(), block);
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerBlockSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
                    }))
                }
                _ => err("meta.block expects List(StmtSyntax) and Option(ExprSyntax)"),
            },
            name if intrinsics::is_meta_param(name) => match args {
                [binding, ty] => {
                    if self.fresh_ident_scope.is_none() {
                        return err("meta.param is available only during compile-time expansion");
                    }
                    let name = compiler_binding_ident_name(binding, "meta.param")?;
                    let ty = compiler_type_syntax_value(ty, &self.compiler_type_syntax)?;
                    let source = format!(
                        "{name}: {}",
                        witchy_syntax::format::type_str(&ty),
                    );
                    let param = Param {
                        name,
                        ty: Some(ty),
                        convention: Convention::Let,
                        default: None,
                    };
                    let handle = self.next_compiler_syntax_handle("parameter")?;
                    self.compiler_param_syntax.insert(handle.clone(), param);
                    Ok(Some(Value::Ctor {
                        name: "meta.CompilerParamSyntax".into(),
                        fields: Rc::new(vec![Value::str(handle), Value::str(source)]),
                    }))
                }
                _ => err("meta.param expects Ident and TypeSyntax"),
            },
            name if intrinsics::is_meta_function_block(name) => match args {
                [Value::Bool(public), name, params, ret, body] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.function_block is available only during compile-time expansion",
                        );
                    }
                    let name = compiler_binding_ident_name(name, "meta.function_block")?;
                    let params = compiler_params(params, &self.compiler_param_syntax)?;
                    let ret = compiler_optional_type_syntax_value(
                        ret,
                        &self.compiler_type_syntax,
                    )?;
                    let body = compiler_block_syntax_value(body, &self.compiler_block_syntax)?;
                    let module = parse_module("fn __witchy_meta_generated():\n    ()\n")
                        .map_err(|error| RuntimeError {
                            message: format!(
                                "meta.function_block failed to build a function skeleton: {error}"
                            ),
                        })?;
                    let [Item::Function(parsed)] = module.items.as_slice() else {
                        return err("meta.function_block failed to build one function skeleton");
                    };
                    let mut function = parsed.clone();
                    function.public = *public;
                    function.name = name;
                    function.params = params;
                    function.ret = ret;
                    function.body = body;
                    let item = Item::Function(function);
                    let handle = self.next_compiler_syntax_handle("function-item")?;
                    self.compiler_item_syntax.insert(handle.clone(), item);
                    Ok(Some(Value::Ctor {
                        name: OWNED_ITEM_SYNTAX_CTOR.into(),
                        fields: Rc::new(vec![
                            Value::str(handle),
                            Value::Int(i64::from(self.cur_line)),
                        ]),
                    }))
                }
                _ => err(
                    "meta.function_block expects Bool, Ident, List(ParamSyntax), Option(TypeSyntax), and BlockSyntax",
                ),
            },
            name if intrinsics::is_meta_impl_block(name) => match args {
                [trait_ty, target_ty, items] => {
                    if self.fresh_ident_scope.is_none() {
                        return err(
                            "meta.impl_block is available only during compile-time expansion",
                        );
                    }
                    let trait_ty =
                        compiler_type_syntax_value(trait_ty, &self.compiler_type_syntax)?;
                    let target_ty =
                        compiler_type_syntax_value(target_ty, &self.compiler_type_syntax)?;
                    let (trait_name, trait_args) =
                        compiler_impl_head(trait_ty, "trait", "meta.impl_block")?;
                    let (type_name, target_args) =
                        compiler_impl_head(target_ty, "target", "meta.impl_block")?;
                    let methods = compiler_impl_methods(items, &self.compiler_item_syntax)?;
                    let item = Item::Impl(ImplDef {
                        origin: ImplOrigin::Source,
                        trait_name: Some(trait_name),
                        trait_args,
                        type_name,
                        target_args,
                        bounds: Vec::new(),
                        methods,
                    });
                    let handle = self.next_compiler_syntax_handle("impl-item")?;
                    self.compiler_item_syntax.insert(handle.clone(), item);
                    Ok(Some(Value::Ctor {
                        name: OWNED_ITEM_SYNTAX_CTOR.into(),
                        fields: Rc::new(vec![
                            Value::str(handle),
                            Value::Int(i64::from(self.cur_line)),
                        ]),
                    }))
                }
                _ => err(
                    "meta.impl_block expects TypeSyntax, TypeSyntax, and List(ItemSyntax)",
                ),
            },
            name if intrinsics::is_meta_item(name) => match args {
                [Value::Str(source)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err("meta.item is available only during compile-time expansion");
                    }
                    let module = parse_module(source).map_err(|error| RuntimeError {
                        message: format!("meta.item source does not parse: {error}"),
                    })?;
                    if !module.modes.is_empty() || module.items.len() != 1 {
                        return err("meta.item expects exactly one complete declaration");
                    }
                    let handle = self.next_compiler_syntax_handle("dynamic-item")?;
                    self.compiler_item_syntax
                        .insert(handle.clone(), module.items[0].clone());
                    self.compiler_item_modules.insert(handle.clone(), module);
                    Ok(Some(Value::Ctor {
                        name: OWNED_ITEM_SYNTAX_CTOR.into(),
                        fields: Rc::new(vec![
                            Value::str(handle),
                            Value::Int(i64::from(self.cur_line)),
                        ]),
                    }))
                }
                _ => err("meta.item expects one String declaration fragment"),
            },
            name if intrinsics::is_meta_expr(name) => match args {
                [Value::Str(source)] => {
                    if self.fresh_ident_scope.is_none() {
                        return err("meta.expr_raw is available only during compile-time expansion");
                    }
                    let parsed = match &self.compiler_expr_qualifiers {
                        Some(qualifiers) => {
                            crate::tagged::parse_generated_splice_expr(source, qualifiers)
                        }
                        None => witchy_syntax::syntax_holes::parse_expr_payload(source),
                    };
                    let expr = parsed.map_err(|error| RuntimeError {
                        message: format!("meta.expr_raw source does not parse: {error}"),
                    })?;
                    Ok(Some(self.store_compiler_expr_syntax(
                        "dynamic-expression",
                        expr,
                        Vec::new(),
                    )?))
                }
                _ => err("meta.expr_raw expects one String expression"),
            },
            name if name == intrinsics::COMPILER_QUOTE_EXPR => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned expression quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::Str(source)]
                        if self.compiler_expr_syntax.contains_key(handle.as_str()) =>
                    {
                        let expr = self.compiler_expr_syntax[handle.as_str()].clone();
                        let instance_handle = self.next_compiler_syntax_handle("expression")?;
                        self.compiler_expr_syntax.insert(instance_handle.clone(), expr);
                        self.compiler_expr_origins.insert(
                            instance_handle.clone(),
                            ComptimeSyntaxOrigin {
                                definition_line: self.cur_line,
                                hole_ancestry: Vec::new(),
                            },
                        );
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerExprSyntax".into(),
                            fields: Rc::new(vec![
                                Value::str(instance_handle),
                                Value::Str(source.clone()),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::Str(_)] => {
                        err("compiler-owned expression quotation referenced an invalid syntax handle")
                    }
                    _ => err("compiler-owned expression quotation expects an expression handle"),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_EXPR_HOLES => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned expression quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::List(parts), Value::List(holes)]
                        if parts.len() == holes.len() + 1
                            && parts.iter().all(|part| matches!(part, Value::Str(_))) =>
                    {
                        let template = self
                            .compiler_expr_syntax
                            .get(handle.as_str())
                            .cloned()
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned expression quotation referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        let definition_line = self.cur_line;
                        let hole_ancestry = compiler_item_hole_origins(
                            holes,
                            &self.compiler_expr_origins,
                            &self.compiler_type_origins,
                            &self.compiler_pattern_origins,
                            self.cur_line,
                        );
                        let holes = compiler_item_holes(
                            holes,
                            &self.compiler_expr_syntax,
                            &self.compiler_type_syntax,
                            &self.compiler_pattern_syntax,
                        )?;
                        let expr =
                            witchy_syntax::syntax_holes::instantiate_expr_mixed(&template, holes)
                                .map_err(|message| RuntimeError { message })?;
                        let source = witchy_syntax::format::expr_str(&expr);
                        let instance_handle = self.next_compiler_syntax_handle("expression")?;
                        if let Some(existing) = self.compiler_expr_syntax.get(&instance_handle) {
                            if existing != &expr {
                                return err(
                                    "compiler-owned expression instance handle collided with a different AST",
                                );
                            }
                        } else {
                            self.compiler_expr_syntax.insert(instance_handle.clone(), expr);
                        }
                        self.compiler_expr_origins.insert(
                            instance_handle.clone(),
                            ComptimeSyntaxOrigin { definition_line, hole_ancestry },
                        );
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerExprSyntax".into(),
                            fields: Rc::new(vec![Value::str(instance_handle), Value::str(source)]),
                        }))
                    }
                    [Value::Str(_), Value::List(_), Value::List(_)] => err(
                        "compiler-owned expression quotation referenced an invalid syntax handle or hole plan",
                    ),
                    _ => err(
                        "compiler-owned expression quotation expects an expression handle and typed holes",
                    ),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_TYPE => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned type quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::Str(source)]
                        if self.compiler_type_syntax.contains_key(handle.as_str()) =>
                    {
                        let ty = self.compiler_type_syntax[handle.as_str()].clone();
                        let instance_handle = self.next_compiler_syntax_handle("type")?;
                        self.compiler_type_syntax.insert(instance_handle.clone(), ty);
                        self.compiler_type_origins.insert(
                            instance_handle.clone(),
                            ComptimeSyntaxOrigin {
                                definition_line: self.cur_line,
                                hole_ancestry: Vec::new(),
                            },
                        );
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerTypeSyntax".into(),
                            fields: Rc::new(vec![
                                Value::str(instance_handle),
                                Value::Str(source.clone()),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::Str(_)] => {
                        err("compiler-owned type quotation referenced an invalid syntax handle")
                    }
                    _ => err("compiler-owned type quotation expects a type handle"),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_TYPE_HOLES => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned type quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::List(parts), Value::List(holes)]
                        if parts.len() == holes.len() + 1
                            && parts.iter().all(|part| matches!(part, Value::Str(_))) =>
                    {
                        let template = self
                            .compiler_type_syntax
                            .get(handle.as_str())
                            .cloned()
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned type quotation referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        let definition_line = self.cur_line;
                        let hole_ancestry = compiler_direct_hole_origins(
                            holes,
                            SyntaxCategory::Type,
                            "CompilerTypeSyntax",
                            &self.compiler_type_origins,
                            self.cur_line,
                        );
                        let holes = compiler_type_holes(holes, &self.compiler_type_syntax)?;
                        let ty = witchy_syntax::syntax_holes::instantiate_type(&template, holes)
                            .map_err(|message| RuntimeError { message })?;
                        let source = witchy_syntax::format::type_str(&ty);
                        let instance_handle = self.next_compiler_syntax_handle("type")?;
                        if let Some(existing) = self.compiler_type_syntax.get(&instance_handle) {
                            if existing != &ty {
                                return err(
                                    "compiler-owned type instance handle collided with a different AST",
                                );
                            }
                        } else {
                            self.compiler_type_syntax.insert(instance_handle.clone(), ty);
                        }
                        self.compiler_type_origins.insert(
                            instance_handle.clone(),
                            ComptimeSyntaxOrigin { definition_line, hole_ancestry },
                        );
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerTypeSyntax".into(),
                            fields: Rc::new(vec![Value::str(instance_handle), Value::str(source)]),
                        }))
                    }
                    [Value::Str(_), Value::List(_), Value::List(_)] => err(
                        "compiler-owned type quotation referenced an invalid syntax handle or hole plan",
                    ),
                    _ => err(
                        "compiler-owned type quotation expects a type handle and typed holes",
                    ),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_PATTERN => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned pattern quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::Str(source)]
                        if self.compiler_pattern_syntax.contains_key(handle.as_str()) =>
                    {
                        let pattern = self.compiler_pattern_syntax[handle.as_str()].clone();
                        let instance_handle = self.next_compiler_syntax_handle("pattern")?;
                        self.compiler_pattern_syntax.insert(instance_handle.clone(), pattern);
                        self.compiler_pattern_origins.insert(
                            instance_handle.clone(),
                            ComptimeSyntaxOrigin {
                                definition_line: self.cur_line,
                                hole_ancestry: Vec::new(),
                            },
                        );
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerPatternSyntax".into(),
                            fields: Rc::new(vec![
                                Value::str(instance_handle),
                                Value::Str(source.clone()),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::Str(_)] => {
                        err("compiler-owned pattern quotation referenced an invalid syntax handle")
                    }
                    _ => err("compiler-owned pattern quotation expects a pattern handle"),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_PATTERN_HOLES => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned pattern quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::List(parts), Value::List(holes)]
                        if parts.len() == holes.len() + 1
                            && parts.iter().all(|part| matches!(part, Value::Str(_))) =>
                    {
                        let template = self
                            .compiler_pattern_syntax
                            .get(handle.as_str())
                            .cloned()
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned pattern quotation referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        let definition_line = self.cur_line;
                        let hole_ancestry = compiler_direct_hole_origins(
                            holes,
                            SyntaxCategory::Pattern,
                            "CompilerPatternSyntax",
                            &self.compiler_pattern_origins,
                            self.cur_line,
                        );
                        let holes =
                            compiler_pattern_holes(holes, &self.compiler_pattern_syntax)?;
                        let pattern =
                            witchy_syntax::syntax_holes::instantiate_pattern(&template, holes)
                                .map_err(|message| RuntimeError { message })?;
                        let source = witchy_syntax::format::pattern_str(&pattern);
                        let instance_handle = self.next_compiler_syntax_handle("pattern")?;
                        if let Some(existing) = self.compiler_pattern_syntax.get(&instance_handle) {
                            if existing != &pattern {
                                return err(
                                    "compiler-owned pattern instance handle collided with a different AST",
                                );
                            }
                        } else {
                            self.compiler_pattern_syntax
                                .insert(instance_handle.clone(), pattern);
                        }
                        self.compiler_pattern_origins.insert(
                            instance_handle.clone(),
                            ComptimeSyntaxOrigin { definition_line, hole_ancestry },
                        );
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerPatternSyntax".into(),
                            fields: Rc::new(vec![Value::str(instance_handle), Value::str(source)]),
                        }))
                    }
                    [Value::Str(_), Value::List(_), Value::List(_)] => err(
                        "compiler-owned pattern quotation referenced an invalid syntax handle or hole plan",
                    ),
                    _ => err(
                        "compiler-owned pattern quotation expects a pattern handle and typed holes",
                    ),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_STMT => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned statement quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::Str(source)]
                        if self.compiler_stmt_syntax.contains_key(handle.as_str()) =>
                    {
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerStmtSyntax".into(),
                            fields: Rc::new(vec![Value::Str(handle.clone()), Value::Str(source.clone())]),
                        }))
                    }
                    [Value::Str(_), Value::Str(_)] => {
                        err("compiler-owned statement quotation referenced an invalid syntax handle")
                    }
                    _ => err("compiler-owned statement quotation expects a statement handle"),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_STMT_HOLES => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned statement quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::List(parts), Value::List(holes)]
                        if parts.len() == holes.len() + 1
                            && parts.iter().all(|part| matches!(part, Value::Str(_))) =>
                    {
                        let template = self
                            .compiler_stmt_syntax
                            .get(handle.as_str())
                            .cloned()
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned statement quotation referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        let holes = compiler_item_holes(
                            holes,
                            &self.compiler_expr_syntax,
                            &self.compiler_type_syntax,
                            &self.compiler_pattern_syntax,
                        )?;
                        let stmt = witchy_syntax::syntax_holes::instantiate_stmt(&template, holes)
                            .map_err(|message| RuntimeError { message })?;
                        let source = witchy_syntax::format::stmt_str(&stmt);
                        let instance_handle =
                            format!("{handle}\0compiler-owned-statement-instance\0{source}");
                        if let Some(existing) = self.compiler_stmt_syntax.get(&instance_handle) {
                            if existing != &stmt {
                                return err(
                                    "compiler-owned statement instance handle collided with a different AST",
                                );
                            }
                        } else {
                            self.compiler_stmt_syntax.insert(instance_handle.clone(), stmt);
                        }
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerStmtSyntax".into(),
                            fields: Rc::new(vec![
                                Value::str(instance_handle),
                                Value::str(source),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::List(_), Value::List(_)] => err(
                        "compiler-owned statement quotation referenced an invalid syntax handle or hole plan",
                    ),
                    _ => err(
                        "compiler-owned statement quotation expects a statement handle and typed holes",
                    ),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_BLOCK => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned block quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::Str(source)]
                        if self.compiler_block_syntax.contains_key(handle.as_str()) =>
                    {
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerBlockSyntax".into(),
                            fields: Rc::new(vec![Value::Str(handle.clone()), Value::Str(source.clone())]),
                        }))
                    }
                    [Value::Str(_), Value::Str(_)] => {
                        err("compiler-owned block quotation referenced an invalid syntax handle")
                    }
                    _ => err("compiler-owned block quotation expects a block handle"),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_BLOCK_HOLES => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned block quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::List(parts), Value::List(holes)]
                        if parts.len() == holes.len() + 1
                            && parts.iter().all(|part| matches!(part, Value::Str(_))) =>
                    {
                        let template = self
                            .compiler_block_syntax
                            .get(handle.as_str())
                            .cloned()
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned block quotation referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        let holes = compiler_item_holes(
                            holes,
                            &self.compiler_expr_syntax,
                            &self.compiler_type_syntax,
                            &self.compiler_pattern_syntax,
                        )?;
                        let block =
                            witchy_syntax::syntax_holes::instantiate_block(&template, holes)
                                .map_err(|message| RuntimeError { message })?;
                        let source = witchy_syntax::format::block_str(&block);
                        let instance_handle =
                            format!("{handle}\0compiler-owned-block-instance\0{source}");
                        if let Some(existing) = self.compiler_block_syntax.get(&instance_handle) {
                            if existing != &block {
                                return err(
                                    "compiler-owned block instance handle collided with a different AST",
                                );
                            }
                        } else {
                            self.compiler_block_syntax.insert(instance_handle.clone(), block);
                        }
                        Ok(Some(Value::Ctor {
                            name: "meta.CompilerBlockSyntax".into(),
                            fields: Rc::new(vec![
                                Value::str(instance_handle),
                                Value::str(source),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::List(_), Value::List(_)] => err(
                        "compiler-owned block quotation referenced an invalid syntax handle or hole plan",
                    ),
                    _ => err(
                        "compiler-owned block quotation expects a block handle and typed holes",
                    ),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_ITEM => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned item quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::Str(_source)]
                        if self.compiler_item_syntax.contains_key(handle.as_str()) =>
                    {
                        Ok(Some(Value::Ctor {
                            name: OWNED_ITEM_SYNTAX_CTOR.into(),
                            fields: Rc::new(vec![
                                Value::Str(handle.clone()),
                                Value::Int(i64::from(self.cur_line)),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::Str(_)] => {
                        err("compiler-owned item quotation referenced an invalid syntax handle")
                    }
                    _ => err("compiler-owned item quotation expects an item handle"),
                }
            }
            name if name == intrinsics::COMPILER_QUOTE_ITEM_HOLES => {
                if self.fresh_ident_scope.is_none() {
                    return err(
                        "compiler-owned item quotation is available only during compile-time expansion",
                    );
                }
                match args {
                    [Value::Str(handle), Value::List(parts), Value::List(holes)]
                        if self.compiler_item_syntax.contains_key(handle.as_str())
                            && parts.len() == holes.len() + 1
                            && parts.iter().all(|part| matches!(part, Value::Str(_))) =>
                    {
                        Ok(Some(Value::Ctor {
                            name: OWNED_ITEM_SYNTAX_CTOR.into(),
                            fields: Rc::new(vec![
                                Value::Str(handle.clone()),
                                Value::List(holes.clone()),
                                Value::Int(i64::from(self.cur_line)),
                            ]),
                        }))
                    }
                    [Value::Str(_), Value::List(_), Value::List(_)] => {
                        err("compiler-owned item quotation referenced an invalid syntax handle or hole plan")
                    }
                    _ => err("compiler-owned item quotation expects an item handle and typed holes"),
                }
            }
            name if name == intrinsics::COMPILER_EMIT_ITEM => {
                if self.fresh_ident_scope.is_none() {
                    return err("item emission is available only during compile-time expansion");
                }
                let emission = match one(args)? {
                    Value::Ctor { name, fields }
                        if &*name == OWNED_ITEM_SYNTAX_CTOR
                            && matches!(fields.as_slice(), [Value::Str(_), Value::Int(_)]) =>
                    {
                        let [Value::Str(handle), Value::Int(definition_line)] = fields.as_slice()
                        else {
                            unreachable!()
                        };
                        let definition_line = u32::try_from(*definition_line).unwrap_or(0);
                        if let Some(module) = self.compiler_item_modules.get(handle.as_str()) {
                            ComptimeItemEmission::ModuleSyntax {
                                module: Box::new(module.clone()),
                                definition_line,
                                hole_ancestry: Vec::new(),
                            }
                        } else {
                            let item = self
                                .compiler_item_syntax
                                .get(handle.as_str())
                                .cloned()
                                .ok_or_else(|| RuntimeError {
                                    message: "compiler-owned item emission referenced an invalid syntax handle"
                                        .into(),
                                })?;
                            ComptimeItemEmission::Syntax {
                                item: Box::new(item),
                                definition_line,
                                hole_ancestry: Vec::new(),
                            }
                        }
                    }
                    Value::Ctor { name, fields }
                        if &*name == OWNED_ITEM_SYNTAX_CTOR
                            && matches!(
                                fields.as_slice(),
                                [Value::Str(_), Value::List(_), Value::Int(_)]
                            ) =>
                    {
                        let [Value::Str(handle), Value::List(holes), Value::Int(invocation_line)] =
                            fields.as_slice()
                        else {
                            unreachable!()
                        };
                        let template = self
                            .compiler_item_syntax
                            .get(handle.as_str())
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned item emission referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        let hole_ancestry = compiler_item_hole_origins(
                            holes,
                            &self.compiler_expr_origins,
                            &self.compiler_type_origins,
                            &self.compiler_pattern_origins,
                            u32::try_from(*invocation_line).unwrap_or(0),
                        );
                        let holes = compiler_item_holes(
                            holes,
                            &self.compiler_expr_syntax,
                            &self.compiler_type_syntax,
                            &self.compiler_pattern_syntax,
                        )?;
                        let item = witchy_syntax::syntax_holes::instantiate_item(template, holes)
                            .map_err(|message| RuntimeError { message })?;
                        ComptimeItemEmission::Syntax {
                            item: Box::new(item),
                            definition_line: u32::try_from(*invocation_line).unwrap_or(0),
                            hole_ancestry,
                        }
                    }
                    _ => return err("emit_item expects meta.ItemSyntax"),
                };
                self.comptime_item_output.push(PositionedComptimeItem {
                    output_position: self.output.len(),
                    emission,
                });
                Ok(Some(Value::Unit))
            }
            name if name == intrinsics::COMPILER_EMIT_EXPR => {
                if self.fresh_ident_scope.is_none() {
                    return err("expression emission is available only during compile-time expansion");
                }
                let emission = match one(args)? {
                    Value::Ctor { name, fields }
                        if name.rsplit_once('.').map_or(&*name, |(_, tail)| tail)
                            == "CompilerExprSyntax" =>
                    {
                        let [Value::Str(handle), Value::Str(_source)] = fields.as_slice() else {
                            return err("CompilerExprSyntax carried an invalid payload");
                        };
                        let expr = self
                            .compiler_expr_syntax
                            .get(handle.as_str())
                            .cloned()
                            .ok_or_else(|| RuntimeError {
                                message: "compiler-owned expression emission referenced an invalid syntax handle"
                                    .into(),
                            })?;
                        ComptimeExprEmission::Syntax(Box::new(expr))
                    }
                    _ => return err("expression emission expects meta.ExprSyntax"),
                };
                self.comptime_expr_output.push(emission);
                Ok(Some(Value::Unit))
            }
            // Pure builtins need no capability.
            name if is_render_intrinsic(name) => Ok(Some(Value::str(self.render_value(&one(args)?)))),
            // (RFC-0055) Channel message erasure. `Value` is uniform, so erasing a
            // typed message to the executor's opaque `__Msg` and recovering the
            // endpoint's type are both the identity — the value passes through
            // unchanged, exactly as the executor's former generic `m` did.
            intrinsics::ERASE | intrinsics::UNERASE => Ok(Some(one(args)?)),
            intrinsics::DYNAMIC_TRY_DECODE_TYPED => match args {
                [Value::Ctor { name, fields }, Value::Int(expected)]
                    if name.rsplit('.').next() == Some("Dynamic") =>
                {
                    let [descriptor, payload] = fields.as_slice() else {
                        return err("malformed Dynamic envelope");
                    };
                    let Value::Ctor { name, fields } = descriptor else {
                        return err("malformed Dynamic runtime descriptor");
                    };
                    if name.rsplit('.').next() != Some("RuntimeType") {
                        return err("malformed Dynamic runtime descriptor");
                    }
                    let [Value::Int(actual), Value::Str(_)] = fields.as_slice() else {
                        return err("malformed Dynamic runtime descriptor");
                    };
                    let Value::Existential { payload, .. } = payload else {
                        return err("malformed Dynamic payload envelope");
                    };
                    if actual == expected {
                        Ok(Some(Value::ctor("Some", vec![(**payload).clone()])))
                    } else {
                        Ok(Some(Value::ctor("None", Vec::new())))
                    }
                }
                _ => err("typed Dynamic decoding expects a Dynamic value and descriptor ID"),
            },
            intrinsics::DYNAMIC_DESCRIPTOR | intrinsics::DYNAMIC_TRY_DECODE => err(
                "internal error: unspecialized Dynamic operation reached the interpreter",
            ),
            // String stdlib.
            intrinsics::STRING_LENGTH => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Int(s.len() as i64))),
                other => err(format!("string_length expects a String, got `{other}`")),
            },
            // (Bytes) Primitive intrinsics behind `std/bytes`. A `Bytes` is raw bytes
            // with no UTF-8 contract; `to_string` decodes lossily (a strict decoder can
            // live in std). `Str <-> Bytes` are real conversions in the tree-walker.
            intrinsics::BYTES_FROM_STRING => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Bytes(
                    Rc::try_unwrap(s).unwrap_or_else(|rc| (*rc).clone()).into_bytes(),
                ))),
                other => err(format!("bytes.from_string expects a String, got `{other}`")),
            },
            intrinsics::BYTES_FROM_LIST => match one(args)? {
                Value::List(xs) => {
                    let mut out = Vec::with_capacity(xs.len());
                    for x in xs.iter().cloned() {
                        let Value::Int(n) = x else {
                            return err("bytes.from_list expects a List(Int)");
                        };
                        if !(0..=255).contains(&n) {
                            return err(format!("bytes.from_list: value {n} is outside 0..=255"));
                        }
                        out.push(n as u8);
                    }
                    Ok(Some(Value::Bytes(out)))
                }
                other => err(format!("bytes.from_list expects a List(Int), got `{other}`")),
            },
            intrinsics::BYTES_TO_STRING => match one(args)? {
                Value::Bytes(b) => Ok(Some(Value::str(String::from_utf8_lossy(&b).into_owned()))),
                other => err(format!("bytes.to_string expects Bytes, got `{other}`")),
            },
            intrinsics::BYTES_LENGTH => match one(args)? {
                Value::Bytes(b) => Ok(Some(Value::Int(b.len() as i64))),
                other => err(format!("bytes.length expects Bytes, got `{other}`")),
            },
            intrinsics::BYTES_AT | "bytes.at" => match args {
                [Value::Bytes(b), Value::Int(i)] => match b.get(*i as usize) {
                    Some(byte) => Ok(Some(Value::Int(*byte as i64))),
                    None => err(DiagTemplate::BytesIndexOob.render(*i, b.len() as i64, "")),
                },
                _ => err("bytes.at expects Bytes and an Int index"),
            },
            intrinsics::BYTES_CONCAT => match args {
                [Value::Bytes(a), Value::Bytes(b)] => {
                    let mut out = a.clone();
                    out.extend_from_slice(b);
                    Ok(Some(Value::Bytes(out)))
                }
                _ => err("bytes.concat expects two Bytes"),
            },
            intrinsics::BYTES_SLICE => match args {
                [Value::Bytes(b), Value::Int(start), Value::Int(end)] => {
                    let lo = (*start).max(0) as usize;
                    let hi = (*end).max(0).min(b.len() as i64) as usize;
                    let hi = hi.max(lo);
                    Ok(Some(Value::Bytes(b.get(lo..hi).unwrap_or(&[]).to_vec())))
                }
                _ => err("bytes.slice expects Bytes and two Int indices"),
            },
            // The number of Unicode scalars — the character count, as opposed to
            // `string_length`'s byte count (they agree for ASCII).
            intrinsics::STRING_CHAR_COUNT => match one(args)? {
                Value::Str(s) => Ok(Some(Value::Int(s.chars().count() as i64))),
                other => err(format!("char_count expects a String, got `{other}`")),
            },
            // ASCII case mapping (a-z <-> A-Z); non-ASCII bytes are unchanged.
            // Deliberately ASCII-only so the WASM backend can match it byte-for-
            // byte (full Unicode case folding would need large tables).
            intrinsics::STRING_TO_UPPER => match one(args)? {
                Value::Str(s) => Ok(Some(Value::str(s.to_ascii_uppercase()))),
                other => err(format!("to_upper expects a String, got `{other}`")),
            },
            intrinsics::STRING_TO_LOWER => match one(args)? {
                Value::Str(s) => Ok(Some(Value::str(s.to_ascii_lowercase()))),
                other => err(format!("to_lower expects a String, got `{other}`")),
            },
            // Abort with a message — the error-raising primitive behind
            // `std/testing`'s assertions (a deliberate, loud failure).
            "fail" => match one(args)? {
                Value::Str(msg) => {
                    // When this `fail` is the one behind a `std/testing` assertion
                    // that user code invoked, retarget the reported location to the
                    // user's call site (recorded at the crossing). A direct `fail`
                    // in user code, or any non-assertion runtime error, is left to
                    // the default innermost-frame reporting.
                    if self.cur_fn.starts_with("testing.") {
                        if let Some((func, line)) = self.assert_site.take() {
                            self.cur_fn = func;
                            self.cur_line = line;
                        }
                    }
                    Err(RuntimeError { message: (*msg).clone() })
                }
                other => err(format!("fail expects a String message, got `{other}`")),
            },
            intrinsics::STRING_TRIM => match one(args)? {
                // ASCII whitespace only — exactly the byte set the WASM `$is_ws`
                // helper strips (space, tab, LF, VT, FF, CR). Rust's `str::trim`
                // would additionally strip Unicode whitespace (NBSP, …), which the
                // compiled backend does not, so we pin both to this set to keep the
                // backends in agreement (consistent with ASCII `to_upper`/`to_lower`).
                Value::Str(s) => {
                    let trimmed =
                        s.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'));
                    Ok(Some(Value::str(trimmed)))
                }
                other => err(format!("trim expects a String, got `{other}`")),
            },
            intrinsics::STRING_STARTS_WITH => match args {
                [Value::Str(s), Value::Str(prefix)] => {
                    Ok(Some(Value::Bool(s.starts_with(prefix.as_str()))))
                }
                _ => err("starts_with expects two Strings"),
            },
            intrinsics::STRING_CONTAINS => match args {
                [Value::Str(s), Value::Str(sub)] => {
                    Ok(Some(Value::Bool(s.contains(sub.as_str()))))
                }
                _ => err("contains expects two Strings"),
            },
            // Split on a separator into a list of pieces (the separator itself is
            // dropped); the empty separator yields the whole string unchanged.
            intrinsics::STRING_SPLIT => match args {
                [Value::Str(s), Value::Str(sep)] => {
                    let parts: Vec<Value> = if sep.is_empty() {
                        vec![Value::Str(s.clone())]
                    } else {
                        s.split(sep.as_str()).map(Value::str).collect()
                    };
                    Ok(Some(Value::list(parts)))
                }
                _ => err("split expects two Strings"),
            },
            // The characters of a string, each as a single-char String (one pass).
            intrinsics::STRING_CHARS => match one(args)? {
                Value::Str(s) => {
                    Ok(Some(Value::list(s.chars().map(|c| Value::str(c.to_string())).collect())))
                }
                _ => err("string_chars expects a String"),
            },
            intrinsics::STRING_REPLACE => match args {
                [Value::Str(s), Value::Str(from), Value::Str(to)] => {
                    Ok(Some(Value::str(s.replace(from.as_str(), to.as_str()))))
                }
                _ => err("replace expects three Strings"),
            },
            intrinsics::STRING_ENDS_WITH => match args {
                [Value::Str(s), Value::Str(suffix)] => {
                    Ok(Some(Value::Bool(s.ends_with(suffix.as_str()))))
                }
                _ => err("ends_with expects two Strings"),
            },
            // Char index of the first occurrence of `sub`, or -1 if absent.
            intrinsics::STRING_FIND => match args {
                [Value::Str(s), Value::Str(sub)] => {
                    let idx = s
                        .find(sub.as_str())
                        .map(|byte| s[..byte].chars().count() as i64)
                        .unwrap_or(-1);
                    Ok(Some(Value::Int(idx)))
                }
                _ => err("index_of expects two Strings"),
            },
            // Characters in the half-open range [start, end), clamped to bounds
            // (counted by Unicode scalar, so slicing never splits a character).
            intrinsics::STRING_SUBSTRING => match args {
                [Value::Str(s), Value::Int(start), Value::Int(end)] => {
                    let chars: Vec<char> = s.chars().collect();
                    let lo = (*start).max(0) as usize;
                    let hi = (*end).max(0) as usize;
                    let lo = lo.min(chars.len());
                    let hi = hi.min(chars.len());
                    let out: String = if lo < hi {
                        chars[lo..hi].iter().collect()
                    } else {
                        String::new()
                    };
                    Ok(Some(Value::str(out)))
                }
                _ => err("substring expects a String and two Int indices"),
            },
            // Conversions.
            intrinsics::MATH_TO_FLOAT => match one(args)? {
                Value::Int(n) => Ok(Some(Value::Float(n as f64))),
                other => err(format!("int_to_float expects an Int, got `{other}`")),
            },
            intrinsics::MATH_TO_INT => match one(args)? {
                Value::Float(x) if x.is_nan() => err(DiagTemplate::NanToInt.render(0, 0, "")),
                Value::Float(x) => Ok(Some(Value::Int(x as i64))),
                other => err(format!("float_to_int expects a Float, got `{other}`")),
            },
            // Duration <-> Int(ms): a Duration is an Int(ms) at runtime, so both
            // directions are the identity.
            "int_to_duration" | "duration_to_int" => match one(args)? {
                Value::Int(n) => Ok(Some(Value::Int(n))),
                other => err(format!("{name} expects an Int/Duration, got `{other}`")),
            },
            intrinsics::MATH_SQRT => match one(args)? {
                Value::Float(x) => Ok(Some(Value::Float(x.sqrt()))),
                other => err(format!("sqrt expects a Float, got `{other}`")),
            },
            intrinsics::STRING_TO_INT => match one(args)? {
                Value::Str(s) => match s.trim().parse::<i64>() {
                    Ok(n) => Ok(Some(Value::Int(n))),
                    Err(_) => err(DiagTemplate::ParseInt.render(0, 0, &s)),
                },
                other => err(format!("string_to_int expects a String, got `{other}`")),
            },
            intrinsics::LIST_LENGTH => match args {
                [Value::List(items)] => Ok(Some(Value::Int(items.len() as i64))),
                _ => err("length expects a list"),
            },
            intrinsics::LIST_AT => match args {
                [Value::List(items), Value::Int(i)] => match items.get(*i as usize) {
                    Some(v) => Ok(Some(v.clone())),
                    None => err(DiagTemplate::ListIndexOob.render(*i, items.len() as i64, "")),
                },
                _ => err("at expects a list and an Int index"),
            },
            // Return a new list with `x` appended (lists are values, so this does
            // not mutate the original).
            intrinsics::LIST_PUSH | intrinsics::GENERATED_LIST_PUSH => match args {
                [Value::List(items), x] => {
                    let mut out = (**items).clone();
                    out.push(x.clone());
                    Ok(Some(Value::list(out)))
                }
                _ => err("push expects a list and a value"),
            },
            name if intrinsics::is_list_pop_extract(name) => match args {
                [Value::List(items)] => {
                    let mut out = (**items).clone();
                    let old = match out.pop() {
                        Some(value) => Value::Ctor {
                            name: "Some".into(),
                            fields: Rc::new(vec![value]),
                        },
                        None => Value::ctor("None", Vec::new()),
                    };
                    Ok(Some(Value::tuple(vec![Value::list(out), old])))
                }
                _ => err("pop expects a list"),
            },
            intrinsics::LIST_SET_AT => match args {
                [Value::List(items), Value::Int(index), value] => {
                    let i = *index as usize;
                    if i >= items.len() {
                        return err(DiagTemplate::ListIndexOob.render(
                            *index,
                            items.len() as i64,
                            "",
                        ));
                    }
                    let mut out = (**items).clone();
                    out[i] = value.clone();
                    Ok(Some(Value::list(out)))
                }
                _ => err("set_at expects a list, an Int index, and a value"),
            },
            // Return a new list that is the two given lists joined.
            intrinsics::LIST_CONCAT => match args {
                [Value::List(a), Value::List(b)] => {
                    let mut out = (**a).clone();
                    out.extend(b.iter().cloned());
                    Ok(Some(Value::list(out)))
                }
                _ => err("concat expects two lists"),
            },
            // --- Dict: an immutable association map ---
            intrinsics::DICT_NEW => match args {
                [] => Ok(Some(Value::dict(Vec::new()))),
                _ => err("dict_new takes no arguments"),
            },
            // Return a new dict with `k` set to `v` (replacing any existing entry).
            intrinsics::DICT_INSERT => match args {
                [Value::Dict(entries), k, v] => {
                    let mut out = (**entries).clone();
                    match self.dict_key_position(&out, k)? {
                        Some(index) => out[index].1 = v.clone(),
                        None => out.push((k.clone(), v.clone())),
                    }
                    Ok(Some(Value::dict(out)))
                }
                _ => err("insert expects a Dict, a key, and a value"),
            },
            name if intrinsics::is_dict_insert_extract(name) => match args {
                [Value::Dict(entries), k, v] => {
                    let mut out = (**entries).clone();
                    let previous = match self.dict_key_position(&out, k)? {
                        Some(index) => {
                            let old = std::mem::replace(&mut out[index].1, v.clone());
                            Value::ctor("Some", vec![old])
                        }
                        None => {
                            out.push((k.clone(), v.clone()));
                            Value::ctor("None", Vec::new())
                        }
                    };
                    Ok(Some(Value::tuple(vec![Value::dict(out), previous])))
                }
                _ => err("insert expects a Dict, a key, and a value"),
            },
            // Value for `k`, or `default` if absent.
            intrinsics::DICT_GET_OR => match args {
                [Value::Dict(entries), k, default] => {
                    let found = self.dict_key_position(entries, k)?;
                    Ok(Some(found.map(|index| entries[index].1.clone()).unwrap_or_else(|| default.clone())))
                }
                _ => err("get_or expects a Dict, a key, and a default value"),
            },
            intrinsics::DICT_AT => match args {
                [Value::Dict(entries), k] => match self.dict_key_position(entries, k)? {
                    Some(index) => Ok(Some(entries[index].1.clone())),
                    None => err(DiagTemplate::DictMissing.render(0, 0, "")),
                },
                _ => err("at expects a Dict and a key"),
            },
            intrinsics::DICT_CONTAINS_KEY => match args {
                [Value::Dict(entries), k] => {
                    Ok(Some(Value::Bool(self.dict_key_position(entries, k)?.is_some())))
                }
                _ => err("has expects a Dict and a key"),
            },
            // A new dict with `k` (and its value) removed; unchanged if absent.
            intrinsics::DICT_REMOVE => match args {
                [Value::Dict(entries), k] => {
                    let mut out = (**entries).clone();
                    if let Some(index) = self.dict_key_position(&out, k)? {
                        out.remove(index);
                    }
                    Ok(Some(Value::dict(out)))
                }
                _ => err("remove expects a Dict and a key"),
            },
            name if intrinsics::is_dict_remove_extract(name) => match args {
                [Value::Dict(entries), k] => {
                    let mut out = (**entries).clone();
                    let previous = match self.dict_key_position(&out, k)? {
                        Some(index) => Value::Ctor {
                            name: "Some".into(),
                            fields: Rc::new(vec![out.remove(index).1]),
                        },
                        None => Value::ctor("None", Vec::new()),
                    };
                    Ok(Some(Value::tuple(vec![Value::dict(out), previous])))
                }
                _ => err("remove expects a Dict and a key"),
            },
            intrinsics::DICT_KEYS => match args {
                [Value::Dict(entries)] => {
                    Ok(Some(Value::list(entries.iter().map(|(k, _)| k.clone()).collect())))
                }
                _ => err("keys expects a Dict"),
            },
            intrinsics::DICT_VALUES => match args {
                [Value::Dict(entries)] => {
                    Ok(Some(Value::list(entries.iter().map(|(_, v)| v.clone()).collect())))
                }
                _ => err("values expects a Dict"),
            },
            // Each entry as a `(key, value)` tuple, in insertion order.
            intrinsics::DICT_PAIRS => match args {
                [Value::Dict(entries)] => Ok(Some(Value::list(
                    entries
                        .iter()
                        .map(|(k, v)| Value::tuple(vec![k.clone(), v.clone()]))
                        .collect(),
                ))),
                _ => err("pairs expects a Dict"),
            },
            intrinsics::DICT_LENGTH => match args {
                [Value::Dict(entries)] => Ok(Some(Value::Int(entries.len() as i64))),
                _ => err("size expects a Dict"),
            },
            // Filesystem capability (cap-std style): attenuate to a subdirectory.
            "subtree" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Dir(DirValue::Fixture(handle), _), Value::Str(name)] => {
                    match self.invoke_fixture(HostRequest::DirSubdir {
                        dir: *handle,
                        name: name.as_str().to_owned(),
                    })? {
                        HostResponse::Handle(handle) => Ok(Some(Value::Dir(
                            DirValue::Fixture(handle),
                            String::new(),
                        ))),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                // A subtree inherits the parent's entry policy (refinement is monotone).
                // Opening a sub-directory is a directory traversal (RFC-0011 `kind`): a
                // `files()` policy forbids it, an `ext`/empty policy does not.
                [Value::Dir(base, pol), Value::Str(name)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, name, true) {
                        return err(format!("`{name}` is not permitted by this Dir capability's entry policy"));
                    }
                    Ok(Some(Value::Dir(dir_child_value(base, name)?, pol.clone())))
                }
                _ => err("subtree expects a Dir and a name"),
            },
            // RFC-0012 navigation: a `Dir` opens a confined `File`. `read_file`
            // requires the file to exist; `write_file` allows a not-yet-existing target.
            "read_file" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Dir(DirValue::Fixture(handle), _), Value::Str(rel)] => {
                    match self.invoke_fixture(HostRequest::DirOpen {
                        dir: *handle,
                        path: rel.as_str().to_owned(),
                    })? {
                        HostResponse::Handle(handle) => {
                            Ok(Some(Value::File(FileValue::Fixture(handle))))
                        }
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, pol), Value::Str(rel)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    Ok(Some(Value::File(dir_file_value(base, rel, false)?)))
                }
                _ => err("read_file expects a Dir and a relative path"),
            },
            "write_file" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Dir(DirValue::Fixture(handle), _), Value::Str(rel)] => {
                    match self.invoke_fixture(HostRequest::DirCreate {
                        dir: *handle,
                        path: rel.as_str().to_owned(),
                    })? {
                        HostResponse::Handle(handle) => {
                            Ok(Some(Value::File(FileValue::Fixture(handle))))
                        }
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, pol), Value::Str(rel)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    Ok(Some(Value::File(dir_file_value(base, rel, true)?)))
                }
                _ => err("write_file expects a Dir and a relative path"),
            },
            // Spawn a native subprocess. `Exec` is the right to spawn; the
            // executable is named through (and confined to) the `Dir[Read]`, so you
            // can only run a file you can read. The low-level primitive takes argv
            // as a single `\0`-joined string and returns a payload string
            // `"<exit_code>\n<stdout><stderr>"`; the std `exec` module wraps this as
            // `(Int, String)` over a `List(String)`. (One staged-string result, so
            // the compiled backend mirrors `dir_read` exactly — see rfcs/0004.)
            "exec" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::FixtureExec(exec), Value::Dir(DirValue::Fixture(dir), _), Value::Str(path), Value::Str(joined), Value::Str(stdin)] => {
                    let arguments = if joined.is_empty() {
                        Vec::new()
                    } else {
                        joined.split('\0').map(str::to_owned).collect()
                    };
                    match self.invoke_fixture(HostRequest::ExecRun {
                        exec: *exec,
                        dir: *dir,
                        path: path.as_str().to_owned(),
                        arguments,
                        stdin: stdin.as_str().to_owned(),
                    })? {
                        HostResponse::Exec(output) => Ok(Some(Value::str(format!(
                            "{}\n{}{}",
                            output.exit_code, output.stdout, output.stderr
                        )))),
                        other => err(format!(
                            "internal error: Exec fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Cap(Capability::Exec(allow)), Value::Dir(base, pol), Value::Str(path), Value::Str(joined), Value::Str(stdin)] => {
                    if allow.as_ref().is_some_and(|programs| !programs.contains(path.as_str())) {
                        return err(format!(
                            "exec: `{path}` is not in this Exec grant's allow-list"
                        ));
                    }
                    // (RFC-0011) exec is the sharpest right, so it takes the SAME entry-policy
                    // gate as read/write: a `Dir[...].only(...)` may only run a file it admits.
                    if !witchy_caps::capabilities::dir_admits(pol, path, false) {
                        return err(format!("`{path}` is not permitted by this Dir capability's entry policy"));
                    }
                    #[cfg(not(feature = "test-fixtures"))]
                    let DirValue::Fs(base) = base;
                    #[cfg(feature = "test-fixtures")]
                    let base = match base {
                        DirValue::Fs(base) => base,
                        DirValue::Fixture(_) => {
                            return err("exec cannot run programs from a fixture-backed Dir");
                        }
                    };
                    let prog = base
                        .file(path, true)
                        .map_err(|error| RuntimeError { message: error.0 })?;
                    let argv: Vec<&str> =
                        if joined.is_empty() { Vec::new() } else { joined.split('\0').collect() };
                    let output = match prog.run(&argv, stdin) {
                        Ok(o) => o,
                        Err(e) => return err(format!(
                            "exec failed running `{}`: {e}",
                            prog.display_path().display()
                        )),
                    };
                    let code = output.status.code().unwrap_or(-1);
                    let out = String::from_utf8_lossy(&output.stdout);
                    let serr = String::from_utf8_lossy(&output.stderr);
                    Ok(Some(Value::str(format!("{code}\n{out}{serr}"))))
                }
                _ => err("exec expects (Exec, Dir, path, args, stdin)"),
            },
            // Read a file relative to a Dir capability (confined to its subtree).
            "read" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Dir(DirValue::Fixture(handle), _), Value::Str(rel)] => {
                    match self.invoke_fixture(HostRequest::DirRead {
                        dir: *handle,
                        path: rel.as_str().to_owned(),
                    })? {
                        HostResponse::Bytes(bytes) => String::from_utf8(bytes)
                            .map(Value::str)
                            .map(Some)
                            .map_err(|error| RuntimeError {
                                message: format!("read returned invalid UTF-8: {error}"),
                            }),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                #[cfg(feature = "test-fixtures")]
                [Value::File(FileValue::Fixture(handle))] => {
                    match self.invoke_fixture(HostRequest::FileRead { file: *handle })? {
                        HostResponse::Bytes(bytes) => String::from_utf8(bytes)
                            .map(Value::str)
                            .map(Some)
                            .map_err(|error| RuntimeError {
                                message: format!("read returned invalid UTF-8: {error}"),
                            }),
                        other => err(format!(
                            "internal error: File fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, pol), Value::Str(rel)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    Ok(Some(Value::str(read_file_value(&dir_file_value(base, rel, false)?)?)))
                }
                // A `File` is already a confined path; read it directly (RFC-0012).
                [Value::File(file)] => Ok(Some(Value::str(read_file_value(file)?))),
                _ => err("read expects a Dir and a relative path, or a File"),
            },
            // Write a file relative to a Dir capability, confined to its subtree
            // (the target may not exist yet, so confinement is checked via its
            // parent directory).
            "write" => match args {
                #[cfg(feature = "test-fixtures")]
                [
                    Value::Dir(DirValue::Fixture(handle), _),
                    Value::Str(rel),
                    Value::Str(contents),
                ] => {
                    match self.invoke_fixture(HostRequest::DirWrite {
                        dir: *handle,
                        path: rel.as_str().to_owned(),
                        bytes: contents.as_bytes().to_vec(),
                    })? {
                        HostResponse::Count(count) if count == contents.len() => {
                            Ok(Some(Value::Unit))
                        }
                        HostResponse::Count(count) => err(format!(
                            "partial fixture write: wrote {count} of {} bytes",
                            contents.len()
                        )),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                #[cfg(feature = "test-fixtures")]
                [Value::File(FileValue::Fixture(handle)), Value::Str(contents)] => {
                    match self.invoke_fixture(HostRequest::FileWrite {
                        file: *handle,
                        bytes: contents.as_bytes().to_vec(),
                    })? {
                        HostResponse::Count(count) if count == contents.len() => {
                            Ok(Some(Value::Unit))
                        }
                        HostResponse::Count(count) => err(format!(
                            "partial fixture write: wrote {count} of {} bytes",
                            contents.len()
                        )),
                        other => err(format!(
                            "internal error: File fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, pol), Value::Str(rel), Value::Str(contents)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    write_file_value(&dir_file_value(base, rel, true)?, contents)?;
                    Ok(Some(Value::Unit))
                }
                // A `File` is already a confined path; write it directly (RFC-0012).
                [Value::File(file), Value::Str(contents)] => {
                    write_file_value(file, contents)?;
                    Ok(Some(Value::Unit))
                }
                _ => err("write expects a Dir + path + contents, or a File + contents"),
            },
            // Append to a file (creating it if absent) — `write`'s confinement
            // and rights, without clobbering existing contents.
            "append" => match args {
                #[cfg(feature = "test-fixtures")]
                [
                    Value::Dir(DirValue::Fixture(handle), _),
                    Value::Str(rel),
                    Value::Str(contents),
                ] => {
                    match self.invoke_fixture(HostRequest::DirAppend {
                        dir: *handle,
                        path: rel.as_str().to_owned(),
                        bytes: contents.as_bytes().to_vec(),
                    })? {
                        HostResponse::Count(count) if count == contents.len() => {
                            Ok(Some(Value::Unit))
                        }
                        HostResponse::Count(count) => err(format!(
                            "partial fixture append: wrote {count} of {} bytes",
                            contents.len()
                        )),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, pol), Value::Str(rel), Value::Str(contents)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    match dir_file_value(base, rel, true)? {
                        FileValue::Fs(file) => {
                            match file.append_all(contents.as_bytes()) {
                                Ok(()) => Ok(Some(Value::Unit)),
                                Err(e) => err(format!(
                                    "append failed for `{}`: {e}",
                                    file.display_path().display()
                                )),
                            }
                        }
                        #[cfg(feature = "test-fixtures")]
                        FileValue::Fixture(_) => err(
                            "internal error: fixture File bypassed fixture dispatch",
                        ),
                    }
                }
                _ => err("append expects a Dir, a relative path, and contents"),
            },
            // RFC-0095: read a file's RAW bytes (no UTF-8 validation) — a binary
            // artifact — yielding `Bytes`.
            "read_bytes" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Dir(DirValue::Fixture(handle), _), Value::Str(rel)] => {
                    match self.invoke_fixture(HostRequest::DirReadBytes {
                        dir: *handle,
                        path: rel.as_str().to_owned(),
                    })? {
                        HostResponse::Bytes(bytes) => Ok(Some(Value::Bytes(bytes))),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, pol), Value::Str(rel)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    Ok(Some(Value::Bytes(read_file_value_bytes(&dir_file_value(base, rel, false)?)?)))
                }
                _ => err("read_bytes expects a Dir and a relative path"),
            },
            // RFC-0095: write RAW bytes to a file (no UTF-8 requirement).
            "write_bytes" => match args {
                #[cfg(feature = "test-fixtures")]
                [
                    Value::Dir(DirValue::Fixture(handle), _),
                    Value::Str(rel),
                    Value::Bytes(data),
                ] => {
                    match self.invoke_fixture(HostRequest::DirWriteBytes {
                        dir: *handle,
                        path: rel.as_str().to_owned(),
                        bytes: data.clone(),
                    })? {
                        HostResponse::Count(_) => Ok(Some(Value::Unit)),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, pol), Value::Str(rel), Value::Bytes(data)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    write_file_value_bytes(&dir_file_value(base, rel, true)?, data)?;
                    Ok(Some(Value::Unit))
                }
                _ => err("write_bytes expects a Dir, a relative path, and Bytes"),
            },
            // RFC-0118: atomically create `path` with `contents` iff it does not
            // already exist (one `O_CREAT|O_EXCL` step). Returns `true` when this
            // call created the file, `false` when it was already present — the
            // race-loser signal two concurrent creators can rely on.
            "create_new" => match args {
                #[cfg(feature = "test-fixtures")]
                [
                    Value::Dir(DirValue::Fixture(handle), _),
                    Value::Str(rel),
                    Value::Str(contents),
                ] => {
                    match self.invoke_fixture(HostRequest::DirCreateNew {
                        dir: *handle,
                        path: rel.as_str().to_owned(),
                        bytes: contents.as_bytes().to_vec(),
                    })? {
                        HostResponse::Bool(created) => Ok(Some(Value::Bool(created))),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, pol), Value::Str(rel), Value::Str(contents)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    match base {
                        DirValue::Fs(base) => match base.create_new(rel, contents.as_bytes()) {
                            Ok(created) => Ok(Some(Value::Bool(created))),
                            Err(e) => err(format!("create_new failed for `{rel}`: {}", e.0)),
                        },
                        #[cfg(feature = "test-fixtures")]
                        DirValue::Fixture(_) => err(
                            "internal error: fixture Dir bypassed fixture dispatch",
                        ),
                    }
                }
                _ => err("create_new expects a Dir, a relative path, and contents"),
            },
            // RFC-0118: atomically replace `path`'s contents (creating it if absent)
            // via a temp sibling + rename, so a concurrent reader never observes a
            // torn write.
            "replace" => match args {
                #[cfg(feature = "test-fixtures")]
                [
                    Value::Dir(DirValue::Fixture(handle), _),
                    Value::Str(rel),
                    Value::Str(contents),
                ] => {
                    match self.invoke_fixture(HostRequest::DirReplace {
                        dir: *handle,
                        path: rel.as_str().to_owned(),
                        bytes: contents.as_bytes().to_vec(),
                    })? {
                        HostResponse::Unit => Ok(Some(Value::Unit)),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, pol), Value::Str(rel), Value::Str(contents)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, rel, false) {
                        return err(format!("`{rel}` is not permitted by this Dir capability's entry policy"));
                    }
                    match base {
                        DirValue::Fs(base) => match base.replace(rel, contents.as_bytes()) {
                            Ok(()) => Ok(Some(Value::Unit)),
                            Err(e) => err(format!("replace failed for `{rel}`: {}", e.0)),
                        },
                        #[cfg(feature = "test-fixtures")]
                        DirValue::Fixture(_) => err(
                            "internal error: fixture Dir bypassed fixture dispatch",
                        ),
                    }
                }
                _ => err("replace expects a Dir, a relative path, and contents"),
            },
            // RFC-0118: atomically move `from` to `to` within the Dir authority,
            // replacing `to` if present (POSIX renameat replace). Both stay confined.
            "rename" => match args {
                #[cfg(feature = "test-fixtures")]
                [
                    Value::Dir(DirValue::Fixture(handle), _),
                    Value::Str(from),
                    Value::Str(to),
                ] => {
                    match self.invoke_fixture(HostRequest::DirRename {
                        dir: *handle,
                        from: from.as_str().to_owned(),
                        to: to.as_str().to_owned(),
                    })? {
                        HostResponse::Unit => Ok(Some(Value::Unit)),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, pol), Value::Str(from), Value::Str(to)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, from, false) {
                        return err(format!("`{from}` is not permitted by this Dir capability's entry policy"));
                    }
                    if !witchy_caps::capabilities::dir_admits(pol, to, false) {
                        return err(format!("`{to}` is not permitted by this Dir capability's entry policy"));
                    }
                    match base {
                        DirValue::Fs(base) => match base.rename(from, to) {
                            Ok(()) => Ok(Some(Value::Unit)),
                            Err(e) => err(format!("rename failed (`{from}` -> `{to}`): {}", e.0)),
                        },
                        #[cfg(feature = "test-fixtures")]
                        DirValue::Fixture(_) => err(
                            "internal error: fixture Dir bypassed fixture dispatch",
                        ),
                    }
                }
                _ => err("rename expects a Dir and two relative paths"),
            },
            // Whether a file exists within the Dir capability's subtree — total
            // (never errors), so a path outside the subtree, or a missing file,
            // simply reads as `false`. Lets `read` callers avoid a crash.
            "exists" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Dir(DirValue::Fixture(handle), _), Value::Str(rel)] => {
                    match self.invoke_fixture(HostRequest::DirExists {
                        dir: *handle,
                        path: rel.as_str().to_owned(),
                    })? {
                        HostResponse::Bool(value) => Ok(Some(Value::Bool(value))),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, _), Value::Str(rel)] => {
                    let ok = match base {
                        DirValue::Fs(base) => base.exists(rel).unwrap_or(false),
                        #[cfg(feature = "test-fixtures")]
                        DirValue::Fixture(_) => false,
                    };
                    Ok(Some(Value::Bool(ok)))
                }
                _ => err("exists expects a Dir and a relative path"),
            },
            // Whether a path within the Dir capability's subtree is a directory —
            // total (a path outside the subtree or a non-dir reads as `false`), so
            // a caller can walk `src/**` without tripping over a file.
            "is_dir" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Dir(DirValue::Fixture(handle), _), Value::Str(rel)] => {
                    match self.invoke_fixture(HostRequest::DirIsDir {
                        dir: *handle,
                        path: rel.as_str().to_owned(),
                    })? {
                        HostResponse::Bool(value) => Ok(Some(Value::Bool(value))),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, _), Value::Str(rel)] => {
                    let ok = match base {
                        DirValue::Fs(base) => base.is_dir(rel).unwrap_or(false),
                        #[cfg(feature = "test-fixtures")]
                        DirValue::Fixture(_) => false,
                    };
                    Ok(Some(Value::Bool(ok)))
                }
                _ => err("is_dir expects a Dir and a relative path"),
            },
            // List the immediate entries of the Dir capability's own directory, as
            // sorted names (deterministic — `read_dir` order is OS-dependent).
            "list" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Dir(DirValue::Fixture(handle), _)] => {
                    match self.invoke_fixture(HostRequest::DirList { dir: *handle })? {
                        HostResponse::Strings(names) => Ok(Some(Value::list(
                            names.into_iter().map(Value::str).collect(),
                        ))),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, _)] => {
                    let names: Vec<String> = match base {
                        DirValue::Fs(base) => base.entries().map_err(|error| RuntimeError {
                            message: format!(
                                "list failed for `{}`: {error}",
                                base.display_path().display()
                            ),
                        })?,
                        #[cfg(feature = "test-fixtures")]
                        DirValue::Fixture(_) => {
                            return err("internal error: fixture Dir bypassed fixture dispatch");
                        }
                    };
                    Ok(Some(Value::list(names.into_iter().map(Value::str).collect())))
                }
                _ => err("list expects a Dir"),
            },
            // Create a subdirectory within the Dir capability's subtree, confined
            // like `write` (idempotent — succeeds if it already exists). Creating a
            // directory is a directory op (RFC-0011 `kind`): a `files()` policy forbids it.
            "make_dir" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Dir(DirValue::Fixture(handle), _), Value::Str(name)] => {
                    match self.invoke_fixture(HostRequest::DirMakeDir {
                        dir: *handle,
                        path: name.as_str().to_owned(),
                    })? {
                        HostResponse::Unit => Ok(Some(Value::Unit)),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Dir(base, pol), Value::Str(name)] => {
                    if !witchy_caps::capabilities::dir_admits(pol, name, true) {
                        return err(format!("`{name}` is not permitted by this Dir capability's entry policy"));
                    }
                    match base {
                        DirValue::Fs(base) => {
                            match base.make_dir(name) {
                                Ok(()) => Ok(Some(Value::Unit)),
                                Err(e) => err(format!(
                                    "make_dir failed for `{}`: {}",
                                    base.display_path().join(name.as_str()).display(),
                                    e.0
                                )),
                            }
                        }
                        #[cfg(feature = "test-fixtures")]
                        DirValue::Fixture(_) => err(
                            "internal error: fixture Dir bypassed fixture dispatch",
                        ),
                    }
                }
                _ => err("make_dir expects a Dir and a name"),
            },
            // Wall-clock time (milliseconds since the Unix epoch) — requires a
            // `Clock` capability, since reading the real clock is ambient
            // nondeterminism (a side channel), not a pure computation.
            "now" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Cap(Capability::Clock)]
                    if self.fixture_host.is_some() =>
                {
                    match self.invoke_fixture(HostRequest::ClockNow)? {
                        HostResponse::U64(nanoseconds) => {
                            Ok(Some(Value::Int((nanoseconds / 1_000_000) as i64)))
                        }
                        other => err(format!(
                            "internal error: Clock fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Cap(Capability::Clock)] => {
                    let ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    Ok(Some(Value::Int(ms)))
                }
                _ => err("now expects a Clock"),
            },
            // Monotonic elapsed nanoseconds since first use — a steady clock for
            // measuring durations (unaffected by wall-clock adjustments). The
            // process-start reference is lazily set on the first call, so a
            // start/stop bracket around a computation yields its elapsed time.
            "now_monotonic" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Cap(Capability::Clock)]
                    if self.fixture_host.is_some() =>
                {
                    match self.invoke_fixture(HostRequest::ClockNow)? {
                        HostResponse::U64(nanoseconds) => {
                            Ok(Some(Value::Int(nanoseconds as i64)))
                        }
                        other => err(format!(
                            "internal error: Clock fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Cap(Capability::Clock)] => {
                    static START: std::sync::LazyLock<std::time::Instant> =
                        std::sync::LazyLock::new(std::time::Instant::now);
                    Ok(Some(Value::Int(START.elapsed().as_nanos() as i64)))
                }
                _ => err("now_monotonic expects a Clock"),
            },
            // A fresh draw of the `Rand` capability. Seeded (WITCHY_RAND_SEED) it is
            // deterministic and matches the compiled backend bit-for-bit (parity);
            // unseeded the oracle clock-seeds splitmix (the production CSPRNG is the
            // compiled host's getrandom, not this tree-walker).
            "rand_u64" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Cap(Capability::Rand)]
                    if self.fixture_host.is_some() =>
                {
                    match self.invoke_fixture(HostRequest::RandU64)? {
                        HostResponse::U64(value) => {
                            Ok(Some(Value::Int(value as i64)))
                        }
                        other => err(format!(
                            "internal error: Rand fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Cap(Capability::Rand)] => Ok(Some(Value::Int(self.rand_next()))),
                _ => err("rand_u64 expects a Rand"),
            },
            // Read a named environment variable through an `Env` capability:
            // `env.get_env(name) -> Option(String)` (None when unset). Reading the
            // process environment is ambient authority, so it is capability-gated.
            "get_env" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Cap(Capability::Env(allow)), Value::Str(name)]
                    if self.fixture_host.is_some() =>
                {
                    let handle = self
                        .fixture_env_handles
                        .get(allow)
                        .copied()
                        .ok_or_else(|| RuntimeError {
                            message:
                                "invalid or foreign Env fixture capability"
                                    .into(),
                        })?;
                    match self.invoke_fixture(HostRequest::EnvGet {
                        env: handle,
                        name: name.as_str().to_owned(),
                    })? {
                        HostResponse::OptionalString(Some(value)) => Ok(Some(
                            Value::ctor("Some", vec![Value::str(value)]),
                        )),
                        HostResponse::OptionalString(None) => {
                            Ok(Some(Value::ctor("None", Vec::new())))
                        }
                        other => err(format!(
                            "internal error: Env fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Cap(Capability::Env(allow)), Value::Str(name)] => {
                    if allow.as_ref().is_some_and(|names| !names.contains(name.as_str())) {
                        return err(format!(
                            "get_env: `{name}` is not in this Env grant's allow-list"
                        ));
                    }
                    Ok(Some(match std::env::var(name.as_str()) {
                        Ok(v) => Value::ctor("Some", vec![Value::str(v)]),
                        Err(_) => Value::ctor("None", Vec::new()),
                    }))
                }
                _ => err("get_env expects an Env and a variable name"),
            },
            // --- build-time host operations (only reachable from a `build` step) ---
            // Write generated source into the confined per-rune output sandbox.
            "write_out" => match args {
                [Value::Build(BuildCap::Out(base)), Value::Str(rel), Value::Str(contents)] => {
                    let parent = std::path::Path::new(rel.as_str())
                        .parent()
                        .and_then(std::path::Path::to_str)
                        .ok_or_else(|| RuntimeError {
                            message: format!("write_out path `{rel}` is not valid UTF-8"),
                        })?;
                    base.make_dir(parent)
                        .map_err(|error| RuntimeError { message: error.0 })?;
                    let file = base
                        .file(rel, false)
                        .map_err(|error| RuntimeError { message: error.0 })?;
                    match file.write_all(contents.as_bytes()) {
                        Ok(()) => Ok(Some(Value::Unit)),
                        Err(e) => err(format!(
                            "write_out failed for `{}`: {e}",
                            file.display_path().display()
                        )),
                    }
                }
                _ => err("write_out expects a BuildOut, a relative path, and contents"),
            },
            // Read a project file confined to the BuildRead grant's subtree(s).
            // Each granted root is tried in turn; the first that both confines the
            // path and holds the file wins. Confinement (no `..`, no absolute, no
            // symlink escape) is enforced per root, exactly like a runtime `Dir`.
            "read_build" => match args {
                [Value::Build(BuildCap::Read(roots)), Value::Str(rel)] => {
                    if roots.is_empty() {
                        return err("read_build: this BuildRead grant names no readable root");
                    }
                    let mut last_err = None;
                    for base in roots {
                        match base.file(rel, true) {
                            Ok(file) => {
                                match file.read_to_string() {
                                    Ok(contents) => return Ok(Some(Value::str(contents))),
                                    Err(e) => last_err = Some(format!(
                                        "`{}`: {e}",
                                        file.display_path().display()
                                    )),
                                }
                            }
                            Err(e) => last_err = Some(e.0),
                        }
                    }
                    err(format!(
                        "read_build: `{rel}` not found in any granted read root ({})",
                        last_err.unwrap_or_default()
                    ))
                }
                _ => err("read_build expects a BuildRead and a relative path"),
            },
            // Read a named env var, but only one on the BuildEnv allow-list.
            "get_build_env" => match args {
                [Value::Build(BuildCap::Env(env)), Value::Str(name)] => {
                    let value = env.get(name.as_str()).ok_or_else(|| RuntimeError {
                        message: format!(
                            "get_build_env: `{name}` is not in this BuildEnv grant's allow-list"
                        ),
                    })?;
                    Ok(Some(match value {
                        Some(v) => Value::ctor("Some", vec![Value::str(v.as_str())]),
                        None => Value::ctor("None", Vec::new()),
                    }))
                }
                _ => err("get_build_env expects a BuildEnv and a variable name"),
            },
            // Fetch over HTTP at build time — but only from a host on the BuildNet
            // grant's allow-list (`host:port` form, exact match — the same shape as
            // the runtime Net allow-list). Returns the response body. The fetched
            // bytes are data, not authority: anything the build step *generates*
            // from them is re-audited against the locked footprint, and
            // BuildNet/BuildExec use marks the build `pinned-only` for determinism.
            "fetch_build" => match args {
                [Value::Build(BuildCap::Net(allow)), Value::Str(host), Value::Str(path)] => {
                    if !allow.iter().any(|h| *h == **host) {
                        return err(format!(
                            "fetch_build: `{host}` is not in this BuildNet grant's allow-list"
                        ));
                    }
                    use std::io::{Read, Write};
                    let mut sock = std::net::TcpStream::connect(host.as_str()).map_err(|e| {
                        RuntimeError {
                            message: format!("fetch_build: cannot connect to `{host}`: {e}"),
                        }
                    })?;
                    let hostname = host.split(':').next().unwrap_or(host);
                    let req = format!(
                        "GET {path} HTTP/1.1\r\nHost: {hostname}\r\nConnection: close\r\n\r\n"
                    );
                    sock.write_all(req.as_bytes()).map_err(|e| RuntimeError {
                        message: format!("fetch_build: sending to `{host}`: {e}"),
                    })?;
                    let mut raw = Vec::new();
                    sock.read_to_end(&mut raw).map_err(|e| RuntimeError {
                        message: format!("fetch_build: reading from `{host}`: {e}"),
                    })?;
                    let text = String::from_utf8_lossy(&raw);
                    let body = match text.split_once("\r\n\r\n") {
                        Some((_, b)) => b.to_string(),
                        None => text.into_owned(),
                    };
                    Ok(Some(Value::str(body)))
                }
                _ => err("fetch_build expects a BuildNet, a host, and a path"),
            },
            // Invoke an external tool — but only one named on the BuildExec grant's
            // allow-list. `input` is fed on stdin; stdout is returned. This is the
            // "native toolchain escape hatch" (§7.1): the allow-list is the
            // confinement, since the tool itself runs as a native process.
            "run_tool" => match args {
                [Value::Build(BuildCap::Exec(allow)), Value::Str(tool), Value::Str(input)] => {
                    if !allow.iter().any(|t| *t == **tool) {
                        return err(format!(
                            "run_tool: `{tool}` is not in this BuildExec grant's allow-list"
                        ));
                    }
                    use std::io::Write;
                    use std::process::{Command, Stdio};
                    let mut child = Command::new(tool.as_str())
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .map_err(|e| RuntimeError {
                            message: format!("run_tool: cannot start `{tool}`: {e}"),
                        })?;
                    if let Some(mut stdin) = child.stdin.take() {
                        stdin.write_all(input.as_bytes()).map_err(|e| RuntimeError {
                            message: format!("run_tool: writing to `{tool}` stdin: {e}"),
                        })?;
                    }
                    let out = child.wait_with_output().map_err(|e| RuntimeError {
                        message: format!("run_tool: `{tool}` failed: {e}"),
                    })?;
                    if !out.status.success() {
                        return err(format!("run_tool: `{tool}` exited with {}", out.status));
                    }
                    Ok(Some(Value::str(String::from_utf8_lossy(&out.stdout).into_owned())))
                }
                _ => err("run_tool expects a BuildExec, a tool name, and input"),
            },
            "fetch" => match args {
                [Value::Net(allow), Value::Str(origins)] => {
                    witchy_runtime::fetch::FetchPolicy::from_net(
                        allow,
                        origins.lines().map(str::to_owned),
                    )
                    .map(Value::Fetch)
                    .map(Some)
                    .map_err(|error| RuntimeError {
                        message: format!("net.fetch: {error}"),
                    })
                }
                _ => err("fetch expects a Net and newline-separated origins"),
            },
            // RFC-0011 typed verbs: the argument is a `NetPolicy` carrying one or more address
            // patterns (a `confine.union` joins them, newline-separated). `only` narrows to the
            // set; `deny` subtracts it (a monotone exclusion recorded as `!`-prefixed entries
            // the shared `net_allows` honours).
            "only" => match args {
                #[cfg(feature = "test-fixtures")]
                [Value::Cap(Capability::Env(allow)), Value::List(names)]
                    if self.fixture_host.is_some() =>
                {
                    let requested = names
                        .iter()
                        .map(|name| match name {
                            Value::Str(name) => {
                                Ok(name.as_str().to_owned())
                            }
                            _ => err("Env.only expects a List(String)"),
                        })
                        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
                    let current_handle = self
                        .fixture_env_handles
                        .get(allow)
                        .copied()
                        .ok_or_else(|| RuntimeError {
                            message:
                                "invalid or foreign Env fixture capability"
                                    .into(),
                        })?;
                    let effective = match allow {
                        Some(current) => requested
                            .intersection(current)
                            .cloned()
                            .collect(),
                        None => requested.clone(),
                    };
                    let requested_names =
                        requested.into_iter().collect::<Vec<_>>();
                    match self.invoke_fixture(HostRequest::EnvOnly {
                        env: current_handle,
                        names: requested_names,
                    })? {
                        HostResponse::Handle(handle) => {
                            self.fixture_env_handles
                                .insert(Some(effective.clone()), handle);
                            Ok(Some(Value::Cap(Capability::Env(Some(
                                effective,
                            )))))
                        }
                        other => err(format!(
                            "internal error: Env fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Cap(Capability::Exec(allow)), Value::List(programs)] => {
                    let requested = programs
                        .iter()
                        .map(|program| match program {
                            Value::Str(program) => Ok(program.as_str().to_string()),
                            _ => err("Exec.only expects a List(String)"),
                        })
                        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
                    let narrowed = match allow {
                        Some(current) => requested.intersection(current).cloned().collect(),
                        None => requested,
                    };
                    Ok(Some(Value::Cap(Capability::Exec(Some(narrowed)))))
                }
                #[cfg(feature = "test-fixtures")]
                [Value::FixtureExec(exec), Value::List(programs)] => {
                    let tools = programs
                        .iter()
                        .map(|program| match program {
                            Value::Str(program) => Ok(program.as_str().to_owned()),
                            _ => err("Exec.only expects a List(String)"),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    match self.invoke_fixture(HostRequest::ExecOnly {
                        exec: *exec,
                        tools,
                    })? {
                        HostResponse::Handle(handle) => {
                            Ok(Some(Value::FixtureExec(handle)))
                        }
                        other => err(format!(
                            "internal error: Exec fixture returned {other:?}"
                        )),
                    }
                }
                #[cfg(feature = "test-fixtures")]
                [Value::FixtureFetch(handle), Value::Str(origins)] => {
                    let origins = origins.lines().map(str::to_owned).collect::<Vec<_>>();
                    match self.invoke_fixture(HostRequest::FetchOnly {
                        fetch: *handle,
                        origins,
                    })? {
                        HostResponse::Handle(handle) => {
                            Ok(Some(Value::FixtureFetch(handle)))
                        }
                        other => err(format!(
                            "internal error: Fetch fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Cap(Capability::Env(allow)), Value::List(names)] => {
                    let requested = names
                        .iter()
                        .map(|name| match name {
                            Value::Str(name) => Ok(name.as_str().to_string()),
                            _ => err("Env.only expects a List(String)"),
                        })
                        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
                    let narrowed = match allow {
                        Some(current) => requested.intersection(current).cloned().collect(),
                        None => requested,
                    };
                    Ok(Some(Value::Cap(Capability::Env(Some(narrowed)))))
                }
                [Value::Fetch(policy), Value::Str(origins)] => policy
                    .only(origins.lines().map(str::to_owned))
                    .map(Value::Fetch)
                    .map(Some)
                    .map_err(|error| RuntimeError {
                        message: format!("fetch.only: {error}"),
                    }),
                #[cfg(feature = "test-fixtures")]
                [Value::Dir(DirValue::Fixture(handle), _), Value::Ctor { fields, .. }]
                    if fields.len() == 1 =>
                {
                    let Value::Str(refine) = &fields[0] else {
                        return err("only expects a DirPolicy");
                    };
                    match self.invoke_fixture(HostRequest::DirOnly {
                        dir: *handle,
                        refine: refine.as_str().to_owned(),
                    })? {
                        HostResponse::Handle(handle) => Ok(Some(Value::Dir(
                            DirValue::Fixture(handle),
                            refine.as_str().to_owned(),
                        ))),
                        other => err(format!(
                            "internal error: Dir fixture returned {other:?}"
                        )),
                    }
                }
                [Value::Net(allow), Value::Ctor { fields, .. }] if fields.len() == 1 => {
                    let Value::Str(addr) = &fields[0] else {
                        return err("only expects a NetPolicy");
                    };
                    Ok(Some(net_narrow_to(allow, addr)?))
                }
                // RFC-0011: `dir.only(DirPolicy)` narrows the Dir's entry policy.
                [Value::Dir(base, pol), Value::Ctor { fields, .. }] if fields.len() == 1 => {
                    let Value::Str(refine) = &fields[0] else {
                        return err("only expects a DirPolicy");
                    };
                    Ok(Some(Value::Dir(
                        base.clone(),
                        witchy_caps::capabilities::dir_only(pol, refine),
                    )))
                }
                _ => err(
                    "only expects an Env/Exec and names, a Fetch and origins, a Net and a NetPolicy, or a Dir and a DirPolicy",
                ),
            },
            "send_raw" => match args {
                #[cfg(feature = "test-fixtures")]
                [
                    Value::FixtureFetch(handle),
                    Value::Str(method),
                    Value::Str(url),
                    Value::Str(headers),
                    Value::Str(body),
                ] => {
                    let request = witchy_testkit::FixtureFetchRequest {
                        method: method.as_ref().clone(),
                        url: url.as_ref().clone(),
                        headers: headers
                            .lines()
                            .filter_map(|line| line.split_once(':'))
                            .map(|(name, value)| {
                                (name.trim().to_owned(), value.trim().to_owned())
                            })
                            .collect(),
                        body: body.as_bytes().to_vec(),
                    };
                    match self.invoke_fixture_raw(HostRequest::FetchSend {
                        fetch: *handle,
                        request,
                    }) {
                        Ok(HostResponse::Fetch(response)) => {
                            let mut raw = format!("HTTP/1.1 {}\r\n", response.status);
                            for (name, value) in response.headers {
                                raw.push_str(&name);
                                raw.push_str(": ");
                                raw.push_str(&value);
                                raw.push_str("\r\n");
                            }
                            raw.push_str("\r\n");
                            raw.push_str(&String::from_utf8_lossy(&response.body));
                            Ok(Some(Value::str(raw)))
                        }
                        Ok(other) => err(format!(
                            "internal error: Fetch fixture returned {other:?}"
                        )),
                        Err(failure) => {
                            let code = match failure.code {
                                FixtureErrorCode::Denied
                                | FixtureErrorCode::PermissionDenied => Some("denied"),
                                FixtureErrorCode::InvalidRequest => Some("invalid-request"),
                                FixtureErrorCode::Timeout => Some("timeout"),
                                FixtureErrorCode::Redirect => Some("redirect"),
                                FixtureErrorCode::Network => Some("network"),
                                FixtureErrorCode::InvalidData => Some("malformed-response"),
                                FixtureErrorCode::ResponseTooLarge => Some("response-too-large"),
                                _ => None,
                            };
                            match code {
                                Some(code) => Ok(Some(Value::str(format!(
                                    "WITCHY_FETCH_ERROR:{code}:{}",
                                    failure.message
                                )))),
                                None => Err(Self::fixture_failure_error(failure)),
                            }
                        }
                    }
                }
                [
                    Value::Fetch(policy),
                    Value::Str(method),
                    Value::Str(url),
                    Value::Str(headers),
                    Value::Str(body),
                ] => {
                    let request = witchy_runtime::fetch::FetchRequest {
                        method: method.as_ref().clone(),
                        url: url.as_ref().clone(),
                        headers: headers
                            .lines()
                            .filter_map(|line| line.split_once(':'))
                            .map(|(name, value)| {
                                (name.trim().to_string(), value.trim().to_string())
                            })
                            .collect(),
                        body: body.as_bytes().to_vec(),
                    };
                    let payload = match witchy_runtime::fetch::send(policy, &request) {
                        Ok(response) => {
                            let mut raw = format!("HTTP/1.1 {}\r\n", response.status);
                            for (name, value) in response.headers {
                                raw.push_str(&name);
                                raw.push_str(": ");
                                raw.push_str(&value);
                                raw.push_str("\r\n");
                            }
                            raw.push_str("\r\n");
                            raw.push_str(&String::from_utf8_lossy(&response.body));
                            raw
                        }
                        Err(error) => {
                            format!("WITCHY_FETCH_ERROR:{}:{error}", error.code())
                        }
                    };
                    Ok(Some(Value::str(payload)))
                }
                _ => err("send_raw expects (Fetch, method, url, headers, body)"),
            },
            "deny" => match args {
                [Value::Net(allow), Value::Ctor { fields, .. }] if fields.len() == 1 => {
                    let Value::Str(addr) = &fields[0] else {
                        return err("deny expects a NetPolicy");
                    };
                    let mut next = allow.clone();
                    for p in addr.split('\n') {
                        next.push(format!("!{p}"));
                    }
                    Ok(Some(Value::Net(next)))
                }
                _ => err("deny expects a Net and a NetPolicy"),
            },
            // Connect only to an address the Net capability permits.
            "connect" => match args {
                [Value::Net(allow), Value::Str(addr)] => {
                    let (tls, host_port) = witchy_runtime::net::parse_scheme(addr);
                    let targets = match witchy_caps::capabilities::resolve_admitted(allow, host_port) {
                        Ok(t) => t,
                        Err(e) => return err(format!("connect: {e}")),
                    };
                    match witchy_runtime::net::dial(&targets, tls, host_port) {
                        Ok(stream) => {
                            let id = self.sockets.len();
                            self.sockets.push(BufReader::new(stream));
                            Ok(Some(Value::Socket(id)))
                        }
                        Err(e) => err(format!("connect to `{addr}` failed: {e}")),
                    }
                }
                _ => err("connect expects a Net and an address"),
            },
            "try_connect" => match args {
                [Value::Net(allow), Value::Str(addr)] => {
                    let (tls, host_port) = witchy_runtime::net::parse_scheme(addr);
                    let targets = match witchy_caps::capabilities::resolve_admitted(allow, host_port) {
                        Ok(t) => t,
                        Err(e) => return err(format!("try_connect: {e}")),
                    };
                    let v = match witchy_runtime::net::dial(&targets, tls, host_port) {
                        Ok(stream) => {
                            let id = self.sockets.len();
                            self.sockets.push(BufReader::new(stream));
                            Value::ctor("Some", vec![Value::Socket(id)])
                        }
                        Err(_) => Value::ctor("None", Vec::new()),
                    };
                    Ok(Some(v))
                }
                _ => err("try_connect expects a Net and an address"),
            },
            // (RFC-0020) Resolve a hostname to its current IP literals. No allowlist
            // filtering — the program inspects the IPs and `connect_pinned` re-checks
            // the chosen one, so resolve adds no authority beyond `connect`. An empty
            // list signals a resolution failure (the std wrapper turns it into `Err`).
            "resolve" => match args {
                [Value::Net(_allow), Value::Str(host)] => {
                    let ips = witchy_runtime::net::resolve_ips(host);
                    Ok(Some(Value::list(ips.into_iter().map(Value::str).collect())))
                }
                _ => err("resolve expects a Net and a host"),
            },
            // (RFC-0020) Dial the EXACT `ip:port` — no DNS — while presenting `host` as
            // the TLS SNI / `Host`. The Net allowlist is still enforced on `ip` (a literal
            // IP resolves to itself), so a pin can never exceed the capability. This is
            // what closes the DNS-rebinding TOCTOU: the checked IP is the dialed IP.
            "connect_pinned" => match args {
                [Value::Net(allow), Value::Str(ip), Value::Str(host), Value::Int(port), Value::Bool(secure)] => {
                    let ip_port = witchy_runtime::net::authority(ip, *port);
                    let targets = match witchy_caps::capabilities::resolve_admitted(allow, &ip_port) {
                        Ok(t) => t,
                        Err(e) => return err(format!("connect_pinned: {e}")),
                    };
                    let host_port = witchy_runtime::net::authority(host, *port);
                    match witchy_runtime::net::dial(&targets, *secure, &host_port) {
                        Ok(stream) => {
                            let id = self.sockets.len();
                            self.sockets.push(BufReader::new(stream));
                            Ok(Some(Value::Socket(id)))
                        }
                        Err(e) => err(format!("connect_pinned to `{ip_port}` failed: {e}")),
                    }
                }
                _ => err("connect_pinned expects (Net, ip, host, port, secure)"),
            },
            "try_connect_pinned" => match args {
                [Value::Net(allow), Value::Str(ip), Value::Str(host), Value::Int(port), Value::Bool(secure)] => {
                    let ip_port = witchy_runtime::net::authority(ip, *port);
                    // A capability breach still traps; only a transient dial failure -> None.
                    let targets = match witchy_caps::capabilities::resolve_admitted(allow, &ip_port) {
                        Ok(t) => t,
                        Err(e) => return err(format!("try_connect_pinned: {e}")),
                    };
                    let host_port = witchy_runtime::net::authority(host, *port);
                    let v = match witchy_runtime::net::dial(&targets, *secure, &host_port) {
                        Ok(stream) => {
                            let id = self.sockets.len();
                            self.sockets.push(BufReader::new(stream));
                            Value::ctor("Some", vec![Value::Socket(id)])
                        }
                        Err(_) => Value::ctor("None", Vec::new()),
                    };
                    Ok(Some(v))
                }
                _ => err("try_connect_pinned expects (Net, ip, host, port, secure)"),
            },
            "send_line" => match args {
                [Value::Socket(id), Value::Str(line)] => {
                    let sock = self
                        .sockets
                        .get_mut(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid socket".into() })?;
                    sock.get_mut()
                        .write_all(line.as_bytes())
                        .and_then(|_| sock.get_mut().write_all(b"\n"))
                        .map_err(|e| RuntimeError { message: format!("send failed: {e}") })?;
                    Ok(Some(Value::Unit))
                }
                _ => err("send_line expects a Socket and a String"),
            },
            "recv_line" => match args {
                [Value::Socket(id)] => {
                    let sock = self
                        .sockets
                        .get_mut(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid socket".into() })?;
                    // (SEC-035) Shared, bounded read so a peer that never sends a newline
                    // can't OOM the host — same cap + logic as the compiled backend.
                    let raw = witchy_runtime::net::read_line_capped(sock)
                        .map_err(|e| RuntimeError { message: e.to_string() })?;
                    let line = String::from_utf8_lossy(&raw);
                    Ok(Some(Value::str(line.trim_end_matches('\n'))))
                }
                _ => err("recv_line expects a Socket"),
            },
            // Write raw bytes to the socket with no trailing newline — for
            // sending an exact request (headers + body) where `send_line`'s
            // appended `\n` would corrupt the framing.
            "send_bytes" => match args {
                [Value::Socket(id), Value::Str(s)] => {
                    let sock = self
                        .sockets
                        .get_mut(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid socket".into() })?;
                    sock.get_mut()
                        .write_all(s.as_bytes())
                        .map_err(|e| RuntimeError { message: format!("send failed: {e}") })?;
                    Ok(Some(Value::Unit))
                }
                _ => err("send_bytes expects a Socket and a String"),
            },
            // Read the rest of the connection to EOF (the peer closing the
            // connection ends it) — e.g. an HTTP `Connection: close` response.
            "recv_all" => match args {
                [Value::Socket(id)] => {
                    let sock = self
                        .sockets
                        .get_mut(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid socket".into() })?;
                    // (SEC-035) Cap the read so a peer streaming without EOF can't OOM the
                    // host — one byte past the cap detects overflow; same as the compiled side.
                    use std::io::Read;
                    let mut buf = Vec::new();
                    match sock
                        .by_ref()
                        .take(witchy_runtime::net::MAX_RECV_BYTES + 1)
                        .read_to_end(&mut buf)
                    {
                        Ok(_) => {}
                        // A stalled server read (timeout) ends the stream cleanly: keep the
                        // bytes so far rather than erroring out the worker. Matches the
                        // compiled backend (`host_net_recv_all_len`).
                        Err(e) if witchy_runtime::net::is_read_timeout(&e) => {}
                        Err(e) => {
                            return Err(RuntimeError { message: format!("recv failed: {e}") });
                        }
                    }
                    if buf.len() as u64 > witchy_runtime::net::MAX_RECV_BYTES {
                        return Err(RuntimeError {
                            message: format!(
                                "recv_all exceeded the {}-byte cap",
                                witchy_runtime::net::MAX_RECV_BYTES
                            ),
                        });
                    }
                    Ok(Some(Value::str(String::from_utf8_lossy(&buf).into_owned())))
                }
                _ => err("recv_all expects a Socket"),
            },
            // Read exactly `n` bytes from the socket — for a request/response body
            // of a known `Content-Length`. Returns fewer bytes only if the peer
            // closes early.
            "recv_bytes" => match args {
                [Value::Socket(id), Value::Int(n)] => {
                    let sock = self
                        .sockets
                        .get_mut(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid socket".into() })?;
                    let want = (*n).max(0) as usize;
                    // `want` is attacker-controlled (an HTTP Content-Length, up to i64::MAX);
                    // do NOT pre-allocate `vec![0u8; want]` — a peer that sends a huge count
                    // but few bytes would OOM the host before a single byte arrives. Read in
                    // bounded chunks so memory tracks bytes actually received, matching the
                    // compiled runtime (`host_net_recv_bytes_len`). (BUG-065)
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 8192];
                    while buf.len() < want {
                        let to_read = (want - buf.len()).min(chunk.len());
                        match sock.read(&mut chunk[..to_read]) {
                            Ok(0) => break,
                            Ok(k) => buf.extend_from_slice(&chunk[..k]),
                            // A stalled server read (timeout) ends the body early, like a
                            // peer that closed mid-stream — return what arrived rather than
                            // crashing the worker. Matches `host_net_recv_bytes_len`.
                            Err(e) if witchy_runtime::net::is_read_timeout(&e) => break,
                            Err(e) => return err(format!("recv failed: {e}")),
                        }
                    }
                    Ok(Some(Value::str(String::from_utf8_lossy(&buf).into_owned())))
                }
                _ => err("recv_bytes expects a Socket and an Int"),
            },
            // Bind and listen on an address the Net capability permits — the
            // server side of the network capability. Returns a `Listener`.
            "listen" => match args {
                [Value::Net(allow), Value::Str(addr)] => {
                    if !witchy_caps::capabilities::net_allows(allow, addr) {
                        return err(format!("listen: `{addr}` is not permitted by this Net capability"));
                    }
                    match TcpListener::bind(addr.as_str()) {
                        Ok(listener) => {
                            let id = self.listeners.len();
                            self.listeners.push((listener, None));
                            Ok(Some(Value::Listener(id)))
                        }
                        Err(e) => err(format!("listen on `{addr}` failed: {e}")),
                    }
                }
                _ => err("listen expects a Net and an address"),
            },
            // (RFC-0060) Bind an HTTPS listener. The rustls config is built ONCE here
            // — through the SAME shared module the compiled runtime uses — from the
            // certificate PEM and the key `Secret`'s host-side bytes; malformed or
            // mismatched material is a loud listen-time error. Accepts handshake
            // host-side and yield ordinary `Socket`s.
            "listen_tls" => match args {
                [Value::Net(allow), Value::Str(addr), Value::Str(cert_pem), Value::Secret(key_bytes, _)] => {
                    if !witchy_caps::capabilities::net_allows(allow, addr) {
                        return err(format!("listen: `{addr}` is not permitted by this Net capability"));
                    }
                    let config = match witchy_runtime::net::server_tls_config(cert_pem, key_bytes) {
                        Ok(config) => config,
                        Err(message) => return err(message),
                    };
                    match TcpListener::bind(addr.as_str()) {
                        Ok(listener) => {
                            let id = self.listeners.len();
                            self.listeners.push((listener, Some(config)));
                            Ok(Some(Value::Listener(id)))
                        }
                        Err(e) => err(format!("listen on `{addr}` failed: {e}")),
                    }
                }
                _ => err("listen_tls expects a Net, an address, a certificate PEM, and a Secret key"),
            },
            // Block until a client connects, returning the connection `Socket`. On a
            // TLS listener the handshake completes host-side first; a failed handshake
            // (plaintext client, bad ClientHello) drops that connection and keeps
            // accepting — connection weather, not a program error (RFC-0060).
            "accept" => match args {
                [Value::Listener(id)] => loop {
                    let (listener, tls) = self
                        .listeners
                        .get(*id)
                        .ok_or_else(|| RuntimeError { message: "invalid listener".into() })?;
                    match listener.accept() {
                        Ok((stream, _peer)) => {
                            // (Availability) Bound a stalled peer's hold on this worker's
                            // request read — set before any TLS handshake so a slow-loris
                            // there is bounded too. Mirrors the compiled accept loop.
                            witchy_runtime::net::prepare_accepted_socket(&stream);
                            let stream: Box<dyn Stream> = match tls {
                                None => Box::new(stream),
                                Some(config) => {
                                    match witchy_runtime::net::accept_tls(config.clone(), stream) {
                                        Ok(tls_stream) => tls_stream,
                                        Err(_) => continue,
                                    }
                                }
                            };
                            let sid = self.sockets.len();
                            self.sockets.push(BufReader::new(stream));
                            return Ok(Some(Value::Socket(sid)));
                        }
                        Err(e) => return err(format!("accept failed: {e}")),
                    }
                },
                _ => err("accept expects a Listener"),
            },
            // (RFC-0032) The compiled runtime's `serve_pool` spawns one worker VM per
            // core sharing the bound listener. The interpreter is a single VM (the
            // parity oracle), so the pool is the identity here: `serve`/`serve_tls`
            // fall through to their own accept loop, single-core — the same observable
            // request/response behavior, minus the scale-out.
            "serve_pool" => match args {
                [Value::Listener(_)] => Ok(Some(Value::Unit)),
                _ => err("serve_pool expects a Listener"),
            },
            // Close a connected socket (e.g. after sending a `Connection: close`
            // response). Idempotent; an already-closed socket is not an error.
            "close" => match args {
                [Value::Socket(id)] => {
                    if let Some(sock) = self.sockets.get_mut(*id) {
                        sock.get_mut().shutdown();
                    }
                    Ok(Some(Value::Unit))
                }
                _ => err("close expects a Socket"),
            },
            _ if catalog.is_some_and(|spec| {
                spec.runtime == intrinsics::IntrinsicRuntime::InterpreterBuiltin
            }) => err(format!(
                "internal error: cataloged interpreter builtin `{name}` has no dispatch arm"
            )),
            _ => Ok(None),
        }
    }
}

fn compiler_impl_head(
    ty: Type,
    role: &str,
    operation: &str,
) -> Result<(String, Vec<Type>), RuntimeError> {
    match ty {
        Type::Named(name, args) => Ok((name, args)),
        _ => err(format!(
            "{operation} expects its {role} TypeSyntax to name a trait or concrete type"
        )),
    }
}

fn compiler_impl_methods(
    value: &Value,
    item_syntax: &HashMap<String, Item>,
) -> Result<Vec<Function>, RuntimeError> {
    let Value::List(items) = value else {
        return err("meta.impl_block expects a list of method ItemSyntax values");
    };
    let mut methods = Vec::with_capacity(items.len());
    for value in items.iter() {
        let item = match value {
            Value::Ctor { name, fields }
                if &**name == OWNED_ITEM_SYNTAX_CTOR
                    && matches!(fields.as_slice(), [Value::Str(_), Value::Int(_)]) =>
            {
                let Value::Str(handle) = &fields[0] else {
                    unreachable!()
                };
                item_syntax.get(handle.as_str()).cloned().ok_or_else(|| RuntimeError {
                    message: "meta.impl_block referenced an invalid compiler-owned item handle"
                        .into(),
                })?
            }
            _ => {
                return err(
                    "meta.impl_block methods must be compiler-owned function ItemSyntax values",
                )
            }
        };
        let Item::Function(function) = item else {
            return err("meta.impl_block accepts function items only");
        };
        methods.push(function);
    }
    Ok(methods)
}
