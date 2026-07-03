//! Recursive-descent parser with a Pratt expression core.
//!
//! Pipelines are desugared at parse time: `x |> f(a)` becomes `f(x, a)`.

use std::fmt;

use crate::ast::*;
use crate::lexer::{tokenize, Tok, Token};

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "parse error at {}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for ParseError {}

pub fn parse_module(src: &str) -> Result<Module, ParseError> {
    let tokens = tokenize(src).map_err(|e| ParseError {
        message: e.message,
        line: e.line,
        col: e.col,
    })?;
    // Off-side-rule layout: indentation-delimited blocks become brace-delimited
    // ones (a no-op for code that already uses explicit braces).
    let tokens = crate::lexer::apply_layout(tokens);
    let mut parser = Parser::new(tokens);
    let mut module = parser.module()?;
    // Each anonymous struct `.{…}` becomes a generic synthetic record carrying
    // `derive(Reflect)`, prepended to the module: `__anonN(t0, …)` with one type
    // parameter per field. Generic-record derive makes `.{…}` ordinary reflectable
    // data with no special builtin.
    if !parser.anon_records.is_empty() {
        let mut defs = String::new();
        for (idx, fields) in parser.anon_records.iter().enumerate() {
            let params: Vec<String> = (0..fields.len()).map(|i| format!("t{i}")).collect();
            defs.push_str(&format!("type __anon{idx}({}) derive(Reflect):\n", params.join(", ")));
            for (i, f) in fields.iter().enumerate() {
                defs.push_str(&format!("    {f}: t{i}\n"));
            }
            defs.push('\n');
        }
        // `defs` has no `.{…}`, so this does not recurse further.
        let synth = parse_module(&defs)?;
        let n = synth.items.len();
        let mut items = synth.items;
        items.append(&mut module.items);
        module.items = items;
        let mut item_lines = vec![u32::MAX; n];
        item_lines.append(&mut module.item_lines);
        module.item_lines = item_lines;
        // The synthetic types `derive(Reflect)`, so the module needs `reflect` in
        // scope (the linker always bundles it; this satisfies the derive check).
        if !module.imports.iter().any(|i| i == "reflect") {
            module.imports.push("reflect".to_string());
            module.import_lines.push(u32::MAX);
        }
    }
    Ok(module)
}


struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// True while parsing a match-arm body. Match arms have no separator, so a
    /// `-` that begins a new line ends the arm (it starts the next arm's
    /// negative-literal pattern) rather than continuing the body as subtraction.
    in_match_arm: bool,
    /// Counter for fresh accumulator names in desugared list comprehensions.
    compr_counter: usize,
    /// Imported module names, so `mod.func(...)` (module-qualified call) can be
    /// told apart from `value.method(...)` (UFCS method call) after `.`.
    imports: std::collections::HashSet<String>,
    /// `impl Trait` parameter bounds collected while parsing one function's
    /// params — `fn f(x: impl Show)` desugars to a fresh type var plus a
    /// `where`-style bound, reusing the whole trait/monomorphization path.
    pending_impl_bounds: Vec<(String, String, Vec<Type>)>,
    /// True while parsing the body of an `async fn`; gates the `.await` postfix.
    in_async: bool,
    /// Distinct field-name sets (sorted) of the anonymous structs `.{…}` seen, in
    /// first-seen order. Each becomes a generic synthetic record `__anonN(t0, …)
    /// derive(Reflect)` prepended to the module, so `.{a: x}` is ordinary
    /// reflectable data — `json.stringify(.{…})`, `debug(.{…})` — with no builtins.
    anon_records: Vec<Vec<String>>,
}

impl Parser {
    fn new(toks: Vec<Token>) -> Self {
        Self {
            toks,
            pos: 0,
            in_match_arm: false,
            compr_counter: 0,
            // The prelude modules qualify without an import line (the linker
            // always bundles them): `list.push(...)` parses as a qualified
            // call everywhere, including inside the std modules themselves.
            // `task`/`chan` are seeded too: the `async`/`await` lowering implies
            // and auto-imports them, so `task.spawn(...)` / `chan.send(...)` parse
            // as qualified calls in async code without an explicit import line.
            imports: [
                "list", "string", "dict", "math", "option", "result", "task", "chan",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            pending_impl_bounds: Vec::new(),
            in_async: false,
            anon_records: Vec::new(),
        }
    }

    // --- token cursor helpers ---

    fn cur(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn kind(&self) -> &Tok {
        &self.toks[self.pos].kind
    }

    fn at_ident(&self, name: &str) -> bool {
        matches!(self.kind(), Tok::Ident(n) if n == name)
    }

    fn at(&self, k: &Tok) -> bool {
        self.kind() == k
    }

    /// Whether the current token sits on the same source line as the previously
    /// consumed one. Used to keep postfix application (`f(x)(y)`) from spanning
    /// a line break into the next statement.
    fn on_same_line_as_prev(&self) -> bool {
        self.pos > 0 && self.cur().line == self.toks[self.pos - 1].line
    }

    fn advance(&mut self) -> Tok {
        let k = self.toks[self.pos].kind.clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        k
    }

    fn eat(&mut self, k: &Tok) -> bool {
        if self.at(k) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, k: &Tok) -> Result<(), ParseError> {
        if self.at(k) {
            self.advance();
            Ok(())
        } else {
            Err(self.error(format!("expected `{k}`, found `{}`", self.kind())))
        }
    }

    fn error(&self, message: impl Into<String>) -> ParseError {
        ParseError {
            message: message.into(),
            line: self.cur().line,
            col: self.cur().col,
        }
    }

    fn ident(&mut self) -> Result<String, ParseError> {
        match self.kind().clone() {
            Tok::Ident(name) => {
                self.advance();
                Ok(name)
            }
            other => Err(self.error(format!("expected an identifier, found `{other}`"))),
        }
    }

    // --- top level ---

    fn module(&mut self) -> Result<Module, ParseError> {
        // The performance mode `mode opt` leads the file. `mode` is a contextual
        // keyword — recognized only here, so it stays usable as an ordinary
        // identifier everywhere else. See rfcs/performance-modes.md.
        let mut modes = Vec::new();
        while self.at_ident("mode") {
            self.advance();
            loop {
                let name = self.ident()?;
                if name != "opt" {
                    return Err(self.error(format!(
                        "unknown performance mode `{name}` — the only mode is `opt`"
                    )));
                }
                if !modes.contains(&name) {
                    modes.push(name);
                }
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }
        // Imports come next: `import name` — declarations only, no code runs.
        let mut imports = Vec::new();
        let mut import_lines = Vec::new();
        while self.at(&Tok::Import) {
            import_lines.push(self.cur().line);
            self.advance();
            let name = self.ident()?;
            self.imports.insert(name.clone());
            imports.push(name);
        }
        let mut items = Vec::new();
        let mut item_lines = Vec::new();
        while !self.at(&Tok::Eof) {
            item_lines.push(self.cur().line);
            items.push(self.item()?);
        }
        Ok(Module {
            modes,
            imports,
            items,
            import_lines,
            item_lines,
        })
    }

    fn item(&mut self) -> Result<Item, ParseError> {
        let public = self.eat(&Tok::Pub);
        if self.at(&Tok::Fn) || self.at(&Tok::Gen) || self.at(&Tok::Async) {
            Ok(Item::Function(self.function(public)?))
        } else if self.at(&Tok::Type) {
            self.type_def()
        } else if self.at(&Tok::Trait) {
            Ok(Item::Trait(self.trait_def()?))
        } else if self.at(&Tok::Impl) {
            Ok(Item::Impl(self.impl_def()?))
        } else if self.at(&Tok::Capability) {
            self.capability_def(false)
        } else if self.at_ident("grantable") {
            // `grantable capability X:` (RFC-0038) — a root-grantable sealed cap.
            self.advance();
            if !self.at(&Tok::Capability) {
                return Err(self.error("`grantable` may only precede a `capability` declaration"));
            }
            self.capability_def(true)
        } else if self.at(&Tok::Let) {
            // A module-level constant: `let NAME = EXPR`. Inlined at use sites.
            self.advance();
            let name = self.ident()?;
            self.expect(&Tok::Eq)?;
            let value = self.expr(0)?;
            Ok(Item::Const { name, value })
        } else if self.at(&Tok::Comptime) {
            // `comptime:` — compile-time item generation (additive, capability-
            // free; expanded by `crate::comptime` during linking).
            self.advance();
            let body = self.block()?;
            Ok(Item::Comptime(body))
        } else {
            Err(self.error(format!(
                "expected a top-level item (`fn`, `type`, `trait`, `impl`, or `let`), found `{}`",
                self.kind()
            )))
        }
    }

    /// `trait Name { fn m(self, ...) -> Ret  ... }` — method signatures only.
    /// `trait Ord: Eq + PartialOrd { ... }` declares supertraits. A marker trait
    /// with no methods needs no block: `trait Eq: PartialEq`.
    fn trait_def(&mut self) -> Result<TraitDef, ParseError> {
        self.expect(&Tok::Trait)?;
        let name = self.ident()?;
        // `trait FromIterator(e):` — type parameters.
        let mut typarams = Vec::new();
        if self.eat(&Tok::LParen) {
            while !self.at(&Tok::RParen) {
                typarams.push(self.ident()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
        }
        // `trait Ord: Eq + PartialOrd` — supertraits. The off-side layout pass
        // drops only the line-final `:` (the block opener), so this leading `:`
        // survives as a real token.
        let mut supertraits = Vec::new();
        if self.eat(&Tok::Colon) {
            loop {
                supertraits.push(self.ident()?);
                if !self.eat(&Tok::Plus) {
                    break;
                }
            }
        }
        // The method block is optional: a marker trait (`trait Eq: PartialEq`)
        // opens none.
        let mut methods = Vec::new();
        if self.eat(&Tok::LBrace) {
            while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
                methods.push(self.method_sig()?);
            }
            self.expect(&Tok::RBrace)?;
        }
        Ok(TraitDef { name, typarams, supertraits, methods })
    }

    /// A method signature inside a `trait`: `fn name(params) -> Ret`, with an
    /// optional default body `{ ... }` that impls inherit unless they override it.
    fn method_sig(&mut self) -> Result<MethodSig, ParseError> {
        self.expect(&Tok::Fn)?;
        let name = self.ident()?;
        self.expect(&Tok::LParen)?;
        let params = self.params()?;
        self.expect(&Tok::RParen)?;
        let ret = if self.eat(&Tok::RArrow) {
            Some(self.ty()?)
        } else {
            None
        };
        let default = if self.at(&Tok::LBrace) {
            Some(self.block()?)
        } else {
            None
        };
        Ok(MethodSig {
            name,
            params,
            ret,
            default,
        })
    }

    /// `impl Trait for Type { <fn ...> }` (trait impl), or the inherent form
    /// `impl Type { <fn ...> }` (no `for`) whose methods belong to no trait but
    /// still dispatch by receiver type.
    fn impl_def(&mut self) -> Result<ImplDef, ParseError> {
        self.expect(&Tok::Impl)?;
        let first = self.ident()?;
        // `impl FromIterator(a) for List(a):` — trait type-arguments, and a
        // possibly-generic target whose HEAD names the impl.
        let mut trait_args = Vec::new();
        if self.at(&Tok::LParen) {
            self.advance();
            while !self.at(&Tok::RParen) {
                trait_args.push(self.ty()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
        }
        let (trait_name, type_name, target_args) = if self.eat(&Tok::For) {
            // A generic target (`List(a)`, `(a, b)`) is registered + mangled by its
            // head, but its type arguments are KEPT: they type the method `self` (so
            // `self` is `List(a)`/`(a, b)`, not a bare head) and pair with the `where`
            // bounds — which is what lets a generic impl monomorphize per element.
            if self.at(&Tok::LParen) {
                // A tuple target `impl Trait for (a, b)`: its head is the synthetic
                // `Tuple{N}` (the same head a tuple value dispatches under).
                self.advance();
                let mut args = Vec::new();
                while !self.at(&Tok::RParen) {
                    args.push(self.ty()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RParen)?;
                (Some(first), format!("Tuple{}", args.len()), args)
            } else {
                let head = self.ident()?;
                let mut args = Vec::new();
                if self.at(&Tok::LParen) {
                    self.advance();
                    while !self.at(&Tok::RParen) {
                        args.push(self.ty()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::RParen)?;
                }
                (Some(first), head, args)
            }
        } else {
            // Inherent impl on a generic type: `impl Stack(a):`. The arguments
            // after the head are the type's OWN parameters; keeping them as the
            // target args types each method's `self` as `Stack(a)` (in
            // `method_fn`), so methods on a generic type monomorphize per element
            // exactly like the trait-impl form `impl Trait for Stack(a)`. An
            // inherent impl carries no trait, so these are the target's args, not
            // a trait's — move them across and leave `trait_args` empty.
            (None, first, std::mem::take(&mut trait_args))
        };
        // `impl Trait for T where a: Bound:` — a conditional impl.
        let bounds = self.where_clause()?;
        // The method block is optional: a marker-trait impl (`impl Eq for Int`)
        // provides no methods.
        let mut methods = Vec::new();
        if self.eat(&Tok::LBrace) {
            while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
                methods.push(self.function(false)?);
            }
            self.expect(&Tok::RBrace)?;
        }
        Ok(ImplDef {
            trait_name,
            trait_args,
            type_name,
            target_args,
            bounds,
            methods,
        })
    }

    fn type_def(&mut self) -> Result<Item, ParseError> {
        self.expect(&Tok::Type)?;
        let name = self.ident()?;
        // Optional explicit type parameters: `type Pair(a, b):`. These FIX the
        // parameter order (needed when a constructor omits one — inference can't
        // place the omitted param). When absent, the checker infers the params
        // from the variant field types.
        let mut params: Vec<String> = Vec::new();
        if self.eat(&Tok::LParen) {
            while !self.at(&Tok::RParen) {
                params.push(self.ident()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
        }
        // A type alias: `type Id = Int`. Expanded to its target before later stages.
        if self.eat(&Tok::Eq) {
            let ty = self.ty()?;
            return Ok(Item::TypeAlias { name, ty });
        }
        // `type Point packed:` (RFC-0027) — inline/unboxed layout. A contextual
        // modifier (like `derive`), so `packed` stays usable as an ordinary ident.
        let packed = self.at_ident("packed");
        if packed {
            self.advance();
        }
        // `derive(Show, Eq, Ord)` — compiler-generated impls (additive,
        // expanded before checking; rfcs/language-evolution.md Phase 4).
        let mut derives = Vec::new();
        if self.at_ident("derive") {
            self.advance();
            self.expect(&Tok::LParen)?;
            while !self.at(&Tok::RParen) {
                derives.push(self.ident()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
        }
        self.expect(&Tok::LBrace)?;
        let mut variants = Vec::new();
        let mut rec_names: Vec<String> = Vec::new();
        let mut rec_types: Vec<crate::ast::Type> = Vec::new();
        let mut is_record = false;
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            let ident = self.ident()?;
            if is_record || self.at(&Tok::Colon) {
                // Record field: `name: Type`. The whole type is one constructor.
                is_record = true;
                self.expect(&Tok::Colon)?;
                rec_names.push(ident);
                rec_types.push(self.ty()?);
            } else {
                // Sum-type variant: `Name` or `Name(Type, ...)`.
                let mut fields = Vec::new();
                if self.eat(&Tok::LParen) {
                    while !self.at(&Tok::RParen) {
                        fields.push(self.ty()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::RParen)?;
                }
                variants.push(Variant {
                    name: ident,
                    fields,
                    field_names: vec![],
                });
            }
            self.eat(&Tok::Comma); // optional separator
        }
        self.expect(&Tok::RBrace)?;
        if is_record {
            // A record becomes a single constructor named after the type, with
            // its field types positional and field names recorded alongside.
            Ok(Item::Type(TypeDef {
                name: name.clone(),
                params,
                variants: vec![Variant {
                    name,
                    fields: rec_types,
                    field_names: rec_names,
                }],
                derives,
                sealed: false,
                grantable: false,
                packed,
                partial_eq_derived: false,
            }))
        } else {
            Ok(Item::Type(TypeDef {
                name,
                params,
                variants,
                derives,
                sealed: false,
                grantable: false,
                packed,
                partial_eq_derived: false,
            }))
        }
    }

    /// `capability X from U` / `capability X from (A, B)` (RFC-0002) — a SEALED
    /// one-variant brand over the host capabilities it refines. Desugars to a
    /// single-constructor type `X(U)` (or `X(A, B)`) carrying `sealed: true`, so
    /// every later stage treats it like an ordinary brand while the link-time
    /// sealing check confines its construction/destructuring to this module.
    ///
    /// The RECORD form (RFC-0011 carried state) names its fields and may mix host
    /// capabilities with ordinary policy data:
    ///
    ///     capability Postgres:
    ///         net: Net[Connect, Tcp]
    ///         table: String
    ///
    /// It desugars to a sealed record `Postgres(net, table)`; its footprint is the
    /// UNION of its capability-typed fields (the `String` contributes nothing), so
    /// it still audits as `Net` — carried policy state with no authority hidden.
    fn capability_def(&mut self, grantable: bool) -> Result<Item, ParseError> {
        self.expect(&Tok::Capability)?;
        let name = self.ident()?;
        // Record form: `capability X:` with named fields (carried state).
        if self.at(&Tok::LBrace) {
            self.advance();
            let mut field_names = Vec::new();
            let mut fields = Vec::new();
            while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
                field_names.push(self.ident()?);
                self.expect(&Tok::Colon)?;
                fields.push(self.ty()?);
                self.eat(&Tok::Comma);
            }
            self.expect(&Tok::RBrace)?;
            if fields.is_empty() {
                return Err(self.error("a `capability` record must declare at least one field"));
            }
            return Ok(Item::Type(TypeDef {
                name: name.clone(),
                params: vec![],
                variants: vec![Variant { name, fields, field_names }],
                derives: vec![],
                sealed: true,
                grantable,
                packed: false,
                partial_eq_derived: false,
            }));
        }
        if !self.at_ident("from") {
            return Err(self.error("expected `from` (or a record body) after the capability name, e.g. `capability Redis from Net[Connect, Tcp]`"));
        }
        self.advance();
        // The underlying capabilities: a single type, or a parenthesized tuple.
        let mut fields = Vec::new();
        if self.at(&Tok::LParen) {
            self.advance();
            while !self.at(&Tok::RParen) {
                fields.push(self.ty()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
        } else {
            fields.push(self.ty()?);
        }
        if fields.is_empty() {
            return Err(self.error("`capability X from …` must refine at least one capability"));
        }
        Ok(Item::Type(TypeDef {
            name: name.clone(),
            params: vec![],
            variants: vec![Variant { name, fields, field_names: vec![] }],
            derives: vec![],
            sealed: true,
            grantable,
            packed: false,
            partial_eq_derived: false,
        }))
    }

    fn is_assignment(&self) -> bool {
        if !matches!(self.kind(), Tok::Ident(_)) {
            return false;
        }
        // Scan past a place chain — `name`, `name[idx]…`, `name.field…`, and any
        // mix (`g[i].f[j]`) — then require an assignment operator. This is what
        // makes `d[k] = v` and `u.field = v` parse as assignments (RFC-0022),
        // not bare expression statements.
        let mut i = self.pos + 1;
        loop {
            match self.toks.get(i).map(|t| &t.kind) {
                Some(Tok::LBracket) => {
                    let mut depth = 1;
                    i += 1;
                    while depth > 0 {
                        match self.toks.get(i).map(|t| &t.kind) {
                            Some(Tok::LBracket) => depth += 1,
                            Some(Tok::RBracket) => depth -= 1,
                            None => return false,
                            _ => {}
                        }
                        i += 1;
                    }
                }
                Some(Tok::Dot)
                    if matches!(self.toks.get(i + 1).map(|t| &t.kind), Some(Tok::Ident(_))) =>
                {
                    i += 2;
                }
                _ => break,
            }
        }
        matches!(
            self.toks.get(i).map(|t| &t.kind),
            Some(
                Tok::Eq
                    | Tok::PlusEq
                    | Tok::MinusEq
                    | Tok::StarEq
                    | Tok::SlashEq
                    | Tok::PercentEq
            )
        )
    }

    fn function(&mut self, public: bool) -> Result<Function, ParseError> {
        let is_async = self.eat(&Tok::Async);
        let is_gen = self.eat(&Tok::Gen);
        self.expect(&Tok::Fn)?;
        let name = self.ident()?;
        self.expect(&Tok::LParen)?;
        self.pending_impl_bounds.clear();
        let params = self.params()?;
        self.expect(&Tok::RParen)?;
        let ret = if self.eat(&Tok::RArrow) {
            Some(self.ty()?)
        } else {
            None
        };
        // `impl Trait` params contribute bounds just like a `where` clause; merge
        // them (a function may use both).
        let mut bounds = std::mem::take(&mut self.pending_impl_bounds);
        bounds.extend(self.where_clause()?);
        // `await` is only legal inside an `async fn`; the body parse consults this.
        let prev_async = std::mem::replace(&mut self.in_async, is_async);
        let body = self.block()?;
        self.in_async = prev_async;
        Ok(Function {
            public,
            name,
            params,
            ret,
            body,
            bounds,
            is_gen,
            is_async,
        })
    }

    /// An optional `where a: Trait, b: Trait2` clause after a function signature.
    fn where_clause(&mut self) -> Result<Vec<(String, String, Vec<Type>)>, ParseError> {
        let mut bounds = Vec::new();
        if self.eat(&Tok::Where) {
            loop {
                let var = self.ident()?;
                self.expect(&Tok::Colon)?;
                let trait_name = self.ident()?;
                // `where c: FromIterator(a)` — the trait's type arguments.
                let mut targs = Vec::new();
                if self.at(&Tok::LParen) {
                    self.advance();
                    while !self.at(&Tok::RParen) {
                        targs.push(self.ty()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::RParen)?;
                }
                bounds.push((var, trait_name, targs));
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }
        Ok(bounds)
    }

    fn params(&mut self) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();
        while !self.at(&Tok::RParen) {
            // `var` mutates in place and writes back; `own` consumes (takes
            // ownership).
            let convention = if self.eat(&Tok::Var) {
                Convention::Var
            } else if self.eat(&Tok::Own) {
                Convention::Own
            } else if self.eat(&Tok::Let) {
                // An explicit `let` opts into an immutable borrow (native passes it
                // `&T`, no clone). Bare params remain owned values.
                Convention::Borrow
            } else {
                Convention::Let
            };
            let name = self.ident()?;
            let ty = if self.eat(&Tok::Colon) {
                // `x: impl Trait` — desugar to a fresh per-param type variable plus
                // a trait bound, so it reuses the whole generics path. Each `impl`
                // param gets its own variable (distinct types are allowed).
                if self.at(&Tok::Impl) {
                    self.advance();
                    let trait_name = self.ident()?;
                    let var = format!("impltrait_{}", self.pending_impl_bounds.len());
                    self.pending_impl_bounds.push((var.clone(), trait_name, Vec::new()));
                    Some(Type::Named(var, Vec::new()))
                } else {
                    Some(self.ty()?)
                }
            } else {
                None
            };
            params.push(Param {
                name,
                ty,
                convention,
            });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        Ok(params)
    }

    fn ty(&mut self) -> Result<Type, ParseError> {
        // Ownership/immutability qualifiers (RFC-0025/0026): `frozen T`, `unique T`,
        // `local unique T`. Contextual — only a qualifier keyword FOLLOWED BY a type
        // is one; a bare `frozen` (nothing following) stays an ordinary type variable.
        if let Some(q) = self.eat_type_qual() {
            return Ok(Type::Qualified(q, Box::new(self.ty()?)));
        }
        if self.eat(&Tok::Fn) {
            // Function type: `fn(T1, T2) -> R`.
            self.expect(&Tok::LParen)?;
            let mut params = Vec::new();
            while !self.at(&Tok::RParen) {
                params.push(self.ty()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
            self.expect(&Tok::RArrow)?;
            let ret = self.ty()?;
            return Ok(Type::Fn(params, Box::new(ret)));
        }
        if self.eat(&Tok::LParen) {
            let mut types = Vec::new();
            while !self.at(&Tok::RParen) {
                types.push(self.ty()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
            return Ok(Type::Tuple(types));
        }
        let name = self.ident()?;
        if self.at(&Tok::Lt) {
            // `List<Int>` is the Rust/TS spelling; witchy writes type arguments in
            // parentheses. Catch the `<` here and suggest the right form, instead
            // of the opaque "expected `=`, found `<`" the caller would otherwise
            // report.
            return Err(self.error(format!(
                "witchy writes type arguments in parentheses, not angle brackets: \
                 use `{name}(…)` instead of `{name}<…>`"
            )));
        }
        let mut args = Vec::new();
        if self.eat(&Tok::LParen) {
            while !self.at(&Tok::RParen) {
                args.push(self.ty()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
        } else if self.eat(&Tok::LBracket) {
            // Capability rights: `Dir[Read]`, `Net[Connect, Tcp]` — the bracketed
            // names are carried as type arguments and read by the checker.
            while !self.at(&Tok::RBracket) {
                args.push(self.ty()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RBracket)?;
        }
        Ok(Type::Named(name, args))
    }

    /// Consume a leading ownership/immutability qualifier (`frozen` / `unique` /
    /// `local unique`) if one is present AND is followed by a type — so a bare
    /// lowercase type variable that happens to be spelled like a qualifier is
    /// unaffected. `None` when no qualifier applies.
    fn eat_type_qual(&mut self) -> Option<TypeQual> {
        let starts_type = |t: Option<&Tok>| {
            matches!(t, Some(Tok::Ident(_)) | Some(Tok::LParen) | Some(Tok::Fn))
        };
        // `local unique T`
        if self.at_ident("local")
            && matches!(self.toks.get(self.pos + 1).map(|t| &t.kind), Some(Tok::Ident(n)) if n == "unique")
            && starts_type(self.toks.get(self.pos + 2).map(|t| &t.kind))
        {
            self.advance();
            self.advance();
            return Some(TypeQual::LocalUnique);
        }
        for (kw, q) in [("frozen", TypeQual::Frozen), ("unique", TypeQual::Unique)] {
            if self.at_ident(kw) && starts_type(self.toks.get(self.pos + 1).map(|t| &t.kind)) {
                self.advance();
                return Some(q);
            }
        }
        None
    }

    // --- blocks & statements ---

    fn block(&mut self) -> Result<Block, ParseError> {
        self.expect(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        let mut lines = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            lines.push(self.cur().line);
            stmts.push(self.stmt()?);
        }
        self.expect(&Tok::RBrace)?;
        Ok(Block { stmts, lines, region: None })
    }

    fn stmt(&mut self) -> Result<Stmt, ParseError> {
        if self.eat(&Tok::Return) {
            // `return` alone (at a block's end) yields Nil; otherwise a value.
            let value = if self.at(&Tok::RBrace) || self.at(&Tok::If) {
                None
            } else {
                Some(self.expr(0)?)
            };
            // Postfix guard: `return X if cond` ≡ `if cond: return X`. Desugared
            // here to an `if` whose then-block is the lone return, tagged with the
            // `u32::MAX` synthetic-line marker so the formatter re-collapses exactly
            // this shape back to the postfix form (an explicitly written multi-line
            // `if cond: return X` keeps its real line numbers and stays as is).
            if self.eat(&Tok::If) {
                let cond = self.expr(0)?;
                let then_block = Block {
                    stmts: vec![Stmt::Return(value)],
                    lines: vec![u32::MAX],
                    region: None,
                };
                return Ok(Stmt::Expr(Expr::If {
                    cond: Box::new(cond),
                    then_block,
                    else_block: None,
                }));
            }
            return Ok(Stmt::Return(value));
        }
        if self.eat(&Tok::Break) {
            return Ok(Stmt::Break);
        }
        if self.eat(&Tok::Continue) {
            return Ok(Stmt::Continue);
        }
        if self.eat(&Tok::Yield) {
            // `yield e` — produce a value from a `gen fn`.
            return Ok(Stmt::Yield(self.expr(0)?));
        }
        if self.at(&Tok::Let) || self.at(&Tok::Var) {
            let mutable = self.advance() == Tok::Var;
            // `let mut x` is the Rust spelling; witchy has no `mut` modifier — a
            // mutable binding is `var x`. Catch `let mut <name>` (where `mut`
            // parses as a bare identifier) and point at the right keyword, instead
            // of the confusing "expected `=`, found `<name>`" that falls out of
            // treating `mut` as the variable name.
            if !mutable
                && self.at_ident("mut")
                && matches!(self.toks.get(self.pos + 1).map(|t| &t.kind), Some(Tok::Ident(_)))
            {
                return Err(self.error(
                    "witchy has no `let mut`; use `var` for a mutable binding (e.g. `var x = …`)",
                ));
            }
            if !mutable && self.eat(&Tok::Underscore) {
                // `let _ = e` — evaluate for effects, bind nothing. Same
                // meaning as the bare expression statement (the canonical
                // form, which `fmt` prints).
                self.expect(&Tok::Eq)?;
                return Ok(Stmt::Expr(self.expr(0)?));
            }
            if self.at(&Tok::LParen) {
                // Tuple destructure: `let (a, b) = e` (bindings are immutable).
                self.advance();
                let mut names = Vec::new();
                while !self.at(&Tok::RParen) {
                    // `_` discards that element: store it as the name "_", which
                    // is not a referenceable identifier, so the binding is dropped.
                    if self.eat(&Tok::Underscore) {
                        names.push("_".to_string());
                    } else {
                        names.push(self.ident()?);
                    }
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RParen)?;
                self.expect(&Tok::Eq)?;
                let value = self.expr(0)?;
                return Ok(Stmt::LetTuple { names, value });
            }
            let name = self.ident()?;
            // `let x: T = e` — an optional ascription, unified with the value.
            let ty = if self.eat(&Tok::Colon) {
                Some(self.ty()?)
            } else {
                None
            };
            self.expect(&Tok::Eq)?;
            let value = self.expr(0)?;
            Ok(Stmt::Let { name, ty, mutable, value })
        } else if self.is_assignment() {
            // The left side is a *place*: a variable, a subscript `x[i]`, or a
            // field `x.f` (RFC-0022). Parse it as an expression (it stops at the
            // assignment operator), then desugar to a plain `Stmt::Assign` of the
            // base variable — `x[i] = v` -> `x = x.set_at(i, v)`, `x.f = v` ->
            // `x = RecordUpdate{x, f: v}`.
            let place = self.expr(0)?;
            // `place op= e` desugars to `place = place op e`; plain `place = e` is
            // unchanged.
            let op = self.advance();
            let rhs = self.expr(0)?;
            let value = match compound_assign_op(&op) {
                Some(binop) => Expr::Binary {
                    op: binop,
                    lhs: Box::new(place.clone()),
                    rhs: Box::new(rhs),
                },
                None => rhs,
            };
            desugar_place_assign(place, value).map_err(|m| self.error(m))
        } else {
            Ok(Stmt::Expr(self.expr(0)?))
        }
    }

    // --- expressions (Pratt) ---

    fn expr(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.prefix()?;
        loop {
            // Inside a match-arm body, a `-` that starts a new line is the next
            // arm's negative pattern, not a continuation of this expression.
            if self.in_match_arm
                && *self.kind() == Tok::Minus
                && self.cur().line > self.toks[self.pos.saturating_sub(1)].line
            {
                break;
            }
            let Some((l_bp, r_bp)) = infix_bp(self.kind()) else {
                break;
            };
            if l_bp < min_bp {
                break;
            }
            let op_tok = self.advance();
            if op_tok == Tok::Pipe {
                let rhs = self.prefix()?;
                lhs = desugar_pipe(lhs, rhs, self)?;
            } else if op_tok == Tok::DotDot || op_tok == Tok::DotDotEq {
                let rhs = self.expr(r_bp)?;
                lhs = Expr::Range {
                    lo: Box::new(lhs),
                    hi: Box::new(rhs),
                    inclusive: op_tok == Tok::DotDotEq,
                };
            } else {
                let rhs = self.expr(r_bp)?;
                lhs = Expr::Binary {
                    op: bin_op(&op_tok),
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                };
            }
        }
        Ok(lhs)
    }

    fn prefix(&mut self) -> Result<Expr, ParseError> {
        if self.eat(&Tok::Minus) {
            let expr = self.prefix()?;
            return Ok(Expr::Unary {
                op: UnOp::Neg,
                expr: Box::new(expr),
            });
        }
        if self.eat(&Tok::Bang) {
            let expr = self.prefix()?;
            return Ok(Expr::Unary {
                op: UnOp::Not,
                expr: Box::new(expr),
            });
        }
        if self.eat(&Tok::Tilde) {
            let expr = self.prefix()?;
            return Ok(Expr::Unary {
                op: UnOp::BitNot,
                expr: Box::new(expr),
            });
        }
        // `move x` — a use-site ownership transfer; evaluates to `x` but tells the
        // compiler the caller is done with the binding (so it can be moved, not
        // cloned).
        if self.eat(&Tok::Move) {
            let expr = self.prefix()?;
            return Ok(Expr::Unary {
                op: UnOp::Move,
                expr: Box::new(expr),
            });
        }
        // `await` is POSTFIX (`e.await`), handled in `postfix()` — not a prefix
        // operator. A leading `await` is therefore a parse error (caught when
        // `atom()` meets the keyword), which points authors at the postfix form.
        self.postfix()
    }

    /// Postfix operators `?` (Result/Option propagation) and `.` (field access /
    /// module-qualified call) bind tighter than any prefix or infix operator, so
    /// `f(x)?` is `(f(x))?` and `p.x + 1` is `(p.x) + 1`.
    fn postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.atom()?;
        loop {
            if self.eat(&Tok::Question) {
                // Optional context message: `e? "msg"` (the message may interpolate,
                // which the lexer expands to a parenthesized concat — hence the
                // `LParen` case). A string/`(` immediately after `?` on the same line
                // is otherwise a syntax error everywhere, so consuming it here is a
                // conservative extension. Desugars to `(__try_ctx(e, msg))?`: the
                // `__try_ctx` intrinsic turns the operand — an `Option` OR a `Result`
                // — into a `Result(T, String)` carrying `msg` (prepended to a Result's
                // existing error), which `?` then unwraps. Generic over both, so the
                // message form works wherever bare `?` does.
                if self.on_same_line_as_prev()
                    && (matches!(self.kind(), Tok::Str(_)) || *self.kind() == Tok::LParen)
                {
                    let msg = self.atom()?;
                    let wrapped = Expr::Call { name: "__try_ctx".into(), args: vec![e, msg] };
                    e = Expr::Try(Box::new(wrapped));
                } else {
                    e = Expr::Try(Box::new(e));
                }
            } else if self.eat(&Tok::As) {
                // `e as T` — a capability narrowing ascription.
                let ty = self.ty()?;
                e = Expr::As { expr: Box::new(e), ty };
            } else if self.at(&Tok::LBracket) && self.on_same_line_as_prev() {
                // `xs[i]` — list subscript: sugar for `list.at(xs, i)`. Requiring the
                // `[` on the same line as the receiver avoids swallowing a list
                // literal that begins the next statement (no statement terminators).
                self.advance();
                let index = self.expr(0)?;
                self.expect(&Tok::RBracket)?;
                e = Expr::Index {
                    base: Box::new(e),
                    index: Box::new(index),
                };
            } else if self.at(&Tok::LParen) && self.on_same_line_as_prev() {
                // Apply the result of an expression: `f(x)(y)`, `make(3)(4)`.
                // Requiring the `(` on the same line as the callee avoids
                // swallowing a parenthesized expression that begins the next
                // statement (witchy has no statement terminators).
                let args = self.call_args()?;
                e = Expr::Apply {
                    func: Box::new(e),
                    args,
                };
            } else if self.eat(&Tok::Dot) {
                // Tuple element access: `pair.0` (the lexer guarantees digits
                // after a field-access dot arrive as a plain Int).
                if let Tok::Int(n) = self.kind().clone() {
                    self.advance();
                    e = Expr::Field {
                        base: Box::new(e),
                        field: n.to_string(),
                    };
                    continue;
                }
                // `e.await` — postfix suspension point (Rust-style). Same node as
                // any other `await`; only legal inside an `async fn`. Chains with
                // `?` and `.method(...)` because it lives in the postfix loop:
                // `f(x).await?` is `((f(x)).await)?`.
                if self.at(&Tok::Await) {
                    if !self.in_async {
                        return Err(self.error(
                            "`.await` is only allowed inside an `async fn`".to_string(),
                        ));
                    }
                    self.advance();
                    e = Expr::Unary { op: UnOp::Await, expr: Box::new(e) };
                    continue;
                }
                let member = self.ident()?;
                if self.at(&Tok::LParen) {
                    let args = self.call_args()?;
                    match e {
                        // `mod.func(args)` — a module-qualified call on a bare
                        // imported module name.
                        Expr::Var(name) if self.imports.contains(&name) => {
                            e = Expr::Call {
                                name: format!("{name}.{member}"),
                                args,
                            };
                        }
                        // `receiver.method(args)` — UFCS method call: sugar for
                        // `method(receiver, args)` (the method name resolves to a
                        // same-module or imported function in the linker). Kept as
                        // a node so the formatter can print it back.
                        receiver => {
                            e = Expr::MethodCall {
                                receiver: Box::new(receiver),
                                method: member,
                                args,
                            };
                        }
                    }
                } else {
                    e = Expr::Field {
                        base: Box::new(e),
                        field: member,
                    };
                }
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn atom(&mut self) -> Result<Expr, ParseError> {
        match self.kind().clone() {
            Tok::Int(n) => {
                self.advance();
                Ok(Expr::Int(n))
            }
            Tok::Duration(ms) => {
                self.advance();
                Ok(Expr::Duration(ms))
            }
            Tok::Float(x) => {
                self.advance();
                Ok(Expr::Float(x))
            }
            Tok::Str(s) => {
                self.advance();
                Ok(Expr::Str(s))
            }
            // `tag"a${x}b"` — a compile-time tagged literal (RFC-0006). The lexer
            // already split it into static parts and per-hole source; expansion
            // (`crate::tagged`) replaces it before type-checking.
            Tok::TagLit { tag, parts, holes, hole_spans } => {
                let line = self.cur().line;
                self.advance();
                Ok(Expr::TaggedLit { tag, parts, holes, hole_spans, line })
            }
            Tok::True => {
                self.advance();
                Ok(Expr::Bool(true))
            }
            Tok::False => {
                self.advance();
                Ok(Expr::Bool(false))
            }
            // `.{ field: expr, … }` — an anonymous struct.
            Tok::DotLBrace => {
                self.advance(); // `.{`
                self.anon_record()
            }
            Tok::LParen => {
                self.advance();
                let first = self.expr(0)?;
                if self.at(&Tok::Comma) {
                    let mut elems = vec![first];
                    while self.eat(&Tok::Comma) {
                        if self.at(&Tok::RParen) {
                            break; // trailing comma
                        }
                        elems.push(self.expr(0)?);
                    }
                    self.expect(&Tok::RParen)?;
                    Ok(Expr::Tuple(elems))
                } else {
                    self.expect(&Tok::RParen)?;
                    Ok(first) // parenthesized grouping
                }
            }
            Tok::LBracket => self.list(),
            Tok::LBrace => Ok(Expr::Block(self.block()?)),
            Tok::If => self.if_expr(),
            Tok::While => {
                self.advance();
                // `while let PAT = SCRUT:` loops while the scrutinee keeps
                // matching: it desugars to `while true` over a match whose
                // wildcard arm breaks out of the loop.
                if self.eat(&Tok::Let) {
                    let pattern = self.pattern()?;
                    self.expect(&Tok::Eq)?;
                    let scrutinee = self.expr(0)?;
                    let body = self.block()?;
                    return Ok(Expr::WhileLet {
                        pattern,
                        scrutinee: Box::new(scrutinee),
                        body,
                    });
                }
                let cond = self.expr(0)?;
                let body = self.block()?;
                Ok(Expr::While {
                    cond: Box::new(cond),
                    body,
                })
            }
            Tok::For => {
                self.advance();
                // `for await x in rx:` — a receive loop over a channel. Marked here
                // by wrapping the iterator in `chan.__recv_stream(..)`, which the
                // async lowering recognises and turns into a `chan.consume` loop
                // (so the body may itself `await`). The marker only survives in a
                // non-async fn, where it is rightly an "unknown function" error.
                let stream = self.eat(&Tok::Await);
                // `for var x in xs:` — write-back mutable iteration (RFC-0028): each
                // element is bound mutably and stored back, so you mutate elements
                // in place without index ceremony. Only the plain single-variable
                // form over a list variable; desugared below.
                let mutable = !stream && self.eat(&Tok::Var);
                let wrap = |iter: Expr| -> Expr {
                    if stream {
                        Expr::Call { name: "chan.__recv_stream".into(), args: vec![iter] }
                    } else {
                        iter
                    }
                };
                // `for (a, b) in pairs:` or `for a, b in pairs:` — a tuple pattern
                // destructures each element: sugar for a fresh element variable
                // plus a leading `let (a, b) = element` in the body. A single
                // unparenthesized name is an ordinary loop variable.
                let paren = self.eat(&Tok::LParen);
                let mut names = Vec::new();
                loop {
                    if self.eat(&Tok::Underscore) {
                        names.push("_".to_string());
                    } else {
                        names.push(self.ident()?);
                    }
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                if paren {
                    self.expect(&Tok::RParen)?;
                }
                self.expect(&Tok::In)?;
                let iter = self.expr(0)?;
                let mut body = self.block()?;
                if mutable {
                    if paren || names.len() != 1 {
                        return Err(self.error("`for var` takes a single loop variable, not a tuple pattern"));
                    }
                    let name = names.pop().unwrap();
                    let Expr::Var(list_var) = &iter else {
                        return Err(self.error("`for var x in <list>` requires a plain list variable to write each element back to"));
                    };
                    if for_var_body_escapes(&body) {
                        return Err(self.error(
                            "`for var` does not yet support break/continue/return/`?` in its body (RFC-0028 v1) — use an index loop for early exit",
                        ));
                    }
                    return Ok(desugar_for_var(name, list_var.clone(), body));
                }
                if !paren && names.len() == 1 {
                    return Ok(Expr::For {
                        var: names.pop().unwrap(),
                        iter: Box::new(wrap(iter)),
                        body,
                    });
                }
                let var = {
                    let v = format!("__fortuple{}", self.compr_counter);
                    self.compr_counter += 1;
                    v
                };
                body.stmts.insert(0, Stmt::LetTuple { names, value: Expr::Var(var.clone()) });
                if let Some(first) = body.lines.first().copied() {
                    body.lines.insert(0, first);
                } else {
                    body.lines.push(0);
                }
                Ok(Expr::For { var, iter: Box::new(wrap(iter)), body })
            }
            Tok::Fn => {
                // Anonymous function. Brace-free single-expression form
                // `fn(params): expr` (used inline inside call parens, where the
                // off-side layout is suppressed), or an indented/`{ }` block body.
                self.advance();
                self.expect(&Tok::LParen)?;
                let params = self.params()?;
                self.expect(&Tok::RParen)?;
                // Optional declared return type: `fn(x: Int) -> Bool: ...`. Makes
                // the closure a `?` boundary with that exact type.
                let ret = if self.eat(&Tok::RArrow) {
                    Some(self.ty()?)
                } else {
                    None
                };
                let body = self.colon_or_block()?;
                Ok(Expr::Lambda { params, body, ret })
            }
            Tok::Match => self.match_expr(),
            Tok::Region => self.region_block(),
            Tok::Ident(name) => {
                self.advance();
                self.name_application(name)
            }
            other => Err(self.error(format!("expected an expression, found `{other}`"))),
        }
    }

    /// Resolve a bare name into a variable, call, constructor, or a qualified
    /// call `module.func(args)`.
    /// Whether the `(` ahead opens named-field record args — the first inner
    /// token is `..` (spread) or an identifier immediately followed by `:`.
    fn peek_named_record(&self) -> bool {
        match self.toks.get(self.pos + 1).map(|t| &t.kind) {
            Some(Tok::DotDot) => true,
            Some(Tok::Ident(_)) => {
                matches!(self.toks.get(self.pos + 2).map(|t| &t.kind), Some(Tok::Colon))
            }
            _ => false,
        }
    }

    /// `.{ field: expr, … }` — an anonymous struct (the `.` is already consumed).
    /// It desugars to a value of a generic synthetic record `__anonN`, registered so
    /// `module()` emits its `derive(Reflect)` definition. The record is constructed
    /// by named field, so its field order is irrelevant; the synthetic type dedups by
    /// the sorted field-name set.
    fn anon_record(&mut self) -> Result<Expr, ParseError> {
        let mut fields = Vec::new();
        while !self.at(&Tok::RBrace) {
            let field = self.ident()?;
            self.expect(&Tok::Colon)?;
            fields.push((field, self.expr(0)?));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RBrace)?;
        let mut names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
        names.sort();
        let idx = self
            .anon_records
            .iter()
            .position(|s| *s == names)
            .unwrap_or_else(|| {
                self.anon_records.push(names);
                self.anon_records.len() - 1
            });
        Ok(Expr::Record { name: format!("__anon{idx}"), fields, spread: None })
    }

    /// `Name(field: value, ..., ..base?)` — named-field construction, optionally
    /// ending with a `..base` spread.
    fn record_literal(&mut self, name: String) -> Result<Expr, ParseError> {
        self.expect(&Tok::LParen)?;
        let mut fields = Vec::new();
        let mut spread = None;
        while !self.at(&Tok::RParen) {
            if self.eat(&Tok::DotDot) {
                spread = Some(Box::new(self.expr(0)?));
                break; // a spread is the last element
            }
            let field = self.ident()?;
            self.expect(&Tok::Colon)?;
            fields.push((field, self.expr(0)?));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        Ok(Expr::Record { name, fields, spread })
    }

    fn name_application(&mut self, name: String) -> Result<Expr, ParseError> {
        // Note: a trailing `.member` (module-qualified call or field access) is
        // handled by `postfix`, which wraps this.
        let is_ctor = name.chars().next().is_some_and(|c| c.is_uppercase());
        // A `(` that begins a NEW line is never call arguments: witchy has no
        // statement terminators, so a leading `(` opens the next statement / match
        // arm — a tuple pattern, a parenthesized expression, or an interpolated
        // string (which the lexer expands to a leading `(`). A genuine call keeps
        // its `(` on the same line as the callee, exactly as the `Apply`/`Index`
        // postfix rules require. Without this, `else: x` (or any bare-name tail
        // expression) followed by a line that starts with `"${...}"` mis-parses
        // `x` as the call `x(...)`.
        let paren_on_new_line = *self.kind() == Tok::LParen
            && self.cur().line > self.toks[self.pos.saturating_sub(1)].line;
        if self.at(&Tok::LParen) && !paren_on_new_line {
            // `Point(x: 1, y: 2)` / `Point(x: 5, ..p)` — named-field record
            // construction (only for constructors, i.e. uppercase names).
            if is_ctor && self.peek_named_record() {
                return self.record_literal(name);
            }
            let args = self.call_args()?;
            if is_ctor {
                Ok(Expr::Ctor { name, args })
            } else {
                Ok(Expr::Call { name, args })
            }
        } else if is_ctor {
            Ok(Expr::Ctor { name, args: vec![] })
        } else {
            Ok(Expr::Var(name))
        }
    }

    fn call_args(&mut self) -> Result<Vec<Expr>, ParseError> {
        self.expect(&Tok::LParen)?;
        let mut args = Vec::new();
        while !self.at(&Tok::RParen) {
            args.push(self.expr(0)?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        Ok(args)
    }

    fn list(&mut self) -> Result<Expr, ParseError> {
        self.expect(&Tok::LBracket)?;
        if self.at(&Tok::RBracket) {
            self.advance();
            return Ok(Expr::List(Vec::new()));
        }
        let first = self.expr(0)?;
        // `[elem for x in iter (if cond)?]` — a list comprehension.
        if self.at(&Tok::For) {
            return self.list_comprehension(first);
        }
        let mut items = vec![first];
        if self.eat(&Tok::Comma) {
            while !self.at(&Tok::RBracket) {
                items.push(self.expr(0)?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }
        self.expect(&Tok::RBracket)?;
        Ok(Expr::List(items))
    }

    /// Desugar a list comprehension with one or more generators and filters —
    /// `[elem for x in xs (if c)* (for y in ys)* ...]` — into a block that builds
    /// the list with nested loops/conditionals: `{ var acc = []; for x in xs {
    /// (if c) (for y in ys { ... acc = list.push(acc, elem) }) }; acc }`. The clauses
    /// nest in source order, so later generators see earlier loop variables.
    fn list_comprehension(&mut self, elem: Expr) -> Result<Expr, ParseError> {
        enum Clause {
            For(String, Expr),
            If(Expr),
        }
        let mut clauses = Vec::new();
        loop {
            if self.eat(&Tok::For) {
                let var = self.ident()?;
                self.expect(&Tok::In)?;
                clauses.push(Clause::For(var, self.expr(0)?));
            } else if self.eat(&Tok::If) {
                clauses.push(Clause::If(self.expr(0)?));
            } else {
                break;
            }
        }
        self.expect(&Tok::RBracket)?;

        let acc = format!("__compr{}", self.compr_counter);
        self.compr_counter += 1;
        // Innermost action: append `elem` to the accumulator.
        let mut inner = Stmt::Assign {
            name: acc.clone(),
            value: Expr::Call {
                name: "list.push".to_string(),
                args: vec![Expr::Var(acc.clone()), elem],
            },
        };
        // Wrap from the innermost clause outward.
        for clause in clauses.into_iter().rev() {
            let body = Block { stmts: vec![inner], lines: vec![0], region: None };
            inner = match clause {
                Clause::If(cond) => Stmt::Expr(Expr::If {
                    cond: Box::new(cond),
                    then_block: body,
                    else_block: None,
                }),
                Clause::For(var, iter) => Stmt::Expr(Expr::For {
                    var,
                    iter: Box::new(iter),
                    body,
                }),
            };
        }
        Ok(Expr::Block(Block {
            stmts: vec![
                Stmt::Let {
                    name: acc.clone(),
                    ty: None,
                    mutable: true,
                    value: Expr::List(Vec::new()),
                },
                inner,
                Stmt::Expr(Expr::Var(acc)),
            ],
            lines: vec![0, 0, 0],
            region: None,
        }))
    }

    /// `region:` / `region -> Type:` — a user-controlled allocation scope.
    /// The optional type ascribes the block's value (the copy-out shape).
    fn region_block(&mut self) -> Result<Expr, ParseError> {
        self.advance(); // `region`
        let ty = if self.eat(&Tok::RArrow) { Some(self.ty()?) } else { None };
        let mut block = self.block()?;
        block.region = Some(RegionAnn { ty });
        Ok(Expr::Block(block))
    }

    /// A block body that may be written brace-free as `: expr` (a single
    /// expression, used inline where the off-side layout is suppressed) or as a
    /// normal indented / `{ ... }` block.
    fn colon_or_block(&mut self) -> Result<Block, ParseError> {
        if self.at(&Tok::Colon) {
            let line = self.cur().line;
            self.advance();
            let e = self.expr(0)?;
            Ok(Block {
                stmts: vec![Stmt::Expr(e)],
                lines: vec![line],
                region: None,
            })
        } else {
            self.block()
        }
    }

    fn if_expr(&mut self) -> Result<Expr, ParseError> {
        self.expect(&Tok::If)?;
        // `if let PAT = SCRUT:` is sugar for a match: the pattern arm runs the
        // body, and a wildcard arm runs the `else` block (or nothing).
        if self.eat(&Tok::Let) {
            let pattern = self.pattern()?;
            self.expect(&Tok::Eq)?;
            let scrutinee = self.expr(0)?;
            let then_block = self.colon_or_block()?;
            let fallback = match self.else_block()? {
                Some(eb) => Expr::Block(eb),
                None => Expr::Block(Block { stmts: vec![], lines: vec![], region: None }),
            };
            return Ok(Expr::Match {
                scrutinee: Box::new(scrutinee),
                arms: vec![
                    MatchArm { pattern, guard: None, body: Expr::Block(then_block) },
                    MatchArm { pattern: Pattern::Wildcard, guard: None, body: fallback },
                ],
            });
        }
        let cond = self.expr(0)?;
        let then_block = self.colon_or_block()?;
        let else_block = self.else_block()?;
        Ok(Expr::If {
            cond: Box::new(cond),
            then_block,
            else_block,
        })
    }

    /// Parse an optional trailing `else:` / `else if …` clause as a block.
    fn else_block(&mut self) -> Result<Option<Block>, ParseError> {
        if self.eat(&Tok::Else) {
            if self.at(&Tok::If) {
                // `else if` chains nest as a block containing one if-expression.
                let line = self.cur().line;
                Ok(Some(Block {
                    stmts: vec![Stmt::Expr(self.if_expr()?)],
                    lines: vec![line],
                    region: None,
                }))
            } else {
                Ok(Some(self.colon_or_block()?))
            }
        } else {
            Ok(None)
        }
    }

    fn match_expr(&mut self) -> Result<Expr, ParseError> {
        self.expect(&Tok::Match)?;
        let scrutinee = self.expr(0)?;
        self.expect(&Tok::LBrace)?;
        let mut arms = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            // Or-patterns: `p1 | p2 | ... -> body` is sugar for one arm per
            // alternative, all sharing the guard and body. Alternatives that
            // bind variables work too, since each expanded arm binds them.
            let mut alternatives = vec![self.arm_pattern()?];
            while self.eat(&Tok::Bar) {
                alternatives.push(self.arm_pattern()?);
            }
            let explicit = if self.eat(&Tok::If) {
                Some(self.expr(0)?)
            } else {
                None
            };
            self.expect(&Tok::RArrow)?;
            let outer = self.in_match_arm;
            self.in_match_arm = true;
            // An inline arm body may be a single statement (`-> return e`,
            // `-> x = e`), not just an expression; it parses as a one-statement
            // block, the same shape the indented form produces.
            let body = if self.at(&Tok::Return)
                || self.at(&Tok::Break)
                || self.at(&Tok::Continue)
                || self.is_assignment()
            {
                let line = self.cur().line;
                let st = self.stmt()?;
                Expr::Block(Block {
                    stmts: vec![st],
                    lines: vec![line],
                    region: None,
                })
            } else {
                self.expr(0)?
            };
            self.in_match_arm = outer;
            let last = alternatives.len() - 1;
            for (i, (pattern, range_guard)) in alternatives.into_iter().enumerate() {
                // A range pattern contributes a bounds-check guard; combine it with
                // any explicit `if` guard. Clone the shared body for all but the last.
                let guard = match (range_guard, explicit.clone()) {
                    (Some(r), Some(e)) => Some(Expr::Binary {
                        op: BinOp::And,
                        lhs: Box::new(r),
                        rhs: Box::new(e),
                    }),
                    (Some(r), None) => Some(r),
                    (None, e) => e,
                };
                if i == last {
                    arms.push(MatchArm { pattern, guard, body });
                    break;
                }
                arms.push(MatchArm {
                    pattern,
                    guard,
                    body: body.clone(),
                });
            }
            self.eat(&Tok::Comma); // optional separator
        }
        self.expect(&Tok::RBrace)?;
        Ok(Expr::Match {
            scrutinee: Box::new(scrutinee),
            arms,
        })
    }

    /// A match-arm pattern, possibly an integer range. `lo..hi` (exclusive) and
    /// `lo..=hi` (inclusive) desugar to a fresh binding plus a bounds-check guard,
    /// so the existing guard machinery handles the test — no dedicated pattern.
    fn arm_pattern(&mut self) -> Result<(Pattern, Option<Expr>), ParseError> {
        let pat = self.pattern()?;
        if let Pattern::Int(lo) = pat {
            let inclusive = self.at(&Tok::DotDotEq);
            if inclusive || self.at(&Tok::DotDot) {
                self.advance();
                let hi = self.int_bound()?;
                let name = format!("_range{}", self.compr_counter);
                self.compr_counter += 1;
                let bind = || Box::new(Expr::Var(name.clone()));
                let guard = Expr::Binary {
                    op: BinOp::And,
                    lhs: Box::new(Expr::Binary {
                        op: BinOp::GtEq,
                        lhs: bind(),
                        rhs: Box::new(Expr::Int(lo)),
                    }),
                    rhs: Box::new(Expr::Binary {
                        op: if inclusive { BinOp::LtEq } else { BinOp::Lt },
                        lhs: bind(),
                        rhs: Box::new(Expr::Int(hi)),
                    }),
                };
                return Ok((Pattern::Var(name), Some(guard)));
            }
        }
        Ok((pat, None))
    }

    /// An integer bound in a range pattern, allowing a leading `-`.
    fn int_bound(&mut self) -> Result<i64, ParseError> {
        let neg = self.eat(&Tok::Minus);
        match self.kind().clone() {
            Tok::Int(n) => {
                self.advance();
                Ok(if neg { -n } else { n })
            }
            other => Err(self.error(format!(
                "expected an integer bound in a range pattern, found `{other}`"
            ))),
        }
    }

    fn pattern(&mut self) -> Result<Pattern, ParseError> {
        match self.kind().clone() {
            Tok::LParen => {
                self.advance();
                let mut pats = Vec::new();
                while !self.at(&Tok::RParen) {
                    pats.push(self.pattern()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RParen)?;
                Ok(Pattern::Tuple(pats))
            }
            Tok::LBracket => {
                self.advance();
                let mut elems = Vec::new();
                let mut rest = None;
                while !self.at(&Tok::RBracket) {
                    if self.eat(&Tok::DotDot) {
                        // `..` or `..name` — captures the remaining tail; must be last.
                        let name = match self.kind() {
                            Tok::Ident(n) => {
                                let n = n.clone();
                                self.advance();
                                Some(n)
                            }
                            _ => None,
                        };
                        rest = Some(name);
                        break;
                    }
                    elems.push(self.pattern()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RBracket)?;
                Ok(Pattern::List { elems, rest })
            }
            Tok::Underscore => {
                self.advance();
                Ok(Pattern::Wildcard)
            }
            Tok::Int(n) => {
                self.advance();
                Ok(Pattern::Int(n))
            }
            Tok::Minus => {
                // Negative integer literal pattern, e.g. `-1`.
                self.advance();
                match self.kind().clone() {
                    Tok::Int(n) => {
                        self.advance();
                        Ok(Pattern::Int(-n))
                    }
                    other => Err(self.error(format!(
                        "expected an integer after `-` in a pattern, found `{other}`"
                    ))),
                }
            }
            Tok::Str(s) => {
                self.advance();
                Ok(Pattern::Str(s))
            }
            Tok::True => {
                self.advance();
                Ok(Pattern::Bool(true))
            }
            Tok::False => {
                self.advance();
                Ok(Pattern::Bool(false))
            }
            Tok::Ident(name) => {
                self.advance();
                let is_ctor = name.chars().next().is_some_and(|c| c.is_uppercase());
                if is_ctor {
                    let mut args = Vec::new();
                    if self.eat(&Tok::LParen) {
                        while !self.at(&Tok::RParen) {
                            args.push(self.pattern()?);
                            if !self.eat(&Tok::Comma) {
                                break;
                            }
                        }
                        self.expect(&Tok::RParen)?;
                    }
                    Ok(Pattern::Ctor { name, args })
                } else {
                    Ok(Pattern::Var(name))
                }
            }
            other => Err(self.error(format!("expected a pattern, found `{other}`"))),
        }
    }
}

fn infix_bp(t: &Tok) -> Option<(u8, u8)> {
    use Tok::*;
    Some(match t {
        Pipe => (1, 2),
        // `a..b` (half-open) and `a..=b` (inclusive) ranges bind loosest after
        // pipe, so `1..n+1` is `1..(n+1)` and arbitrary Int expressions work.
        DotDot | DotDotEq => (2, 3),
        OrOr => (3, 4),
        AndAnd => (5, 6),
        EqEq | NotEq | Lt | LtEq | Gt | GtEq => (7, 8),
        // Bitwise ops bind tighter than comparison (so `a & b == c` is
        // `(a & b) == c`) and looser than arithmetic, ordered `|` < `^` < `&` <
        // shifts. `Bar` here is bitwise-or; in pattern position it's an
        // or-pattern separator, consumed by `match_expr` before expressions run.
        Bar => (9, 10),
        Caret => (11, 12),
        Amp => (13, 14),
        Shl | Shr => (15, 16),
        Plus | Minus => (17, 18),
        Star | Slash | Percent => (19, 20),
        _ => return None,
    })
}

fn bin_op(t: &Tok) -> BinOp {
    match t {
        Tok::Plus => BinOp::Add,
        Tok::Minus => BinOp::Sub,
        Tok::Star => BinOp::Mul,
        Tok::Slash => BinOp::Div,
        Tok::Percent => BinOp::Mod,
        Tok::EqEq => BinOp::Eq,
        Tok::NotEq => BinOp::NotEq,
        Tok::Lt => BinOp::Lt,
        Tok::LtEq => BinOp::LtEq,
        Tok::Gt => BinOp::Gt,
        Tok::GtEq => BinOp::GtEq,
        Tok::AndAnd => BinOp::And,
        Tok::OrOr => BinOp::Or,
        Tok::Bar => BinOp::BitOr,
        Tok::Caret => BinOp::BitXor,
        Tok::Amp => BinOp::BitAnd,
        Tok::Shl => BinOp::Shl,
        Tok::Shr => BinOp::Shr,
        other => unreachable!("not a binary operator: {other:?}"),
    }
}

/// Desugar an assignment whose left side is a *place* expression into a plain
/// `Stmt::Assign` of the base variable (RFC-0022). A subscript becomes a
/// `set_at` method call (UFCS-resolved to `list`/`dict`), a field becomes a
/// `RecordUpdate`, and nested places (`g[i][j] = v`) recurse outward — every
/// step reassigns a value, so the uniqueness pass keeps it in place. The base
/// must bottom out at a variable.
pub(crate) fn desugar_place_assign(place: Expr, value: Expr) -> Result<Stmt, String> {
    match place {
        Expr::Var(name) => Ok(Stmt::Assign { name, value }),
        Expr::Index { base, index } => {
            let new_base = Expr::MethodCall {
                receiver: base.clone(),
                method: "set_at".to_string(),
                args: vec![*index, value],
            };
            desugar_place_assign(*base, new_base)
        }
        Expr::Field { base, field } => {
            let new_base = Expr::RecordUpdate {
                base: base.clone(),
                fields: vec![(field, value)],
            };
            desugar_place_assign(*base, new_base)
        }
        _ => Err(
            "the left side of `=` must be a variable, an index `x[i]`, or a field `x.f`"
                .to_string(),
        ),
    }
}

/// Desugar `for var x in xs:` (RFC-0028) into an indexed loop that writes each
/// element back, so a mutation of `x` lands in `xs` — using only existing nodes
/// (range-for + `xs[i] = …` place-assignment), so both backends lower it
/// identically and the uniqueness pass keeps the write in place:
///
/// ```text
/// for __fvN in 0..xs.length():
///     var x = xs[__fvN]
///     <body>
///     xs[__fvN] = x
/// ```
///
/// v1 requires `xs` be a plain list variable and rejects loop-belonging
/// `break`/`continue`/`return`/`?` in the body (checked by the caller via
/// [`for_var_body_escapes`]) — straight-line element mutation, the common case;
/// loss-free write-back across early exit is a later refinement.
fn desugar_for_var(name: String, list_var: String, body: Block) -> Expr {
    thread_local! {
        static FORVAR_COUNTER: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let n = FORVAR_COUNTER.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    let idx = format!("__fv{n}");
    let elem = || Expr::Index {
        base: Box::new(Expr::Var(list_var.clone())),
        index: Box::new(Expr::Var(idx.clone())),
    };
    let bind = Stmt::Let { name: name.clone(), ty: None, mutable: true, value: elem() };
    // `xs[idx] = name` — desugar_place_assign turns it into `xs = xs.set_at(idx, name)`.
    let writeback = desugar_place_assign(elem(), Expr::Var(name))
        .expect("an index place always desugars");
    let mut stmts = Vec::with_capacity(body.stmts.len() + 2);
    stmts.push(bind);
    stmts.extend(body.stmts);
    stmts.push(writeback);
    let lines = vec![0u32; stmts.len()];
    let inner = Block { stmts, lines, region: None };
    Expr::For {
        var: idx,
        iter: Box::new(Expr::Range {
            lo: Box::new(Expr::Int(0)),
            hi: Box::new(Expr::MethodCall {
                receiver: Box::new(Expr::Var(list_var)),
                method: "length".to_string(),
                args: vec![],
            }),
            inclusive: false,
        }),
        body: inner,
    }
}

/// Does `body` contain a control-flow exit that belongs to the enclosing `for
/// var` (rather than to a nested loop or lambda)? Such an exit would skip the
/// element write-back, so RFC-0028 v1 rejects it up front instead of silently
/// losing the write. `break`/`continue` belong to the nearest loop; `return` and
/// `?` exit the function (so they belong unless inside a lambda).
fn for_var_body_escapes(b: &Block) -> bool {
    b.stmts.iter().any(|s| for_var_stmt_escapes(s, false, false))
}

fn for_var_stmt_escapes(s: &Stmt, in_loop: bool, in_lambda: bool) -> bool {
    match s {
        Stmt::Break | Stmt::Continue => !in_loop,
        Stmt::Return(e) => {
            !in_lambda || e.as_ref().is_some_and(|x| for_var_expr_escapes(x, in_loop, in_lambda))
        }
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::LetTuple { value, .. }
        | Stmt::Yield(value)
        | Stmt::Expr(value) => for_var_expr_escapes(value, in_loop, in_lambda),
    }
}

fn for_var_expr_escapes(e: &Expr, in_loop: bool, in_lambda: bool) -> bool {
    let blk = |b: &Block, il: bool, ila: bool| {
        b.stmts.iter().any(|s| for_var_stmt_escapes(s, il, ila))
    };
    match e {
        // `?` propagates from the enclosing function (or closure) — a function-level
        // early exit, exactly like `return`.
        Expr::Try(inner) => !in_lambda || for_var_expr_escapes(inner, in_loop, in_lambda),
        // Nested loops capture their own break/continue, but a `return`/`?` inside
        // one still escapes the `for var`, so descend with in_loop = true.
        Expr::For { iter, body, .. } => {
            for_var_expr_escapes(iter, in_loop, in_lambda) || blk(body, true, in_lambda)
        }
        Expr::While { cond, body } => {
            for_var_expr_escapes(cond, in_loop, in_lambda) || blk(body, true, in_lambda)
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            for_var_expr_escapes(scrutinee, in_loop, in_lambda) || blk(body, true, in_lambda)
        }
        // A lambda owns its `return`/`?`; break/continue cannot cross into it.
        Expr::Lambda { body, .. } => blk(body, true, true),
        Expr::If { cond, then_block, else_block } => {
            for_var_expr_escapes(cond, in_loop, in_lambda)
                || blk(then_block, in_loop, in_lambda)
                || else_block.as_ref().is_some_and(|b| blk(b, in_loop, in_lambda))
        }
        Expr::Match { scrutinee, arms } => {
            for_var_expr_escapes(scrutinee, in_loop, in_lambda)
                || arms.iter().any(|a| {
                    a.guard.as_ref().is_some_and(|g| for_var_expr_escapes(g, in_loop, in_lambda))
                        || for_var_expr_escapes(&a.body, in_loop, in_lambda)
                })
        }
        Expr::Block(b) => blk(b, in_loop, in_lambda),
        Expr::Call { args, .. }
        | Expr::Ctor { args, .. }
        | Expr::List(args)
        | Expr::Tuple(args) => args.iter().any(|a| for_var_expr_escapes(a, in_loop, in_lambda)),
        Expr::Apply { func, args } => {
            for_var_expr_escapes(func, in_loop, in_lambda)
                || args.iter().any(|a| for_var_expr_escapes(a, in_loop, in_lambda))
        }
        Expr::Binary { lhs, rhs, .. } => {
            for_var_expr_escapes(lhs, in_loop, in_lambda)
                || for_var_expr_escapes(rhs, in_loop, in_lambda)
        }
        Expr::Range { lo, hi, .. } => {
            for_var_expr_escapes(lo, in_loop, in_lambda)
                || for_var_expr_escapes(hi, in_loop, in_lambda)
        }
        Expr::Index { base, index } => {
            for_var_expr_escapes(base, in_loop, in_lambda)
                || for_var_expr_escapes(index, in_loop, in_lambda)
        }
        Expr::MethodCall { receiver, args, .. } => {
            for_var_expr_escapes(receiver, in_loop, in_lambda)
                || args.iter().any(|a| for_var_expr_escapes(a, in_loop, in_lambda))
        }
        Expr::Unary { expr, .. } | Expr::As { expr, .. } | Expr::Field { base: expr, .. } => {
            for_var_expr_escapes(expr, in_loop, in_lambda)
        }
        Expr::RecordUpdate { base, fields } => {
            for_var_expr_escapes(base, in_loop, in_lambda)
                || fields.iter().any(|(_, v)| for_var_expr_escapes(v, in_loop, in_lambda))
        }
        _ => false,
    }
}

/// The arithmetic op a compound-assignment token applies (`+=` -> Add, ...);
/// `None` for a plain `=`.
fn compound_assign_op(t: &Tok) -> Option<BinOp> {
    match t {
        Tok::PlusEq => Some(BinOp::Add),
        Tok::MinusEq => Some(BinOp::Sub),
        Tok::StarEq => Some(BinOp::Mul),
        Tok::SlashEq => Some(BinOp::Div),
        Tok::PercentEq => Some(BinOp::Mod),
        _ => None,
    }
}

/// Desugar `lo..hi` (half-open) or `lo..=hi` (inclusive) integer ranges into a
/// block that builds the list: `{ var acc = []; var i = lo; let end = hi;
/// while i < end (or i <= end) { acc = list.push(acc, i); i = i + 1 }; acc }`. `hi`
/// is bound once so it isn't re-evaluated each iteration. Self-contained.
///
/// A free function (not a parser method) because the parser keeps ranges as
/// `Expr::Range` for the formatter; every other consumer (typeck, interpreter,
/// codegen) calls this to lower them. The synthetic-name counter is a
/// thread-local so repeated lowerings never collide.
pub fn desugar_range(lo: Expr, hi: Expr, inclusive: bool) -> Expr {
    thread_local! {
        static RANGE_COUNTER: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let n = RANGE_COUNTER.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    let acc = format!("__range{n}");
    let idx = format!("__ri{n}");
    let end = format!("__rend{n}");
    let lt = Expr::Binary {
        op: if inclusive { BinOp::LtEq } else { BinOp::Lt },
        lhs: Box::new(Expr::Var(idx.clone())),
        rhs: Box::new(Expr::Var(end.clone())),
    };
    let body = Block {
        stmts: vec![
            Stmt::Assign {
                name: acc.clone(),
                value: Expr::Call {
                    name: "list.push".to_string(),
                    args: vec![Expr::Var(acc.clone()), Expr::Var(idx.clone())],
                },
            },
            Stmt::Assign {
                name: idx.clone(),
                value: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Var(idx.clone())),
                    rhs: Box::new(Expr::Int(1)),
                },
            },
        ],
        lines: vec![0, 0],
        region: None,
    };
    Expr::Block(Block {
        stmts: vec![
            Stmt::Let { name: acc.clone(), ty: None, mutable: true, value: Expr::List(Vec::new()) },
            Stmt::Let { name: idx.clone(), ty: None, mutable: true, value: lo },
            Stmt::Let { name: end, ty: None, mutable: false, value: hi },
            Stmt::Expr(Expr::While { cond: Box::new(lt), body }),
            Stmt::Expr(Expr::Var(acc)),
        ],
        lines: vec![0, 0, 0, 0, 0],
        region: None,
    })
}

/// Lower `base[index]` to the call `list.at(base, index)`. A free function for the
/// same reason as [`desugar_range`]: the parser keeps subscripts as
/// `Expr::Index` for the formatter, and every other consumer lowers them here.
pub fn desugar_index(base: Expr, index: Expr) -> Expr {
    Expr::Call {
        name: "list.at".into(),
        args: vec![base, index],
    }
}

/// Lower `receiver.method(args)` to the call `method(receiver, args)` — exactly
/// what the parser used to build inline. The linker then resolves `method` by
/// the receiver's type just as for any call.
pub fn desugar_method(receiver: Expr, method: String, args: Vec<Expr>) -> Expr {
    let mut all = Vec::with_capacity(args.len() + 1);
    all.push(receiver);
    all.extend(args);
    Expr::Call { name: method, args: all }
}

/// Lower `while let PAT = SCRUT: body` to `while true` over a match whose
/// wildcard arm breaks the loop. A free function for the same reason as
/// [`desugar_range`]: the parser keeps `Expr::WhileLet` for the formatter, and
/// every other consumer lowers it here.
pub fn desugar_while_let(pattern: Pattern, scrutinee: Expr, body: Block) -> Expr {
    let dispatch = Expr::Match {
        scrutinee: Box::new(scrutinee),
        arms: vec![
            MatchArm { pattern, guard: None, body: Expr::Block(body) },
            MatchArm {
                pattern: Pattern::Wildcard,
                guard: None,
                body: Expr::Block(Block { stmts: vec![Stmt::Break], lines: vec![0], region: None }),
            },
        ],
    };
    Expr::While {
        cond: Box::new(Expr::Bool(true)),
        body: Block { stmts: vec![Stmt::Expr(dispatch)], lines: vec![0], region: None },
    }
}

/// Replace every sugar node the parser preserves for the formatter — `Expr::Range`
/// and `Expr::Index` — with its lowering. Codegen runs this once up front so its
/// multiple passes (local collection, then emission) agree on ranges' synthetic
/// loop-variable names and see subscripts as plain `at` calls; the formatter,
/// which never lowers, keeps the nodes so it can print `lo..hi` and `base[i]`.
pub fn lower_sugar_module(m: &mut Module) {
    for item in &mut m.items {
        match item {
            Item::Function(f) => lower_sugar_block(&mut f.body),
            Item::Impl(im) => {
                for meth in &mut im.methods {
                    lower_sugar_block(&mut meth.body);
                }
            }
            Item::Const { value, .. } => lower_sugar_expr(value),
            Item::Type(_) | Item::Trait(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
}

fn lower_sugar_block(b: &mut Block) {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::LetTuple { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Expr(value)
            | Stmt::Yield(value)
            | Stmt::Return(Some(value)) => lower_sugar_expr(value),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn lower_sugar_expr(e: &mut Expr) {
    match e {
        Expr::Range { lo, hi, inclusive } => {
            lower_sugar_expr(lo);
            lower_sugar_expr(hi);
            *e = desugar_range((**lo).clone(), (**hi).clone(), *inclusive);
        }
        Expr::Index { base, index } => {
            lower_sugar_expr(base);
            lower_sugar_expr(index);
            *e = desugar_index((**base).clone(), (**index).clone());
        }
        Expr::WhileLet { pattern, scrutinee, body } => {
            lower_sugar_expr(scrutinee);
            lower_sugar_block(body);
            *e = desugar_while_let(pattern.clone(), (**scrutinee).clone(), body.clone());
        }
        Expr::MethodCall { receiver, method, args } => {
            lower_sugar_expr(receiver);
            for a in args.iter_mut() {
                lower_sugar_expr(a);
            }
            *e = desugar_method((**receiver).clone(), method.clone(), std::mem::take(args));
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::Var(_) | Expr::TaggedLit { .. } => {}
        Expr::List(xs)
        | Expr::Tuple(xs)
        | Expr::Call { args: xs, .. }
        | Expr::Ctor { args: xs, .. } => {
            for x in xs {
                lower_sugar_expr(x);
            }
        }
        Expr::Apply { func, args } => {
            lower_sugar_expr(func);
            for a in args {
                lower_sugar_expr(a);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::Field { base: expr, .. } => lower_sugar_expr(expr),
        Expr::RecordUpdate { base, fields } => {
            lower_sugar_expr(base);
            for (_, v) in fields {
                lower_sugar_expr(v);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                lower_sugar_expr(v);
            }
            if let Some(s) = spread {
                lower_sugar_expr(s);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            lower_sugar_expr(lhs);
            lower_sugar_expr(rhs);
        }
        Expr::If { cond, then_block, else_block } => {
            lower_sugar_expr(cond);
            lower_sugar_block(then_block);
            if let Some(b) = else_block {
                lower_sugar_block(b);
            }
        }
        Expr::While { cond, body } => {
            lower_sugar_expr(cond);
            lower_sugar_block(body);
        }
        Expr::For { iter, body, .. } => {
            // A range iterator stays a `Range`: the backends iterate it by
            // counting (no list materialized), so lower the bounds in place but
            // keep the node. Any other iterator (a real list) lowers normally.
            if let Expr::Range { lo, hi, .. } = iter.as_mut() {
                lower_sugar_expr(lo);
                lower_sugar_expr(hi);
            } else {
                lower_sugar_expr(iter);
            }
            lower_sugar_block(body);
        }
        Expr::Match { scrutinee, arms } => {
            lower_sugar_expr(scrutinee);
            for arm in arms.iter_mut() {
                if let Some(g) = &mut arm.guard {
                    lower_sugar_expr(g);
                }
                lower_sugar_expr(&mut arm.body);
            }
        }
        Expr::Lambda { body, .. } => lower_sugar_block(body),
        Expr::Block(b) => lower_sugar_block(b),
    }
}

/// `lhs |> rhs` threads `lhs` in as the first argument of `rhs`.
fn desugar_pipe(lhs: Expr, rhs: Expr, p: &Parser) -> Result<Expr, ParseError> {
    match rhs {
        Expr::Call { name, mut args } => {
            args.insert(0, lhs);
            Ok(Expr::Call { name, args })
        }
        Expr::Var(name) => Ok(Expr::Call {
            name,
            args: vec![lhs],
        }),
        Expr::Ctor { name, mut args } => {
            args.insert(0, lhs);
            Ok(Expr::Ctor { name, args })
        }
        _ => Err(p.error("right-hand side of `|>` must be a function or constructor")),
    }
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
