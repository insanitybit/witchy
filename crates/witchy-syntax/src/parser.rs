//! Recursive-descent parser with a Pratt expression core.

use std::fmt;
// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::HashSet;

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

fn anon_record_type_name(fields: &[String]) -> String {
    let mut suffix = format!("{:010}", fields.len());
    for field in fields {
        suffix.push_str(&format!("{:010}", field.len()));
        for byte in field.as_bytes() {
            suffix.push_str(&format!("{byte:03}"));
        }
    }
    format!("__anon{suffix}")
}

fn anon_union_type_name(variants: &[(String, usize)]) -> String {
    let mut suffix = format!("{:010}", variants.len());
    for (tag, arity) in variants {
        suffix.push_str(&format!("{:010}", tag.len()));
        for byte in tag.as_bytes() {
            suffix.push_str(&format!("{byte:03}"));
        }
        suffix.push_str(&format!("{arity:010}"));
    }
    format!("__union{suffix}")
}

fn reserved_source_identifier(name: &str) -> bool {
    name.contains("__")
}

const QUOTE_EXPR_HOLE_INTRINSIC: &str = "@quote_expr_hole";
pub(crate) const QUOTE_EXPR_HOLE_PREFIX: &str = "__witchy_quote_expr_hole_";
pub(crate) const QUOTE_TYPE_HOLE_PREFIX: &str = "__witchy_quote_type_hole_";
pub(crate) const QUOTE_PATTERN_HOLE_PREFIX: &str = "__witchy_quote_pattern_hole_";

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
    // `derive(Reflect)`, prepended to the module. The synthetic type name is
    // keyed by the sorted field-name set, so distinct shapes from different
    // modules cannot collide after linking. Generic-record derive makes `.{…}`
    // ordinary reflectable data with no special builtin.
    if !parser.anon_records.is_empty() {
        let mut defs = String::new();
        for fields in &parser.anon_records {
            let name = anon_record_type_name(fields);
            let params: Vec<String> = (0..fields.len()).map(|i| format!("t{i}")).collect();
            defs.push_str(&format!("type {name}({}) derive(Reflect):\n", params.join(", ")));
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
    imports: HashSet<String>,
    /// `impl Trait` parameter bounds collected while parsing one function's
    /// params — `fn f(x: impl Show)` desugars to a fresh type var plus a
    /// `where`-style bound, reusing the whole trait/monomorphization path.
    pending_impl_bounds: Vec<(String, String, Vec<Type>)>,
    /// True while parsing the body of an `async fn`; gates the `.await` postfix.
    in_async: bool,
    in_gen: bool,
    /// Distinct field-name sets (sorted) of the anonymous structs `.{…}` seen, in
    /// first-seen order. Each becomes an exactly shape-keyed generic synthetic
    /// record `__anon...` prepended to the module, so `.{a: x}` is ordinary
    /// reflectable data — `json.stringify(.{…})`, `debug(.{…})` — with no builtins.
    anon_records: Vec<Vec<String>>,
    /// True once a parser-backed `quote ...:` form lowers to a `std/meta`
    /// constructor, so the parser also makes the implied `meta` module available
    /// to the linker.
    needs_meta_import: bool,
    /// Hole-free item quotations stay as parsed AST. The expression receives
    /// only an opaque handle through an unspellable compiler intrinsic.
    compiler_item_syntax: Vec<CompilerItemSyntax>,
    /// Hole-free expression quotations retain their parsed AST behind the same
    /// kind of compiler-only handle.
    compiler_expr_syntax: Vec<CompilerExprSyntax>,
    /// Hole-free type quotations retain their parsed AST behind a compiler-only
    /// handle. Source-backed builders remain the compatibility representation.
    compiler_type_syntax: Vec<CompilerTypeSyntax>,
    /// Hole-free pattern quotations retain their parsed AST behind a
    /// compiler-only handle.
    compiler_pattern_syntax: Vec<CompilerPatternSyntax>,
    /// Hole-free statement and block quotations retain parsed body AST behind
    /// compiler-only handles.
    compiler_stmt_syntax: Vec<CompilerStmtSyntax>,
    compiler_block_syntax: Vec<CompilerBlockSyntax>,
    /// Positive while parsing the literal body of `quote expr:`. `${...}` holes
    /// are syntax splices there; everywhere else they are rejected at parse time.
    quote_expr_hole_depth: u32,
    /// Positive while parsing the literal body of `quote type:`.
    quote_type_hole_depth: u32,
    quote_type_holes: Vec<Expr>,
    quote_type_hole_bases: Vec<usize>,
    /// Positive while parsing the literal body of `quote pattern:`.
    quote_pattern_hole_depth: u32,
    quote_pattern_holes: Vec<Expr>,
    quote_pattern_hole_bases: Vec<usize>,
    /// Current recursion depth of the mutually-recursive descent (expressions,
    /// types, patterns). Guarded against `MAX_PARSE_DEPTH` so deeply-nested
    /// untrusted source (e.g. `(((((…)))))`) returns a `ParseError` instead of
    /// overflowing the native stack — which is uncatchable (`SIGABRT`) when the
    /// parser runs inside a wasmtime host function (`compiler.footprint`/`doc`/
    /// `diff` on the supply-chain gate). Mirrors the interpreter's `depth_limit`.
    depth: u32,
}

/// Maximum nesting depth of the recursive-descent parser. Exceeding it is a clean
/// `ParseError`, never a native-stack overflow (`SIGABRT`) — which is uncatchable
/// when the parser runs inside a wasmtime host function (`compiler.footprint`/
/// `doc`/`diff` on the supply-chain gate; `func_wrap`, so `StoreLimits` doesn't
/// bound it and `catch_unwind` can't catch an abort).
///
/// Calibrated by measurement, not by mirroring the interpreter's much larger
/// `depth_limit` (25 000) — the interpreter runs on a dedicated 4 GiB stack,
/// whereas the parser runs on whatever thread calls it (the primary ~8 MiB thread,
/// but also a 2 MiB `serve`-worker thread). The parser's per-level frame is large
/// (a release build overflows a 1 MiB stack near depth ~240, a 2 MiB stack near
/// ~470). 128 leaves comfortable margin on the smallest production stack while
/// sitting far above any hand-written program's nesting (no real source nests
/// expressions/types/patterns anywhere near this deep).
const MAX_PARSE_DEPTH: u32 = 128;

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
            imports: crate::linker::PRELUDE_MODULES
                .iter()
                .copied()
                .chain(["task", "chan"])
                .map(String::from)
                .collect(),
            pending_impl_bounds: Vec::new(),
            in_async: false,
            in_gen: false,
            anon_records: Vec::new(),
            needs_meta_import: false,
            compiler_item_syntax: Vec::new(),
            compiler_expr_syntax: Vec::new(),
            compiler_type_syntax: Vec::new(),
            compiler_pattern_syntax: Vec::new(),
            compiler_stmt_syntax: Vec::new(),
            compiler_block_syntax: Vec::new(),
            quote_expr_hole_depth: 0,
            quote_type_hole_depth: 0,
            quote_type_holes: Vec::new(),
            quote_type_hole_bases: Vec::new(),
            quote_pattern_hole_depth: 0,
            quote_pattern_holes: Vec::new(),
            quote_pattern_hole_bases: Vec::new(),
            depth: 0,
        }
    }

    /// Error (never overflow the native stack) if the recursive descent has nested
    /// past `MAX_PARSE_DEPTH`. Call at the entry of each recursion step *after*
    /// incrementing `self.depth`; the caller decrements on return. Any parse error
    /// aborts the whole parse (there is no error recovery), so imprecise depth
    /// accounting on an error path is harmless — only the success path must balance.
    fn check_depth(&self) -> Result<(), ParseError> {
        if self.depth >= MAX_PARSE_DEPTH {
            return Err(self.error("input nests too deeply"));
        }
        Ok(())
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
        } else if matches!(k, Tok::LBrace) {
            Err(self.error(format!(
                "expected an indented block; add `:` after the header (found `{}`)",
                self.kind()
            )))
        } else {
            Err(self.error(format!("expected `{k}`, found `{}`", self.kind())))
        }
    }

    fn at_call_close(&self) -> bool {
        self.at(&Tok::RParen) || self.at(&Tok::InterpRBrace)
    }

    fn expect_call_close(&mut self) -> Result<(), ParseError> {
        if self.at_call_close() {
            self.advance();
            Ok(())
        } else {
            Err(self.error(format!("expected `)`, found `{}`", self.kind())))
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

    /// Consume a lifetime name token `'a` (RFC-0083), returning the bare name.
    fn lifetime_name(&mut self) -> Result<String, ParseError> {
        match self.kind().clone() {
            Tok::Lifetime(name) => {
                self.advance();
                Ok(name)
            }
            other => Err(self.error(format!("expected a lifetime like `'a`, found `{other}`"))),
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
        // `from X import Y, Z` (RFC-0042) additionally binds the listed TYPE or
        // function names unqualified; it implies `import X`.
        let mut imports = Vec::new();
        let mut import_lines = Vec::new();
        let mut from_imports: Vec<(String, Vec<String>)> = Vec::new();
        loop {
            if self.at(&Tok::Import) {
                import_lines.push(self.cur().line);
                self.advance();
                let name = self.ident()?;
                self.imports.insert(name.clone());
                imports.push(name);
            } else if self.at_ident("from") {
                // `from X import Y, Z` — deny-by-omission: no `from X import *`
                // (an unbounded import would let a dependency inject names) and no
                // `from X import Y as Z` (aliasing is out of scope per RFC-0042).
                let line = self.cur().line;
                self.advance();
                let module = self.ident()?;
                self.expect(&Tok::Import)?;
                let mut names = Vec::new();
                loop {
                    if self.at(&Tok::Star) {
                        return Err(self.error(
                            "`from X import *` is not supported: every unqualified name must \
                             be written down (deny-by-omission). List the names explicitly, or \
                             use `import X` and qualify (`X.Name`)",
                        ));
                    }
                    names.push(self.ident()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.imports.insert(module.clone());
                if !imports.iter().any(|i| i == &module) {
                    imports.push(module.clone());
                    import_lines.push(line);
                }
                from_imports.push((module, names));
            } else {
                break;
            }
        }
        let mut items = Vec::new();
        let mut item_lines = Vec::new();
        while !self.at(&Tok::Eof) {
            item_lines.push(self.cur().line);
            items.push(self.item()?);
        }
        if self.needs_meta_import && !imports.iter().any(|i| i == "meta") {
            imports.push("meta".to_string());
            import_lines.push(u32::MAX);
            self.imports.insert("meta".to_string());
        }
        Ok(Module {
            modes,
            imports,
            from_imports,
            items,
            import_lines,
            item_lines,
            compiler_item_syntax: std::mem::take(&mut self.compiler_item_syntax),
            compiler_expr_syntax: std::mem::take(&mut self.compiler_expr_syntax),
            compiler_type_syntax: std::mem::take(&mut self.compiler_type_syntax),
            compiler_pattern_syntax: std::mem::take(&mut self.compiler_pattern_syntax),
            compiler_stmt_syntax: std::mem::take(&mut self.compiler_stmt_syntax),
            compiler_block_syntax: std::mem::take(&mut self.compiler_block_syntax),
        })
    }

    fn item(&mut self) -> Result<Item, ParseError> {
        let public = self.eat(&Tok::Pub);
        if public && !(self.at(&Tok::Fn) || self.at(&Tok::Gen) || self.at(&Tok::Async)) {
            return Err(self.error(
                "`pub` may only precede a function declaration (`pub fn`, `pub gen fn`, or `pub async fn`)",
            ));
        }
        if self.at(&Tok::Fn) || self.at(&Tok::Gen) || self.at(&Tok::Async) {
            Ok(Item::Function(self.function(public, false)?))
        } else if self.at(&Tok::Type) {
            self.type_def(false)
        } else if self.at_ident("sealed") {
            // `sealed type X:` (RFC-0065) — seal a type's data constructor(s) so a
            // value may be built only in its home module (the same enforcement
            // `capability` uses, generalized to any type). Contextual, so `sealed`
            // stays usable as an ordinary ident.
            self.advance();
            if !self.at(&Tok::Type) {
                return Err(self.error("`sealed` may only precede a `type` declaration"));
            }
            self.type_def(true)
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
            if self.at(&Tok::Fn) {
                return Ok(Item::Function(self.function(false, true)?));
            }
            if self.at(&Tok::Gen) || self.at(&Tok::Async) {
                return Err(self.error("`comptime` may only precede `fn` or a block"));
            }
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
        // `gen`/`async` trait methods are not supported: the async lowering leaves
        // the return type to inference (a phantom `Task`), which no trait signature
        // can declare, and a `gen fn` in a trait would be an asymmetric half-feature
        // (the impl generates a helper the trait can't name). Reject loudly here —
        // declare a plain `fn … -> Iter(_)`/`-> Task(_)` in the trait instead. A
        // `gen`/`async` method may only appear in an inherent `impl Type:` block.
        if self.at(&Tok::Gen) || self.at(&Tok::Async) {
            return Err(self.error(
                "a `gen`/`async` trait method is not supported: declare a plain \
                 `fn` returning `Iter(_)`/`Task(_)` in the trait; a `gen`/`async` \
                 method may only appear in an inherent `impl Type:` block",
            ));
        }
        self.expect(&Tok::Fn)?;
        let name = self.ident()?;
        self.expect(&Tok::LParen)?;
        let params = self.params(true)?;
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
    /// Consume a trailing `.Type` on a module-qualified type head (`set.Set`),
    /// forming the canonical dotted name. Only a lowercase segment followed by an
    /// uppercase one qualifies — a trait name (`FromIterator`) is never dotted.
    fn maybe_qualify_head(&mut self, name: String) -> Result<String, ParseError> {
        if self.at(&Tok::Dot)
            && name.chars().next().is_some_and(|c| c.is_lowercase())
            && matches!(self.toks.get(self.pos + 1).map(|t| &t.kind),
                Some(Tok::Ident(n)) if n.chars().next().is_some_and(|c| c.is_uppercase()))
        {
            self.advance(); // `.`
            let ty = self.ident()?;
            return Ok(format!("{name}.{ty}"));
        }
        Ok(name)
    }

    fn impl_def(&mut self) -> Result<ImplDef, ParseError> {
        self.expect(&Tok::Impl)?;
        let first = self.ident()?;
        let first = self.maybe_qualify_head(first)?;
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
                let head = self.maybe_qualify_head(head)?;
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
                // A method may declare its visibility (`pub fn …`). This matters
                // for type-associated (self-less) constructors — `Net.tcp(…)` —
                // that a module exports as public API (RFC-0057).
                let public = self.eat(&Tok::Pub);
                methods.push(self.function(public, false)?);
            }
            self.expect(&Tok::RBrace)?;
        }
        // A `gen`/`async` method implementing a TRAIT method is rejected: the
        // trait machinery can't express it (async's inferred phantom-`Task` return
        // has no declarable trait signature; a `gen` impl emits a helper the trait
        // can't name), so supporting it would be silent half-wiring. The inherent
        // form (`impl Type:`, no `for`) is fully supported. Loud beats wrong.
        if trait_name.is_some() {
            if let Some(method) = methods.iter().find(|m| m.is_gen || m.is_async) {
                let kw = if method.is_gen { "gen" } else { "async" };
                return Err(self.error(format!(
                    "`{kw} fn {}` cannot implement a trait method: a `gen`/`async` \
                     method is only supported in an inherent `impl {type_name}:` \
                     block (no `for`)",
                    method.name,
                )));
            }
        }
        Ok(ImplDef {
            origin: crate::ast::ImplOrigin::Source,
            trait_name,
            trait_args,
            type_name,
            target_args,
            bounds,
            methods,
        })
    }

    fn type_def(&mut self, sealed: bool) -> Result<Item, ParseError> {
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
        // A type alias: `type Id = Int` or `type Pair(a) = (a, a)`. Expanded to
        // its target before later stages.
        if self.eat(&Tok::Eq) {
            let ty = self.ty()?;
            return Ok(Item::TypeAlias { name, params, ty });
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
        let mut rec_lines: Vec<u32> = Vec::new();
        let mut is_record = false;
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            let line = self.cur().line;
            let ident = self.ident()?;
            if is_record || self.at(&Tok::Colon) {
                // Record field: `name: Type`. The whole type is one constructor.
                is_record = true;
                self.expect(&Tok::Colon)?;
                rec_names.push(ident);
                rec_lines.push(line);
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
                    line,
                    fields,
                    field_names: vec![],
                    field_lines: vec![],
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
                    line: rec_lines.first().copied().unwrap_or(0),
                    fields: rec_types,
                    field_names: rec_names,
                    field_lines: rec_lines,
                }],
                derives,
                sealed,
                is_capability: false,
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
                sealed,
                is_capability: false,
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
    /// ```text
    /// capability Postgres:
    ///     net: Net[Connect, Tcp]
    ///     table: String
    /// ```
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
            let mut field_lines = Vec::new();
            while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
                field_lines.push(self.cur().line);
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
                variants: vec![Variant {
                    name,
                    line: field_lines.first().copied().unwrap_or(0),
                    fields,
                    field_names,
                    field_lines,
                }],
                derives: vec![],
                sealed: true,
                is_capability: true,
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
            variants: vec![Variant {
                name,
                line: 0,
                fields,
                field_names: vec![],
                field_lines: vec![],
            }],
            derives: vec![],
            sealed: true,
            is_capability: true,
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

    fn function(&mut self, public: bool, comptime_only: bool) -> Result<Function, ParseError> {
        let is_async = self.eat(&Tok::Async);
        let is_gen = self.eat(&Tok::Gen);
        if comptime_only && (is_async || is_gen) {
            return Err(self.error("`comptime fn` cannot be `async` or `gen`"));
        }
        self.expect(&Tok::Fn)?;
        let name = self.ident()?;
        self.expect(&Tok::LParen)?;
        self.pending_impl_bounds.clear();
        let params = self.params(true)?;
        self.expect(&Tok::RParen)?;
        if (is_async || is_gen)
            && let Some(param) = params.iter().find(|param| param.convention == Convention::Var)
        {
            let kind = if is_async { "async" } else { "generator" };
            return Err(self.error(format!(
                "a {kind} function cannot take `var` parameter `{}`: suspension may outlive the caller's write-back place; use an ordinary parameter or mutate a local until the lifetime model admits suspended `var` access",
                param.name,
            )));
        }
        let ret = if self.eat(&Tok::RArrow) {
            Some(self.ty()?)
        } else {
            None
        };
        // `impl Trait` params contribute bounds just like a `where` clause; merge
        // them (a function may use both).
        let mut bounds = std::mem::take(&mut self.pending_impl_bounds);
        bounds.extend(self.where_clause()?);
        // `await`/`yield` are only legal inside an `async`/`gen fn`; the body parse
        // consults these flags.
        let prev_async = std::mem::replace(&mut self.in_async, is_async);
        let prev_gen = std::mem::replace(&mut self.in_gen, is_gen);
        let body = self.block()?;
        self.in_async = prev_async;
        self.in_gen = prev_gen;
        Ok(Function {
            public,
            comptime_only,
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

    /// Parse a declaration parameter list. `allow_defaults` gates the RFC-0056
    /// `= <constant>` default syntax: permitted on a function/method declaration,
    /// rejected on a lambda (a function *value*, which never carries defaults).
    fn params(&mut self, allow_defaults: bool) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();
        let mut seen_default = false;
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
            // (RFC-0056) An optional closed-constant default: `port: Int = 443`.
            let default = if allow_defaults && self.eat(&Tok::Eq) {
                let d = self.expr(0)?;
                if !is_closed_const(&d) {
                    return Err(self.error(format!(
                        "the default for parameter `{name}` must be a closed constant \
                         (a literal, `None`, `true`/`false`, `[]`, or a constructor of \
                         constants) — no calls, no references to other parameters or state"
                    )));
                }
                if convention == Convention::Var {
                    return Err(self.error(format!(
                        "a `var` parameter cannot have a default: `{name}` writes back to a \
                         caller variable, so an omitted argument has nothing to write to"
                    )));
                }
                seen_default = true;
                Some(d)
            } else {
                if seen_default {
                    return Err(self.error(format!(
                        "parameter `{name}` has no default but follows one — a defaulted \
                         parameter must be a suffix of the list (all later parameters must \
                         also have defaults)"
                    )));
                }
                None
            };
            params.push(Param {
                name,
                ty,
                convention,
                default,
            });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        Ok(params)
    }

    fn ty(&mut self) -> Result<Type, ParseError> {
        // Types nest independently of expressions (`((((…))))` tuples, generic
        // args), so bound this recursion too. Balanced on the success path.
        self.depth += 1;
        self.check_depth()?;
        let out = self.ty_inner();
        self.depth -= 1;
        out
    }

    fn ty_inner(&mut self) -> Result<Type, ParseError> {
        if self.at(&Tok::QuoteHoleStart) {
            return self.quote_type_hole();
        }
        // Ownership/immutability qualifiers (RFC-0025/0026): `frozen T`, `unique T`,
        // `local unique T`. Contextual — only a qualifier keyword FOLLOWED BY a type
        // is one; a bare `frozen` (nothing following) stays an ordinary type variable.
        if let Some(q) = self.eat_type_qual() {
            return Ok(Type::Qualified(q, Box::new(self.ty()?)));
        }
        // (RFC-0083) A borrowed-parameter view: `let('a) T`. The leading `let` is a
        // TYPE constructor here (a read-only borrow carrying lifetime `'a`),
        // distinct from the `let` parameter *convention* that may precede a
        // parameter name. Recognized only when `let` is immediately followed by
        // `('lifetime)`; a bare `let` is never a type.
        if self.at(&Tok::Let)
            && matches!(self.toks.get(self.pos + 1).map(|t| &t.kind), Some(Tok::LParen))
            && matches!(self.toks.get(self.pos + 2).map(|t| &t.kind), Some(Tok::Lifetime(_)))
        {
            self.advance(); // `let`
            self.expect(&Tok::LParen)?;
            let life = self.lifetime_name()?;
            self.expect(&Tok::RParen)?;
            let inner = self.ty()?;
            return Ok(Type::Qualified(TypeQual::Borrow(life), Box::new(inner)));
        }
        if self.eat(&Tok::Fn) {
            // Function type: `fn(T1, var T2, own T3) -> R`.
            self.expect(&Tok::LParen)?;
            let mut params = Vec::new();
            let mut conventions = Vec::new();
            while !self.at(&Tok::RParen) {
                let convention = if self.eat(&Tok::Var) {
                    Convention::Var
                } else if self.eat(&Tok::Own) {
                    Convention::Own
                } else if self.eat(&Tok::Let) {
                    Convention::Borrow
                } else {
                    Convention::Let
                };
                params.push(self.ty()?);
                conventions.push(convention);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
            self.expect(&Tok::RArrow)?;
            let ret = self.ty()?;
            return Ok(Type::Fn(params, Box::new(ret), conventions));
        }
        if self.eat(&Tok::DotLBrace) {
            return self.anon_record_type();
        }
        if self.eat(&Tok::DotLBracket) {
            return self.anon_union_type();
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
        let mut name = self.ident()?;
        // (RFC-0042) A module-qualified type: `iter.Step`, `json.Json`. A lowercase
        // first segment (a module name) followed by `.` and an uppercase segment (a
        // type name) is a qualified type reference; the linker validates and keeps
        // it as the canonical `module.Type` name. A dot after an uppercase name, or
        // before a lowercase one, is never a type — leave the `.` for the caller.
        if self.at(&Tok::Dot)
            && name.chars().next().is_some_and(|c| c.is_lowercase())
            && matches!(self.toks.get(self.pos + 1).map(|t| &t.kind),
                Some(Tok::Ident(n)) if n.chars().next().is_some_and(|c| c.is_uppercase()))
        {
            self.advance(); // `.`
            let ty = self.ident()?;
            name = format!("{name}.{ty}");
        }
        // (RFC-0083) The borrowed-result surface `View(T, 'a)` — a named type whose
        // second argument is a lifetime, which the general type-argument loop below
        // cannot parse. Desugars to the same `Qualified(Borrow('a), T)` as
        // `let('a) T`.
        if name == "View"
            && self.at(&Tok::LParen)
            && !matches!(self.toks.get(self.pos + 1).map(|t| &t.kind), Some(Tok::RParen))
        {
            self.expect(&Tok::LParen)?;
            let inner = self.ty()?;
            self.expect(&Tok::Comma)?;
            let life = self.lifetime_name()?;
            self.expect(&Tok::RParen)?;
            return Ok(Type::Qualified(TypeQual::Borrow(life), Box::new(inner)));
        }
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
            matches!(
                t,
                Some(Tok::Ident(_))
                    | Some(Tok::LParen)
                    | Some(Tok::Fn)
                    | Some(Tok::DotLBrace)
                    | Some(Tok::DotLBracket)
            )
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
        self.block_after_open()
    }

    fn block_after_open(&mut self) -> Result<Block, ParseError> {
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
        if self.at(&Tok::Yield) {
            // `yield e` produces a value from a `gen fn` — only legal there, mirroring
            // the `.await`/`async fn` gate. Outside one it silently no-op'd on the
            // interpreter but failed to compile: a backend divergence caught here.
            if !self.in_gen {
                return Err(self.error(
                    "`yield` may appear only directly in a `gen fn` body — not inside a lambda \
                     or a non-generator function"
                        .to_string(),
                ));
            }
            self.advance();
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
                // `let _ = e` — evaluate for effects, bind nothing. (RFC-0043)
                // This is the EXPLICIT-DISCARD escape: kept as a wildcard
                // `LetPattern` (not a bare `Stmt::Expr`) so it is distinguishable
                // from an accidental discard — a statement-position method call
                // whose non-Nil result is thrown away is an error, and `let _ =`
                // is how the author says "I meant to discard this". `fmt` prints
                // a wildcard `LetPattern` back as `let _ = e`.
                self.expect(&Tok::Eq)?;
                return Ok(Stmt::LetPattern { pattern: Pattern::Wildcard, value: self.expr(0)? });
            }
            // A SIMPLE binding — `let x = e`, `let x: T = e`, or `var x = e` — is a
            // plain lowercase name (optionally ascribed / mutable): it stays
            // `Stmt::Let` (which carries the ascription and the `var` bit). Detect
            // it by a lowercase identifier followed by `:` or `=`.
            let simple = matches!(self.kind(), Tok::Ident(n) if n.chars().next().is_some_and(char::is_lowercase))
                && matches!(
                    self.toks.get(self.pos + 1).map(|t| &t.kind),
                    Some(Tok::Colon) | Some(Tok::Eq)
                );
            if simple {
                let name = self.ident()?;
                // `let x: T = e` — an optional ascription, unified with the value.
                let ty = if self.eat(&Tok::Colon) { Some(self.ty()?) } else { None };
                self.expect(&Tok::Eq)?;
                let value = self.expr(0)?;
                return Ok(Stmt::Let { name, ty, mutable, value });
            }
            // Otherwise a destructuring pattern (RFC-0052): ONE grammar for every
            // binding position. `let (a, (b, c)) = …`, `let Point(x, y) = p`, a
            // single-variant wrapper — all irrefutable, checked by the refutability
            // pass (a refutable pattern here is a check-time error pointing at
            // `if let`). Bindings are immutable, so `var` + a pattern is rejected.
            if mutable {
                return Err(self.error(
                    "`var` takes a single mutable name, not a destructuring pattern — \
                     use `let` (pattern bindings are immutable)",
                ));
            }
            let pattern = self.pattern()?;
            self.expect(&Tok::Eq)?;
            let value = self.expr(0)?;
            Ok(Stmt::LetPattern { pattern, value })
        } else if self.is_assignment() {
            // The left side is a *place*: a variable, a subscript `x[i]`, or a
            // field `x.f` (RFC-0022). Parse it as an expression (it stops at the
            // assignment operator), then desugar to a plain `Stmt::Assign` of the
            // base variable — `x[i] = v` -> `x.set_at(i, v)`, `x.f = v` ->
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
            if op_tok == Tok::DotDot || op_tok == Tok::DotDotEq {
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
        // Every expression recursion passes through `prefix` (unary chains recurse
        // `prefix`→`prefix`; parentheses recurse `atom`→`expr`→`prefix`), so
        // bounding depth here bounds the whole expression grammar. Balanced on the
        // success path; a depth error aborts the parse so the leak is harmless.
        self.depth += 1;
        self.check_depth()?;
        let out = self.prefix_inner();
        self.depth -= 1;
        out
    }

    fn prefix_inner(&mut self) -> Result<Expr, ParseError> {
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
                // `__try_ctx` intrinsic turns an `Option(T)` or `Result(T, String)`
                // into a `Result(T, String)` carrying `msg` (prepended to a Result's
                // existing String error), which `?` then unwraps. Bare `?` is
                // generic over typed Result errors; the message form is the
                // string-error convenience boundary tracked by RFC-0054.
                if self.on_same_line_as_prev()
                    && (matches!(self.kind(), Tok::Str(_)) || *self.kind() == Tok::LParen)
                {
                    let msg = self.atom()?;
                    let wrapped = Expr::Call {
                        name: crate::intrinsics::TRY_CONTEXT.into(),
                        args: vec![e, msg],
                    };
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
                let args = self.call_args_labeled()?;
                if args.iter().any(|(l, _)| l.is_some()) {
                    // A call through a function *value* has no declared parameter
                    // names to label against (RFC-0056 rule 4/5).
                    return Err(self.error(
                        "labels need the callee's declaration — this is a call through a \
                         function value, which is positional-only",
                    ));
                }
                e = Expr::Apply {
                    func: Box::new(e),
                    args: unlabel(args),
                };
            } else if self.in_match_arm
                && self.at(&Tok::Dot)
                && !self.on_same_line_as_prev()
                && matches!(
                    self.toks.get(self.pos + 1).map(|t| &t.kind),
                    Some(Tok::Ident(n)) if n.chars().next().is_some_and(|c| c.is_uppercase())
                )
            {
                // In an inline match-arm body, the next source line's `.Tag`
                // starts another anonymous-union pattern, not a continuation of
                // the body expression. Lowercase `.method` keeps the existing
                // cross-line method-chain behavior.
                break;
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
                let member_line = self.cur().line;
                let member = self.ident()?;
                if self.at(&Tok::LParen) {
                    if let Expr::Var(module) = &e {
                        if self.imports.contains(module)
                            && member.chars().next().is_some_and(|c| c.is_uppercase())
                            && self.peek_named_record()
                        {
                            e = self.record_literal(format!("{module}.{member}"))?;
                            continue;
                        }
                    }
                    let args = self.call_args_labeled()?;
                    match e {
                        // `mod.func(args)` — a module-qualified call on a bare
                        // imported module name. This is a DIRECT call (statically
                        // known callee), so it may carry keyword labels (RFC-0056).
                        Expr::Var(name) if self.imports.contains(&name) => {
                            let name = format!("{name}.{member}");
                            e = if args.iter().any(|(l, _)| l.is_some()) {
                                Expr::LabeledCall { name, args }
                            } else {
                                let args = unlabel(args);
                                if name == "meta.item"
                                    && let [Expr::Str(source)] = args.as_slice()
                                    && let Some(owned) =
                                        self.compiler_owned_item_literal(source, member_line)
                                {
                                    owned
                                } else if name == "meta.item_join_syntax"
                                    && let Some(owned) =
                                        self.compiler_owned_item_join(&args, member_line)
                                {
                                    owned
                                } else if name == "meta.expr_raw"
                                    && let [Expr::Str(source)] = args.as_slice()
                                    && let Some(owned) =
                                        self.compiler_owned_expr_literal(source, member_line)
                                {
                                    owned
                                } else if name == "meta.expr_join"
                                    && let Some(owned) =
                                        self.compiler_owned_expr_join(&args, member_line)
                                {
                                    owned
                                } else if name == "meta.type_join"
                                    && let Some(owned) =
                                        self.compiler_owned_type_join(&args, member_line)
                                {
                                    owned
                                } else if name == "meta.pattern_join"
                                    && let [Expr::List(parts), Expr::List(holes)] = args.as_slice()
                                    && holes.is_empty()
                                    && let [Expr::Str(source)] = parts.as_slice()
                                    && let Some(owned) =
                                        self.compiler_owned_pattern_literal(source, member_line)
                                {
                                    owned
                                } else if name == "meta.stmt_raw"
                                    && let [Expr::Str(source)] = args.as_slice()
                                    && let Some(owned) =
                                        self.compiler_owned_stmt_literal(source, member_line)
                                {
                                    owned
                                } else if name == "meta.block_raw"
                                    && let [Expr::Str(source)] = args.as_slice()
                                    && let Some(owned) =
                                        self.compiler_owned_block_literal(source, member_line)
                                {
                                    owned
                                } else {
                                    Expr::Call { name, args }
                                }
                            };
                        }
                        // `receiver.method(args)` — UFCS method call: sugar for
                        // `method(receiver, args)` (the method name resolves to a
                        // same-module or imported function in the linker). Kept as
                        // a node so the formatter can print it back. Method callees
                        // resolve LATER (traits.rs), so keyword labels are excluded
                        // here in v1 (RFC-0056): a label on one is a compile error.
                        receiver => {
                            if args.iter().any(|(l, _)| l.is_some()) {
                                return Err(self.error(format!(
                                    "keyword labels are not supported on method calls yet \
                                     (RFC-0056 v1): `{member}` is resolved by the receiver's \
                                     type after linking. Write it as a direct call, e.g. \
                                     `module.{member}(...)`, to label its arguments"
                                )));
                            }
                            e = Expr::MethodCall {
                                receiver: Box::new(receiver),
                                method: member,
                                args: unlabel(args),
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
            Tok::Ident(name) if name == "quote" && self.quote_category().is_some() => {
                self.quote_syntax()
            }
            Tok::QuoteHoleStart => self.quote_expr_hole(),
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
            // `.Tag` / `.Tag(payload, …)` — anonymous tagged-union injection.
            // This arm is reached only where a new expression starts; postfix
            // method/field chains consume `.` in `postfix()` after a receiver.
            Tok::Dot => self.anon_union_injection(),
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
                // `for var x in xs:` keeps its single-plain-variable restriction
                // (a write-back form, not a pattern context — RFC-0043/0052).
                if mutable {
                    let name = self.ident()?;
                    if reserved_source_identifier(&name) {
                        return Err(self.error(format!(
                            "identifier `{name}` is reserved for the compiler"
                        )));
                    }
                    self.expect(&Tok::In)?;
                    let iter = self.expr(0)?;
                    let body = self.block()?;
                    let Expr::Var(list_var) = &iter else {
                        return Err(self.error("`for var x in <list>` requires a plain list variable to write each element back to"));
                    };
                    if for_var_body_escapes(&body) {
                        return Err(self.error(
                            "`for var` does not yet support break/continue/return/`?` in its body (RFC-0028 v1) — use an index loop for early exit",
                        ));
                    }
                    let n = self.compr_counter;
                    self.compr_counter += 1;
                    return Ok(desugar_for_var(n, name, list_var.clone(), body));
                }
                // Otherwise the loop header is a PATTERN (RFC-0052 — one grammar for
                // every binding position). The unparenthesized `for a, b in xs`
                // form is still accepted: parse a pattern, then if a comma follows,
                // gather the rest into a tuple pattern. The comprehension generator
                // parses the same way (`for_pattern`), so the two accept the same
                // headers by construction.
                let pattern = self.for_pattern()?;
                self.expect(&Tok::In)?;
                let iter = self.expr(0)?;
                let mut body = self.block()?;
                // A single plain variable is the common fast path — bind it
                // directly as the loop variable (no destructuring wrapper).
                if let Pattern::Var(name) = &pattern {
                    if reserved_source_identifier(name) {
                        return Err(self.error(format!(
                            "identifier `{name}` is reserved for the compiler"
                        )));
                    }
                    return Ok(Expr::For {
                        var: name.clone(),
                        iter: Box::new(wrap(iter)),
                        body,
                    });
                }
                // Any other (irrefutable) pattern desugars to a fresh element
                // variable plus a leading `let PAT = element` (checked for
                // refutability like every `let` pattern).
                let var = {
                    let v = format!("__fortuple{}", self.compr_counter);
                    self.compr_counter += 1;
                    v
                };
                body.stmts.insert(0, Stmt::LetPattern { pattern, value: Expr::Var(var.clone()) });
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
                // A lambda is a function VALUE — no keyword-argument defaults.
                let params = self.params(false)?;
                self.expect(&Tok::RParen)?;
                // Optional declared return type: `fn(x: Int) -> Bool: ...`. Makes
                // the closure a `?` boundary with that exact type.
                let ret = if self.eat(&Tok::RArrow) {
                    Some(self.ty()?)
                } else {
                    None
                };
                // A lambda is its own function scope, never a generator: `yield`
                // inside it belongs to no generator (the enclosing `gen fn`'s
                // lowering does not descend into closures), so clearing `in_gen`
                // here rejects `yield`-in-lambda at parse time instead of letting
                // it slip through `check` and fail only in codegen (BUG-183).
                let prev_gen = std::mem::replace(&mut self.in_gen, false);
                let body = self.colon_or_block();
                self.in_gen = prev_gen;
                let body = body?;
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

    fn quote_category(&self) -> Option<&str> {
        let category = match self.toks.get(self.pos + 1).map(|t| &t.kind) {
            Some(Tok::Ident(category)) => category.as_str(),
            Some(Tok::Type) => "type",
            _ => return None,
        };
        if !matches!(self.toks.get(self.pos + 2).map(|t| &t.kind), Some(Tok::LBrace)) {
            return None;
        }
        Some(category)
    }

    fn quote_syntax(&mut self) -> Result<Expr, ParseError> {
        let Some(category) = self.quote_category().map(str::to_string) else {
            return Err(self.error(
                "expected `quote expr:`, `quote type:`, `quote pattern:`, `quote stmt:`, \
                 `quote block:`, or `quote item:`",
            ));
        };
        let quote_line = self.cur().line;
        self.advance(); // `quote`
        self.advance(); // category
        self.expect(&Tok::LBrace)?;
        // Quotation itself imports `meta`; make that parse context effective
        // immediately so a quoted `meta.f()` and later expressions classify it
        // as a qualified call exactly as the final Module import says they do.
        self.needs_meta_import = true;
        self.imports.insert("meta".to_string());
        if category == "block" {
            let type_base = self.quote_type_holes.len();
            let pattern_base = self.quote_pattern_holes.len();
            self.quote_type_hole_bases.push(type_base);
            self.quote_pattern_hole_bases.push(pattern_base);
            self.quote_expr_hole_depth += 1;
            self.quote_type_hole_depth += 1;
            self.quote_pattern_hole_depth += 1;
            let quoted = self.block_after_open();
            self.quote_expr_hole_depth -= 1;
            self.quote_type_hole_depth -= 1;
            self.quote_pattern_hole_depth -= 1;
            self.quote_type_hole_bases.pop();
            self.quote_pattern_hole_bases.pop();
            if quoted.is_err() {
                self.quote_type_holes.truncate(type_base);
                self.quote_pattern_holes.truncate(pattern_base);
            }
            let quoted = quoted?;
            let type_holes = self.quote_type_holes.split_off(type_base);
            let pattern_holes = self.quote_pattern_holes.split_off(pattern_base);
            return self.block_syntax_expr_with_holes(
                quoted,
                type_holes,
                pattern_holes,
                quote_line,
            );
        }
        let quoted = match category.as_str() {
            "expr" => {
                self.quote_expr_hole_depth += 1;
                let quoted = self.expr(0);
                self.quote_expr_hole_depth -= 1;
                let quoted = quoted?;
                self.quote_expr_syntax_expr(quoted, quote_line)?
            }
            "type" => {
                let base = self.quote_type_holes.len();
                self.quote_type_hole_bases.push(base);
                self.quote_type_hole_depth += 1;
                let quoted = self.ty();
                self.quote_type_hole_depth -= 1;
                self.quote_type_hole_bases.pop();
                if quoted.is_err() {
                    self.quote_type_holes.truncate(base);
                }
                let quoted = quoted?;
                let holes = self.quote_type_holes.split_off(base);
                self.type_syntax_expr_with_holes(quoted, holes, quote_line)?
            }
            "pattern" => {
                let base = self.quote_pattern_holes.len();
                self.quote_pattern_hole_bases.push(base);
                self.quote_pattern_hole_depth += 1;
                let quoted = self.pattern();
                self.quote_pattern_hole_depth -= 1;
                self.quote_pattern_hole_bases.pop();
                if quoted.is_err() {
                    self.quote_pattern_holes.truncate(base);
                }
                let quoted = quoted?;
                let holes = self.quote_pattern_holes.split_off(base);
                self.pattern_syntax_expr_with_holes(quoted, holes, quote_line)?
            }
            "stmt" => {
                let type_base = self.quote_type_holes.len();
                let pattern_base = self.quote_pattern_holes.len();
                self.quote_type_hole_bases.push(type_base);
                self.quote_pattern_hole_bases.push(pattern_base);
                self.quote_expr_hole_depth += 1;
                self.quote_type_hole_depth += 1;
                self.quote_pattern_hole_depth += 1;
                let quoted = self.stmt();
                self.quote_expr_hole_depth -= 1;
                self.quote_type_hole_depth -= 1;
                self.quote_pattern_hole_depth -= 1;
                self.quote_type_hole_bases.pop();
                self.quote_pattern_hole_bases.pop();
                if quoted.is_err() {
                    self.quote_type_holes.truncate(type_base);
                    self.quote_pattern_holes.truncate(pattern_base);
                }
                let quoted = quoted?;
                let type_holes = self.quote_type_holes.split_off(type_base);
                let pattern_holes = self.quote_pattern_holes.split_off(pattern_base);
                self.stmt_syntax_expr_with_holes(
                    quoted,
                    type_holes,
                    pattern_holes,
                    quote_line,
                )?
            }
            "item" => {
                let type_base = self.quote_type_holes.len();
                let pattern_base = self.quote_pattern_holes.len();
                self.quote_type_hole_bases.push(type_base);
                self.quote_pattern_hole_bases.push(pattern_base);
                self.quote_expr_hole_depth += 1;
                self.quote_type_hole_depth += 1;
                self.quote_pattern_hole_depth += 1;
                let quoted = self.item();
                self.quote_expr_hole_depth -= 1;
                self.quote_type_hole_depth -= 1;
                self.quote_pattern_hole_depth -= 1;
                self.quote_type_hole_bases.pop();
                self.quote_pattern_hole_bases.pop();
                if quoted.is_err() {
                    self.quote_type_holes.truncate(type_base);
                    self.quote_pattern_holes.truncate(pattern_base);
                }
                let quoted = quoted?;
                let type_holes = self.quote_type_holes.split_off(type_base);
                let pattern_holes = self.quote_pattern_holes.split_off(pattern_base);
                self.item_syntax_expr_with_holes(
                    quoted,
                    type_holes,
                    pattern_holes,
                    quote_line,
                )?
            }
            _ => {
                return Err(self.error(format!(
                    "`quote {category}:` is not implemented yet; use `quote expr:`, \
                     `quote type:`, `quote pattern:`, `quote stmt:`, `quote block:`, \
                     or `quote item:`"
                )));
            }
        };
        self.expect(&Tok::RBrace)?;
        Ok(quoted)
    }

    fn quote_expr_hole(&mut self) -> Result<Expr, ParseError> {
        if self.quote_expr_hole_depth == 0 {
            return Err(self.error("`${...}` quote holes are only valid inside `quote expr:`"));
        }
        let expr = self.quote_hole_expr()?;
        Ok(Expr::Call {
            name: QUOTE_EXPR_HOLE_INTRINSIC.to_string(),
            args: vec![expr],
        })
    }

    fn quote_hole_expr(&mut self) -> Result<Expr, ParseError> {
        self.advance(); // `${`
        let saved_expr_depth = std::mem::replace(&mut self.quote_expr_hole_depth, 0);
        let saved_type_depth = std::mem::replace(&mut self.quote_type_hole_depth, 0);
        let saved_pattern_depth = std::mem::replace(&mut self.quote_pattern_hole_depth, 0);
        let expr = self.expr(0);
        self.quote_expr_hole_depth = saved_expr_depth;
        self.quote_type_hole_depth = saved_type_depth;
        self.quote_pattern_hole_depth = saved_pattern_depth;
        let expr = expr?;
        self.expect(&Tok::RBrace)?;
        Ok(expr)
    }

    fn quote_type_hole(&mut self) -> Result<Type, ParseError> {
        if self.quote_type_hole_depth == 0 {
            return Err(self.error("`${...}` type quote holes are only valid inside `quote type:`"));
        }
        let Some(base) = self.quote_type_hole_bases.last().copied() else {
            return Err(self.error("internal error: quote type hole has no active quote"));
        };
        let expr = self.quote_hole_expr()?;
        let idx = self.quote_type_holes.len().saturating_sub(base);
        self.quote_type_holes.push(expr);
        Ok(Type::Named(format!("{QUOTE_TYPE_HOLE_PREFIX}{idx}"), Vec::new()))
    }

    fn quote_pattern_hole(&mut self) -> Result<Pattern, ParseError> {
        if self.quote_pattern_hole_depth == 0 {
            return Err(self.error(
                "`${...}` pattern quote holes are only valid inside `quote pattern:`",
            ));
        }
        let Some(base) = self.quote_pattern_hole_bases.last().copied() else {
            return Err(self.error("internal error: quote pattern hole has no active quote"));
        };
        let expr = self.quote_hole_expr()?;
        let idx = self.quote_pattern_holes.len().saturating_sub(base);
        self.quote_pattern_holes.push(expr);
        Ok(Pattern::Var(format!("{QUOTE_PATTERN_HOLE_PREFIX}{idx}")))
    }

    fn quote_expr_syntax_expr(
        &mut self,
        mut quoted: Expr,
        definition_line: u32,
    ) -> Result<Expr, ParseError> {
        let mut holes = Vec::new();
        Self::collect_quote_expr_holes(&mut quoted, &mut holes);
        let source = crate::format::expr_str(&quoted);
        if holes.is_empty() {
            return Ok(self.compiler_owned_expr(quoted, source, definition_line));
        }
        let parts = self.quote_hole_parts(&source, QUOTE_EXPR_HOLE_PREFIX, holes.len(), "expression")?;
        let (handle, _) = self.register_expr_syntax(quoted, source, definition_line);
        Ok(Expr::Call {
            name: crate::intrinsics::COMPILER_QUOTE_EXPR_HOLES.into(),
            args: vec![Expr::Str(handle), Expr::List(parts), Expr::List(holes)],
        })
    }

    fn compiler_owned_expr(
        &mut self,
        quoted: Expr,
        source: String,
        definition_line: u32,
    ) -> Expr {
        let (handle, source) = self.register_expr_syntax(quoted, source, definition_line);
        Expr::Call {
            name: crate::intrinsics::COMPILER_QUOTE_EXPR.into(),
            args: vec![Expr::Str(handle), Expr::Str(source)],
        }
    }

    fn register_expr_syntax(
        &mut self,
        quoted: Expr,
        source: String,
        definition_line: u32,
    ) -> (String, String) {
        let handle = self.compiler_syntax_handle("expr", &source);
        self.compiler_expr_syntax.push(CompilerExprSyntax {
            handle: handle.clone(),
            expr: quoted,
            definition_line,
        });
        (handle, source)
    }

    fn compiler_owned_expr_literal(
        &mut self,
        source: &str,
        definition_line: u32,
    ) -> Option<Expr> {
        let mut wrapper = String::new();
        let mut imports: Vec<&str> = self.imports.iter().map(String::as_str).collect();
        imports.sort_unstable();
        for import in imports {
            wrapper.push_str("import ");
            wrapper.push_str(import);
            wrapper.push('\n');
        }
        wrapper.push_str("fn __witchy_expr_syntax_payload():\n");
        for line in source.lines() {
            wrapper.push_str("    ");
            wrapper.push_str(line);
            wrapper.push('\n');
        }
        // Parse without `parse_module`'s synthetic anonymous-record insertion;
        // merge those shapes into the enclosing parser instead. This lets a
        // formatted `meta.expr_raw(".{x: 1}")` reconstruct the same owned AST
        // and causes the enclosing module to emit the required structural type.
        let tokens = tokenize(&wrapper).ok()?;
        let tokens = crate::lexer::apply_layout(tokens);
        let mut payload_parser = Parser::new(tokens);
        let mut parsed = payload_parser.module().ok()?;
        let [Item::Function(function)] = parsed.items.as_slice() else {
            return None;
        };
        let [Stmt::Expr(expr)] = function.body.stmts.as_slice() else {
            return None;
        };
        let expr = expr.clone();
        for fields in payload_parser.anon_records {
            if !self.anon_records.contains(&fields) {
                self.anon_records.push(fields);
            }
        }
        self.compiler_item_syntax.append(&mut parsed.compiler_item_syntax);
        self.compiler_expr_syntax.append(&mut parsed.compiler_expr_syntax);
        self.compiler_type_syntax.append(&mut parsed.compiler_type_syntax);
        self.compiler_pattern_syntax.append(&mut parsed.compiler_pattern_syntax);
        self.compiler_stmt_syntax.append(&mut parsed.compiler_stmt_syntax);
        self.compiler_block_syntax.append(&mut parsed.compiler_block_syntax);
        Some(self.compiler_owned_expr(expr, source.to_string(), definition_line))
    }

    fn compiler_owned_expr_join(
        &mut self,
        args: &[Expr],
        definition_line: u32,
    ) -> Option<Expr> {
        let [Expr::List(parts), Expr::List(holes)] = args else {
            return None;
        };
        if parts.len() != holes.len() + 1 {
            return None;
        }
        let mut source = String::new();
        for (index, part) in parts.iter().enumerate() {
            let Expr::Str(part) = part else {
                return None;
            };
            source.push_str(part);
            if index < holes.len() {
                source.push_str(&format!("{QUOTE_EXPR_HOLE_PREFIX}{index}"));
            }
        }
        let owned = self.compiler_owned_expr_literal(&source, definition_line)?;
        let Expr::Call { name, args: owned_args } = owned else {
            return None;
        };
        if name != crate::intrinsics::COMPILER_QUOTE_EXPR {
            return None;
        }
        let [Expr::Str(handle), Expr::Str(_)] = owned_args.as_slice() else {
            return None;
        };
        Some(Expr::Call {
            name: crate::intrinsics::COMPILER_QUOTE_EXPR_HOLES.into(),
            args: vec![
                Expr::Str(handle.clone()),
                Expr::List(parts.clone()),
                Expr::List(holes.clone()),
            ],
        })
    }

    fn compiler_owned_type_literal(
        &mut self,
        source: &str,
        definition_line: u32,
    ) -> Option<Expr> {
        let wrapper = format!("type TypeSyntaxPayload = {source}\n");
        let tokens = tokenize(&wrapper).ok()?;
        let tokens = crate::lexer::apply_layout(tokens);
        let mut payload_parser = Parser::new(tokens);
        let parsed = payload_parser.module().ok()?;
        let [Item::TypeAlias { ty, .. }] = parsed.items.as_slice() else {
            return None;
        };
        let ty = ty.clone();
        for fields in payload_parser.anon_records {
            if !self.anon_records.contains(&fields) {
                self.anon_records.push(fields);
            }
        }
        let handle = self.register_type_syntax(ty, source, definition_line);
        Some(Expr::Call {
            name: crate::intrinsics::COMPILER_QUOTE_TYPE.into(),
            args: vec![Expr::Str(handle), Expr::Str(source.to_string())],
        })
    }

    fn register_type_syntax(
        &mut self,
        ty: Type,
        source: &str,
        definition_line: u32,
    ) -> String {
        let handle = self.compiler_syntax_handle("type", source);
        self.compiler_type_syntax.push(CompilerTypeSyntax {
            handle: handle.clone(),
            ty,
            definition_line,
        });
        handle
    }

    fn compiler_owned_type_join(
        &mut self,
        args: &[Expr],
        definition_line: u32,
    ) -> Option<Expr> {
        let [Expr::List(parts), Expr::List(holes)] = args else {
            return None;
        };
        if parts.len() != holes.len() + 1 {
            return None;
        }
        if holes.is_empty() {
            let [Expr::Str(source)] = parts.as_slice() else {
                return None;
            };
            return self.compiler_owned_type_literal(source, definition_line);
        }
        let mut source = String::new();
        for (index, part) in parts.iter().enumerate() {
            let Expr::Str(part) = part else {
                return None;
            };
            source.push_str(part);
            if index < holes.len() {
                source.push_str(&format!("{QUOTE_TYPE_HOLE_PREFIX}{index}"));
            }
        }
        let owned = self.compiler_owned_type_literal(&source, definition_line)?;
        let Expr::Call { name, args: owned_args } = owned else {
            return None;
        };
        if name != crate::intrinsics::COMPILER_QUOTE_TYPE {
            return None;
        }
        let [Expr::Str(handle), Expr::Str(_)] = owned_args.as_slice() else {
            return None;
        };
        Some(Expr::Call {
            name: crate::intrinsics::COMPILER_QUOTE_TYPE_HOLES.into(),
            args: vec![
                Expr::Str(handle.clone()),
                Expr::List(parts.clone()),
                Expr::List(holes.clone()),
            ],
        })
    }

    fn compiler_owned_pattern_literal(
        &mut self,
        source: &str,
        definition_line: u32,
    ) -> Option<Expr> {
        let wrapper = format!("fn pattern_syntax_payload(value: Int):\n    match value:\n        {source} -> 1\n        _ -> 0\n");
        let tokens = tokenize(&wrapper).ok()?;
        let tokens = crate::lexer::apply_layout(tokens);
        let mut payload_parser = Parser::new(tokens);
        let parsed = payload_parser.module().ok()?;
        let [Item::Function(function)] = parsed.items.as_slice() else {
            return None;
        };
        let [Stmt::Expr(Expr::Match { arms, .. })] = function.body.stmts.as_slice() else {
            return None;
        };
        let [arm, _fallback] = arms.as_slice() else {
            return None;
        };
        let pattern = arm.pattern.clone();
        let handle = self.compiler_syntax_handle("pattern", source);
        self.compiler_pattern_syntax.push(CompilerPatternSyntax {
            handle: handle.clone(),
            pattern,
            definition_line,
        });
        Some(Expr::Call {
            name: crate::intrinsics::COMPILER_QUOTE_PATTERN.into(),
            args: vec![Expr::Str(handle), Expr::Str(source.to_string())],
        })
    }

    fn compiler_owned_stmt_literal(
        &mut self,
        source: &str,
        definition_line: u32,
    ) -> Option<Expr> {
        let mut wrapper = "fn stmt_syntax_payload():\n".to_string();
        Self::push_indented_source(&mut wrapper, source);
        wrapper.push_str("    0\n");
        let (parsed, payload_parser) = Self::parse_payload_module(&wrapper)?;
        let [Item::Function(function)] = parsed.items.as_slice() else {
            return None;
        };
        let [stmt, Stmt::Expr(Expr::Int(0))] = function.body.stmts.as_slice() else {
            return None;
        };
        let stmt = stmt.clone();
        self.merge_payload_parser(parsed, payload_parser);
        let source = crate::format::stmt_str(&stmt);
        let handle = self.compiler_syntax_handle("stmt", &source);
        self.compiler_stmt_syntax.push(CompilerStmtSyntax {
            handle: handle.clone(),
            stmt,
            definition_line,
        });
        Some(Expr::Call {
            name: crate::intrinsics::COMPILER_QUOTE_STMT.into(),
            args: vec![Expr::Str(handle), Expr::Str(source)],
        })
    }

    fn compiler_owned_block_literal(
        &mut self,
        source: &str,
        definition_line: u32,
    ) -> Option<Expr> {
        let mut wrapper = "fn block_syntax_payload():\n".to_string();
        Self::push_indented_source(&mut wrapper, source);
        let (parsed, payload_parser) = Self::parse_payload_module(&wrapper)?;
        let [Item::Function(function)] = parsed.items.as_slice() else {
            return None;
        };
        let block = function.body.clone();
        self.merge_payload_parser(parsed, payload_parser);
        let source = crate::format::block_str(&block);
        let handle = self.compiler_syntax_handle("block", &source);
        self.compiler_block_syntax.push(CompilerBlockSyntax {
            handle: handle.clone(),
            block,
            definition_line,
        });
        Some(Expr::Call {
            name: crate::intrinsics::COMPILER_QUOTE_BLOCK.into(),
            args: vec![Expr::Str(handle), Expr::Str(source)],
        })
    }

    fn push_indented_source(wrapper: &mut String, source: &str) {
        for line in source.lines() {
            wrapper.push_str("    ");
            wrapper.push_str(line);
            wrapper.push('\n');
        }
    }

    fn parse_payload_module(source: &str) -> Option<(Module, Parser)> {
        let tokens = tokenize(source).ok()?;
        let tokens = crate::lexer::apply_layout(tokens);
        let mut parser = Parser::new(tokens);
        let module = parser.module().ok()?;
        Some((module, parser))
    }

    fn merge_payload_parser(&mut self, mut parsed: Module, payload_parser: Parser) {
        for fields in payload_parser.anon_records {
            if !self.anon_records.contains(&fields) {
                self.anon_records.push(fields);
            }
        }
        self.compiler_item_syntax.append(&mut parsed.compiler_item_syntax);
        self.compiler_expr_syntax.append(&mut parsed.compiler_expr_syntax);
        self.compiler_type_syntax.append(&mut parsed.compiler_type_syntax);
        self.compiler_pattern_syntax.append(&mut parsed.compiler_pattern_syntax);
        self.compiler_stmt_syntax.append(&mut parsed.compiler_stmt_syntax);
        self.compiler_block_syntax.append(&mut parsed.compiler_block_syntax);
    }

    fn quote_hole_parts(
        &self,
        source: &str,
        prefix: &str,
        count: usize,
        category: &str,
    ) -> Result<Vec<Expr>, ParseError> {
        let mut parts = Vec::with_capacity(count + 1);
        let mut rest = source;
        for i in 0..count {
            let marker = format!("{prefix}{i}");
            let Some(pos) = Self::quote_marker_pos(rest, &marker) else {
                return Err(self.error(format!(
                    "internal error: quote {category} hole marker was lost"
                )));
            };
            let (before, after_marker) = rest.split_at(pos);
            let after = &after_marker[marker.len()..];
            parts.push(Expr::Str(before.to_string()));
            rest = after;
        }
        parts.push(Expr::Str(rest.to_string()));
        Ok(parts)
    }

    fn type_syntax_expr_with_holes(
        &mut self,
        quoted: Type,
        holes: Vec<Expr>,
        definition_line: u32,
    ) -> Result<Expr, ParseError> {
        if holes.is_empty() {
            let source = crate::format::type_str(&quoted);
            let handle = self.register_type_syntax(quoted, &source, definition_line);
            return Ok(Expr::Call {
                name: crate::intrinsics::COMPILER_QUOTE_TYPE.into(),
                args: vec![Expr::Str(handle), Expr::Str(source)],
            });
        }
        let source = crate::format::type_str(&quoted);
        let parts = self.quote_hole_parts(&source, QUOTE_TYPE_HOLE_PREFIX, holes.len(), "type")?;
        let handle = self.register_type_syntax(quoted, &source, definition_line);
        Ok(Expr::Call {
            name: crate::intrinsics::COMPILER_QUOTE_TYPE_HOLES.into(),
            args: vec![Expr::Str(handle), Expr::List(parts), Expr::List(holes)],
        })
    }

    fn pattern_syntax_expr_with_holes(
        &mut self,
        quoted: Pattern,
        holes: Vec<Expr>,
        definition_line: u32,
    ) -> Result<Expr, ParseError> {
        if holes.is_empty() {
            let source = crate::format::pattern_str(&quoted);
            let handle = self.compiler_syntax_handle("pattern", &source);
            self.compiler_pattern_syntax.push(CompilerPatternSyntax {
                handle: handle.clone(),
                pattern: quoted,
                definition_line,
            });
            return Ok(Expr::Call {
                name: crate::intrinsics::COMPILER_QUOTE_PATTERN.into(),
                args: vec![Expr::Str(handle), Expr::Str(source)],
            });
        }
        let source = crate::format::pattern_str(&quoted);
        let parts =
            self.quote_hole_parts(&source, QUOTE_PATTERN_HOLE_PREFIX, holes.len(), "pattern")?;
        Ok(self.meta_call("pattern_join", vec![Expr::List(parts), Expr::List(holes)]))
    }

    fn stmt_syntax_expr_with_holes(
        &mut self,
        mut quoted: Stmt,
        type_holes: Vec<Expr>,
        pattern_holes: Vec<Expr>,
        definition_line: u32,
    ) -> Result<Expr, ParseError> {
        let mut expr_holes = Vec::new();
        Self::collect_quote_expr_holes_stmt(&mut quoted, &mut expr_holes);
        if expr_holes.is_empty() && type_holes.is_empty() && pattern_holes.is_empty() {
            let source = crate::format::stmt_str(&quoted);
            let handle = self.compiler_syntax_handle("stmt", &source);
            self.compiler_stmt_syntax.push(CompilerStmtSyntax {
                handle: handle.clone(),
                stmt: quoted,
                definition_line,
            });
            return Ok(Expr::Call {
                name: crate::intrinsics::COMPILER_QUOTE_STMT.into(),
                args: vec![Expr::Str(handle), Expr::Str(source)],
            });
        }
        let source = crate::format::stmt_str(&quoted);
        let (parts, holes) =
            self.quote_mixed_hole_parts(&source, expr_holes, type_holes, pattern_holes, "statement")?;
        Ok(self.meta_call("stmt_join_syntax", vec![Expr::List(parts), Expr::List(holes)]))
    }

    fn block_syntax_expr_with_holes(
        &mut self,
        mut quoted: Block,
        type_holes: Vec<Expr>,
        pattern_holes: Vec<Expr>,
        definition_line: u32,
    ) -> Result<Expr, ParseError> {
        let mut expr_holes = Vec::new();
        Self::collect_quote_expr_holes_block(&mut quoted, &mut expr_holes);
        if expr_holes.is_empty() && type_holes.is_empty() && pattern_holes.is_empty() {
            let source = crate::format::block_str(&quoted);
            let handle = self.compiler_syntax_handle("block", &source);
            self.compiler_block_syntax.push(CompilerBlockSyntax {
                handle: handle.clone(),
                block: quoted,
                definition_line,
            });
            return Ok(Expr::Call {
                name: crate::intrinsics::COMPILER_QUOTE_BLOCK.into(),
                args: vec![Expr::Str(handle), Expr::Str(source)],
            });
        }
        let source = crate::format::block_str(&quoted);
        let (parts, holes) =
            self.quote_mixed_hole_parts(&source, expr_holes, type_holes, pattern_holes, "block")?;
        Ok(self.meta_call("block_join_syntax", vec![Expr::List(parts), Expr::List(holes)]))
    }

    fn item_syntax_expr(&mut self, quoted: Item, definition_line: u32) -> Expr {
        let (handle, source) = self.register_item_syntax(quoted, definition_line);
        Expr::Call {
            name: crate::intrinsics::COMPILER_QUOTE_ITEM.into(),
            args: vec![Expr::Str(handle), Expr::Str(source)],
        }
    }

    fn register_item_syntax(&mut self, quoted: Item, definition_line: u32) -> (String, String) {
        let source = Self::item_source(quoted.clone());
        let handle = self.compiler_syntax_handle("item", &source);
        self.compiler_item_syntax.push(CompilerItemSyntax {
            handle: handle.clone(),
            item: quoted,
            definition_line,
        });
        (handle, source)
    }

    fn compiler_syntax_handle(&self, category: &str, source: &str) -> String {
        // Imports affect whether `x.y()` is a qualified call or a method call,
        // so source alone is not an AST identity. Length-prefix the sorted parse
        // context to keep handles deterministic and collision-free without a
        // native-only hashing dependency in the browser parser.
        let mut imports: Vec<&str> = self.imports.iter().map(String::as_str).collect();
        imports.sort_unstable();
        let mut handle = format!("witchy-compiler-{category}-syntax-v2\0");
        for import in imports {
            handle.push_str(&format!("{}:{import}", import.len()));
        }
        handle.push('\0');
        handle.push_str(source);
        handle
    }

    fn compiler_owned_item_literal(
        &mut self,
        source: &str,
        definition_line: u32,
    ) -> Option<Expr> {
        let mut parsed = parse_module(source).ok()?;
        if !parsed.modes.is_empty()
            || !parsed.imports.is_empty()
            || !parsed.from_imports.is_empty()
            || parsed.items.len() != 1
        {
            return None;
        }
        self.compiler_item_syntax
            .append(&mut parsed.compiler_item_syntax);
        Some(self.item_syntax_expr(parsed.items.remove(0), definition_line))
    }

    fn compiler_owned_item_join(&mut self, args: &[Expr], definition_line: u32) -> Option<Expr> {
        let [Expr::List(parts), Expr::List(holes)] = args else {
            return None;
        };
        if parts.len() != holes.len() + 1 {
            return None;
        }
        let mut expr_index = 0;
        let mut type_index = 0;
        let mut pattern_index = 0;
        let mut source = String::new();
        for (index, part) in parts.iter().enumerate() {
            let Expr::Str(part) = part else {
                return None;
            };
            source.push_str(part);
            let Some(hole) = holes.get(index) else {
                continue;
            };
            let Expr::Call { name, args } = hole else {
                return None;
            };
            if args.len() != 1 {
                return None;
            }
            let marker = match name.as_str() {
                "meta.expr_hole" => {
                    let marker = format!("{QUOTE_EXPR_HOLE_PREFIX}{expr_index}");
                    expr_index += 1;
                    marker
                }
                "meta.type_hole" => {
                    let marker = format!("{QUOTE_TYPE_HOLE_PREFIX}{type_index}");
                    type_index += 1;
                    marker
                }
                "meta.pattern_hole" => {
                    let marker = format!("{QUOTE_PATTERN_HOLE_PREFIX}{pattern_index}");
                    pattern_index += 1;
                    marker
                }
                _ => return None,
            };
            source.push_str(&marker);
        }
        let mut parsed = parse_module(&source).ok()?;
        if !parsed.modes.is_empty()
            || !parsed.imports.is_empty()
            || !parsed.from_imports.is_empty()
            || parsed.items.len() != 1
        {
            return None;
        }
        self.compiler_item_syntax.append(&mut parsed.compiler_item_syntax);
        let (handle, _) = self.register_item_syntax(parsed.items.remove(0), definition_line);
        Some(Expr::Call {
            name: crate::intrinsics::COMPILER_QUOTE_ITEM_HOLES.into(),
            args: vec![Expr::Str(handle), Expr::List(parts.clone()), Expr::List(holes.clone())],
        })
    }

    fn item_syntax_expr_with_holes(
        &mut self,
        mut quoted: Item,
        type_holes: Vec<Expr>,
        pattern_holes: Vec<Expr>,
        definition_line: u32,
    ) -> Result<Expr, ParseError> {
        let mut expr_holes = Vec::new();
        Self::collect_quote_expr_holes_item(&mut quoted, &mut expr_holes);
        if expr_holes.is_empty() && type_holes.is_empty() && pattern_holes.is_empty() {
            return Ok(self.item_syntax_expr(quoted, definition_line));
        }
        let source = Self::item_source(quoted.clone());
        let (parts, holes) =
            self.quote_mixed_hole_parts(&source, expr_holes, type_holes, pattern_holes, "item")?;
        let (handle, _) = self.register_item_syntax(quoted, definition_line);
        Ok(Expr::Call {
            name: crate::intrinsics::COMPILER_QUOTE_ITEM_HOLES.into(),
            args: vec![Expr::Str(handle), Expr::List(parts), Expr::List(holes)],
        })
    }

    fn item_source(item: Item) -> String {
        let module = Module {
            modes: Vec::new(),
            imports: Vec::new(),
            from_imports: Vec::new(),
            items: vec![item],
            import_lines: Vec::new(),
            item_lines: vec![1],
            compiler_item_syntax: Vec::new(),
            compiler_expr_syntax: Vec::new(),
            compiler_type_syntax: Vec::new(),
            compiler_pattern_syntax: Vec::new(),
            compiler_stmt_syntax: Vec::new(),
            compiler_block_syntax: Vec::new(),
        };
        crate::format::module(&module, &[])
    }

    fn quote_marker_pos(source: &str, marker: &str) -> Option<usize> {
        let mut offset = 0;
        while let Some(pos) = source[offset..].find(marker) {
            let absolute = offset + pos;
            let next = source.as_bytes().get(absolute + marker.len()).copied();
            if !next.is_some_and(|b| b.is_ascii_digit()) {
                return Some(absolute);
            }
            offset = absolute + marker.len();
        }
        None
    }

    fn quote_mixed_hole_parts(
        &self,
        source: &str,
        expr_holes: Vec<Expr>,
        type_holes: Vec<Expr>,
        pattern_holes: Vec<Expr>,
        category: &str,
    ) -> Result<(Vec<Expr>, Vec<Expr>), ParseError> {
        let mut markers = Vec::new();
        for (idx, hole) in expr_holes.into_iter().enumerate() {
            let marker = format!("{QUOTE_EXPR_HOLE_PREFIX}{idx}");
            let Some(pos) = Self::quote_marker_pos(source, &marker) else {
                return Err(self.error(format!(
                    "internal error: quote {category} expression hole marker was lost"
                )));
            };
            markers.push((pos, marker.len(), self.meta_call("expr_hole", vec![hole])));
        }
        for (idx, hole) in type_holes.into_iter().enumerate() {
            let marker = format!("{QUOTE_TYPE_HOLE_PREFIX}{idx}");
            let Some(pos) = Self::quote_marker_pos(source, &marker) else {
                return Err(self.error(format!(
                    "internal error: quote {category} type hole marker was lost"
                )));
            };
            markers.push((pos, marker.len(), self.meta_call("type_hole", vec![hole])));
        }
        for (idx, hole) in pattern_holes.into_iter().enumerate() {
            let marker = format!("{QUOTE_PATTERN_HOLE_PREFIX}{idx}");
            let Some(pos) = Self::quote_marker_pos(source, &marker) else {
                return Err(self.error(format!(
                    "internal error: quote {category} pattern hole marker was lost"
                )));
            };
            markers.push((pos, marker.len(), self.meta_call("pattern_hole", vec![hole])));
        }

        markers.sort_by_key(|(pos, _, _)| *pos);
        let mut parts = Vec::with_capacity(markers.len() + 1);
        let mut holes = Vec::with_capacity(markers.len());
        let mut cursor = 0;
        for (pos, len, hole) in markers {
            if pos < cursor {
                return Err(self.error(format!(
                    "internal error: quote {category} hole markers overlapped"
                )));
            }
            parts.push(Expr::Str(source[cursor..pos].to_string()));
            holes.push(hole);
            cursor = pos + len;
        }
        parts.push(Expr::Str(source[cursor..].to_string()));
        Ok((parts, holes))
    }

    fn collect_quote_expr_holes(expr: &mut Expr, holes: &mut Vec<Expr>) {
        if let Expr::Call { name, args } = expr {
            if name == QUOTE_EXPR_HOLE_INTRINSIC && args.len() == 1 {
                let idx = holes.len();
                let hole = args.pop().expect("checked one quote hole arg");
                holes.push(hole);
                *expr = Expr::Var(format!("{QUOTE_EXPR_HOLE_PREFIX}{idx}"));
                return;
            }
        }

        match expr {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Var(_)
            | Expr::TaggedLit { .. } => {}
            Expr::List(xs)
            | Expr::Tuple(xs)
            | Expr::Call { args: xs, .. }
            | Expr::Ctor { args: xs, .. }
            | Expr::AnonCtor { args: xs, .. } => {
                for x in xs {
                    Self::collect_quote_expr_holes(x, holes);
                }
            }
            Expr::LabeledCall { args, .. } => {
                for (_, x) in args {
                    Self::collect_quote_expr_holes(x, holes);
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                Self::collect_quote_expr_holes(receiver, holes);
                for x in args {
                    Self::collect_quote_expr_holes(x, holes);
                }
            }
            Expr::Apply { func, args } => {
                Self::collect_quote_expr_holes(func, holes);
                for x in args {
                    Self::collect_quote_expr_holes(x, holes);
                }
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::Field { base: expr, .. } => Self::collect_quote_expr_holes(expr, holes),
            Expr::Lambda { body, .. } | Expr::Block(body) => {
                Self::collect_quote_expr_holes_block(body, holes);
            }
            Expr::RecordUpdate { base, fields, .. } => {
                Self::collect_quote_expr_holes(base, holes);
                for (_, x) in fields {
                    Self::collect_quote_expr_holes(x, holes);
                }
            }
            Expr::Record { fields, spread, .. } => {
                for (_, x) in fields {
                    Self::collect_quote_expr_holes(x, holes);
                }
                if let Some(spread) = spread {
                    Self::collect_quote_expr_holes(spread, holes);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                Self::collect_quote_expr_holes(lhs, holes);
                Self::collect_quote_expr_holes(rhs, holes);
            }
            Expr::If { cond, then_block, else_block } => {
                Self::collect_quote_expr_holes(cond, holes);
                Self::collect_quote_expr_holes_block(then_block, holes);
                if let Some(else_block) = else_block {
                    Self::collect_quote_expr_holes_block(else_block, holes);
                }
            }
            Expr::Match { scrutinee, arms } => {
                Self::collect_quote_expr_holes(scrutinee, holes);
                for arm in arms {
                    if let Some(guard) = &mut arm.guard {
                        Self::collect_quote_expr_holes(guard, holes);
                    }
                    Self::collect_quote_expr_holes(&mut arm.body, holes);
                }
            }
            Expr::While { cond, body } => {
                Self::collect_quote_expr_holes(cond, holes);
                Self::collect_quote_expr_holes_block(body, holes);
            }
            Expr::For { iter, body, .. } => {
                Self::collect_quote_expr_holes(iter, holes);
                Self::collect_quote_expr_holes_block(body, holes);
            }
            Expr::Range { lo, hi, .. } => {
                Self::collect_quote_expr_holes(lo, holes);
                Self::collect_quote_expr_holes(hi, holes);
            }
            Expr::Index { base, index } => {
                Self::collect_quote_expr_holes(base, holes);
                Self::collect_quote_expr_holes(index, holes);
            }
            Expr::WhileLet { scrutinee, body, .. } => {
                Self::collect_quote_expr_holes(scrutinee, holes);
                Self::collect_quote_expr_holes_block(body, holes);
            }
        }
    }

    fn collect_quote_expr_holes_block(block: &mut Block, holes: &mut Vec<Expr>) {
        for stmt in &mut block.stmts {
            Self::collect_quote_expr_holes_stmt(stmt, holes);
        }
    }

    fn collect_quote_expr_holes_stmt(stmt: &mut Stmt, holes: &mut Vec<Expr>) {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => Self::collect_quote_expr_holes(value, holes),
            Stmt::Return(Some(value)) => Self::collect_quote_expr_holes(value, holes),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }

    fn collect_quote_expr_holes_item(item: &mut Item, holes: &mut Vec<Expr>) {
        match item {
            Item::Function(func) => Self::collect_quote_expr_holes_function(func, holes),
            Item::Trait(trait_def) => {
                for method in &mut trait_def.methods {
                    Self::collect_quote_expr_holes_params(&mut method.params, holes);
                    if let Some(default) = &mut method.default {
                        Self::collect_quote_expr_holes_block(default, holes);
                    }
                }
            }
            Item::Impl(impl_def) => {
                for method in &mut impl_def.methods {
                    Self::collect_quote_expr_holes_function(method, holes);
                }
            }
            Item::Const { value, .. } => Self::collect_quote_expr_holes(value, holes),
            Item::Comptime(block) => Self::collect_quote_expr_holes_block(block, holes),
            Item::Type(_) | Item::TypeAlias { .. } => {}
        }
    }

    fn collect_quote_expr_holes_function(func: &mut Function, holes: &mut Vec<Expr>) {
        Self::collect_quote_expr_holes_params(&mut func.params, holes);
        Self::collect_quote_expr_holes_block(&mut func.body, holes);
    }

    fn collect_quote_expr_holes_params(params: &mut [Param], holes: &mut Vec<Expr>) {
        for param in params {
            if let Some(default) = &mut param.default {
                Self::collect_quote_expr_holes(default, holes);
            }
        }
    }

    fn meta_call(&self, name: &str, args: Vec<Expr>) -> Expr {
        Expr::Call { name: format!("meta.{name}"), args }
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
    /// It desugars to a value of a shape-keyed generic synthetic record, registered
    /// so `module()` emits its `derive(Reflect)` definition. The record is
    /// constructed by named field, so its field order is irrelevant; the synthetic
    /// type dedups by the sorted field-name set.
    fn anon_record(&mut self) -> Result<Expr, ParseError> {
        let mut fields = Vec::new();
        let mut spread = None;
        while !self.at(&Tok::RBrace) {
            if self.eat(&Tok::DotDot) {
                spread = Some(Box::new(self.expr(0)?));
                break;
            }
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
        if spread.is_none() && !self.anon_records.contains(&names) {
            self.anon_records.push(names.clone());
        }
        Ok(Expr::Record { name: anon_record_type_name(&names), fields, spread })
    }

    /// `.{ field: Type, … }` in type position. This is the type-level mirror of
    /// anonymous records in value position: it names an instantiation of the same
    /// shape-keyed synthetic generic record, so aliases/signatures/fields all use
    /// the existing nominal machinery after parsing.
    fn anon_record_type(&mut self) -> Result<Type, ParseError> {
        let mut fields = Vec::new();
        let mut seen = HashSet::default();
        while !self.at(&Tok::RBrace) {
            let field = self.ident()?;
            if !seen.insert(field.clone()) {
                return Err(self.error(format!(
                    "field `{field}` is declared more than once in anonymous record type"
                )));
            }
            self.expect(&Tok::Colon)?;
            fields.push((field, self.ty()?));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RBrace)?;
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        let names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
        if !self.anon_records.contains(&names) {
            self.anon_records.push(names.clone());
        }
        Ok(Type::Named(
            anon_record_type_name(&names),
            fields.into_iter().map(|(_, ty)| ty).collect(),
        ))
    }

    /// `.[ Tag | Tag(Payload, …) ]` in type position. The synthetic head encodes
    /// the closed tag set and per-tag arity; payload types stay as ordinary type
    /// arguments in the same canonical tag order.
    fn anon_union_type(&mut self) -> Result<Type, ParseError> {
        if self.at(&Tok::RBracket) {
            return Err(self.error("anonymous union type must contain at least one tag"));
        }
        let mut variants: Vec<(String, Vec<Type>)> = Vec::new();
        let mut seen = HashSet::default();
        while !self.at(&Tok::RBracket) {
            let tag = self.ident()?;
            if !tag.chars().next().is_some_and(|c| c.is_uppercase()) {
                return Err(self.error(format!(
                    "anonymous union tag `{tag}` must start with an uppercase letter"
                )));
            }
            if !seen.insert(tag.clone()) {
                return Err(self.error(format!(
                    "anonymous union tag `{tag}` is listed more than once"
                )));
            }
            let mut payloads = Vec::new();
            if self.eat(&Tok::LParen) {
                if self.at(&Tok::RParen) {
                    return Err(self.error(format!(
                        "anonymous union tag `{tag}` has empty payload parens; use bare `{tag}`"
                    )));
                }
                while !self.at(&Tok::RParen) {
                    payloads.push(self.ty()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RParen)?;
            }
            variants.push((tag, payloads));
            if !self.eat(&Tok::Bar) {
                break;
            }
        }
        self.expect(&Tok::RBracket)?;
        variants.sort_by(|a, b| a.0.cmp(&b.0));
        let shape: Vec<(String, usize)> =
            variants.iter().map(|(tag, payloads)| (tag.clone(), payloads.len())).collect();
        let args = variants.into_iter().flat_map(|(_, payloads)| payloads).collect();
        Ok(Type::Named(anon_union_type_name(&shape), args))
    }

    fn anon_union_injection(&mut self) -> Result<Expr, ParseError> {
        self.expect(&Tok::Dot)?;
        let tag = self.ident()?;
        if !tag.chars().next().is_some_and(|c| c.is_uppercase()) {
            return Err(self.error(format!(
                "anonymous union tag `.{tag}` must start with an uppercase letter"
            )));
        }
        let args = if self.at(&Tok::LParen) && self.on_same_line_as_prev() {
            let args = self.call_args_labeled()?;
            if args.iter().any(|(label, _)| label.is_some()) {
                return Err(self.error(format!(
                    "anonymous union injection `.{tag}(...)` takes positional payloads, not labels"
                )));
            }
            unlabel(args)
        } else {
            Vec::new()
        };
        Ok(Expr::AnonCtor { tag, args })
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
            let args = self.call_args_labeled()?;
            if is_ctor {
                // Constructors take positional args here (uppercase named-field
                // construction went through `record_literal` above); a stray label
                // on a positional ctor call is not meaningful.
                if args.iter().any(|(l, _)| l.is_some()) {
                    return Err(self.error(format!(
                        "`{name}(...)` is a constructor call — use named-field \
                         construction `{name}(field: value, ...)` for a record type, not \
                         labeled positional arguments"
                    )));
                }
                Ok(Expr::Ctor { name, args: unlabel(args) })
            } else if args.iter().any(|(l, _)| l.is_some()) {
                // A DIRECT free call with keyword labels (RFC-0056).
                Ok(Expr::LabeledCall { name, args })
            } else {
                Ok(Expr::Call { name, args: unlabel(args) })
            }
        } else if is_ctor {
            Ok(Expr::Ctor { name, args: vec![] })
        } else {
            Ok(Expr::Var(name))
        }
    }

    /// Parse a call's argument list, allowing RFC-0056 keyword labels: an
    /// argument is either positional (`expr`) or labeled (`ident: expr`). A
    /// positional argument after a labeled one is a parse error (rule 2: positional
    /// prefix, labeled suffix). The caller decides whether labels are meaningful
    /// for the callee shape (direct call vs value/method call). `ident: expr` is
    /// unambiguous inside call parens: lambdas begin with `fn`, there is no ternary
    /// or slice colon, and dict/record colons live in braces / `.{…}`.
    fn call_args_labeled(&mut self) -> Result<Vec<(Option<String>, Expr)>, ParseError> {
        self.expect(&Tok::LParen)?;
        let mut args: Vec<(Option<String>, Expr)> = Vec::new();
        let mut seen_label = false;
        while !self.at(&Tok::RParen) {
            // A label is a bare identifier immediately followed by `:`.
            let is_label = matches!(self.kind(), Tok::Ident(_))
                && matches!(self.toks.get(self.pos + 1).map(|t| &t.kind), Some(Tok::Colon));
            if is_label {
                let label = self.ident()?;
                self.expect(&Tok::Colon)?;
                args.push((Some(label), self.expr(0)?));
                seen_label = true;
            } else {
                if seen_label {
                    return Err(self.error(
                        "a positional argument may not follow a labeled one — labeled \
                         arguments must come last (RFC-0056)",
                    ));
                }
                args.push((None, self.expr(0)?));
            }
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect_call_close()?;
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
    /// (if c) (for y in ys { ... acc = @list_push(acc, elem) }) }; acc }`. The clauses
    /// nest in source order, so later generators see earlier loop variables.
    fn list_comprehension(&mut self, elem: Expr) -> Result<Expr, ParseError> {
        enum Clause {
            For(Pattern, Expr),
            If(Expr),
        }
        let mut clauses = Vec::new();
        loop {
            if self.eat(&Tok::For) {
                // (RFC-0052) The generator takes the SAME pattern grammar as `for`
                // — `[a + b for (a, b) in pairs]` now works — parsed via the shared
                // `for_pattern`, so the comprehension accepts exactly what `for`
                // does by construction.
                let pattern = self.for_pattern()?;
                self.expect(&Tok::In)?;
                clauses.push(Clause::For(pattern, self.expr(0)?));
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
                name: crate::intrinsics::GENERATED_LIST_PUSH.to_string(),
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
                Clause::For(pattern, iter) => {
                    // A plain variable binds directly; any other (irrefutable)
                    // pattern desugars to a fresh loop var + a leading
                    // `let PAT = elem` in the loop body — the same shape `for`
                    // produces (and the refutability checker vets the pattern).
                    if let Pattern::Var(name) = pattern {
                        Stmt::Expr(Expr::For { var: name, iter: Box::new(iter), body })
                    } else {
                        let v = format!("__fortuple{}", self.compr_counter);
                        self.compr_counter += 1;
                        let mut body = body;
                        body.stmts.insert(0, Stmt::LetPattern { pattern, value: Expr::Var(v.clone()) });
                        body.lines.insert(0, 0);
                        Stmt::Expr(Expr::For { var: v, iter: Box::new(iter), body })
                    }
                }
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
                    MatchArm { line: 0, pattern, guard: None, body: Expr::Block(then_block) },
                    MatchArm { line: 0, pattern: Pattern::Wildcard, guard: None, body: fallback },
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
            // ONE grammar (RFC-0052): `pattern()` now folds `|` alternatives into
            // a real `Pattern::Or` and `..`/`..=` into a real `Pattern::IntRange`
            // at any depth — no parse-time arm duplication, no synthesized range
            // guard (the checker/backends reason about both nodes directly).
            let line = self.cur().line;
            let pattern = self.pattern()?;
            let guard = if self.eat(&Tok::If) {
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
            arms.push(MatchArm { line, pattern, guard, body });
            self.eat(&Tok::Comma); // optional separator
        }
        self.expect(&Tok::RBrace)?;
        Ok(Expr::Match {
            scrutinee: Box::new(scrutinee),
            arms,
        })
    }

    /// An integer bound in a range pattern, allowing a leading `-`.
    fn int_bound(&mut self) -> Result<i64, ParseError> {
        let neg = self.eat(&Tok::Minus);
        match self.kind().clone() {
            Tok::Int(n) => {
                self.advance();
                Ok(if neg { n.wrapping_neg() } else { n })
            }
            other => Err(self.error(format!(
                "expected an integer bound in a range pattern, found `{other}`"
            ))),
        }
    }

    /// A `for`/comprehension loop-header pattern. Identical to `pattern()`, except
    /// it also accepts the brace-free comma form `for a, b in xs` (no parens) —
    /// unparenthesized comma-separated patterns gather into a tuple. Used by BOTH
    /// `for` and the comprehension generator, so they accept the same headers.
    fn for_pattern(&mut self) -> Result<Pattern, ParseError> {
        let first = self.pattern()?;
        if !self.at(&Tok::Comma) {
            return Ok(first);
        }
        let mut pats = vec![first];
        while self.eat(&Tok::Comma) {
            // Trailing comma before `in` — stop.
            if self.at(&Tok::In) {
                break;
            }
            pats.push(self.pattern()?);
        }
        Ok(Pattern::Tuple(pats))
    }

    fn pattern(&mut self) -> Result<Pattern, ParseError> {
        // Patterns nest (`Some(Some(Some(…)))`, tuple/list patterns) through
        // `pattern`→`pattern_primary`→`pattern`, so bound this recursion too.
        self.depth += 1;
        self.check_depth()?;
        let out = self.pattern_inner();
        self.depth -= 1;
        out
    }

    fn pattern_inner(&mut self) -> Result<Pattern, ParseError> {
        // One grammar for every binding position (RFC-0052): parse a primary
        // pattern, then fold trailing `| alt` alternatives into an or-pattern and
        // trailing `..`/`..=` into a range. Or-patterns and ranges are real AST
        // nodes usable at ANY depth (`Some(1 | 2)`, `(0..10, _)`).
        let first = self.pattern_primary()?;
        // Integer range: `lo..hi` / `lo..=hi`. Only an Int (or `-Int`) primary can
        // start a range; the primary returns `Pattern::Int` for both.
        if let Pattern::Int(lo) = first {
            let inclusive = self.at(&Tok::DotDotEq);
            if inclusive || self.at(&Tok::DotDot) {
                self.advance();
                let hi = self.int_bound()?;
                return Ok(Pattern::IntRange { lo, hi, inclusive });
            }
        }
        if !self.at(&Tok::Bar) {
            return Ok(first);
        }
        let mut alts = vec![first];
        while self.eat(&Tok::Bar) {
            alts.push(self.pattern_primary()?);
        }
        Ok(Pattern::Or(alts))
    }

    /// A single pattern with no trailing `|` alternative or `..` range (those are
    /// folded by `pattern`). This is the former `pattern` body plus Float-literal
    /// rejection and Duration-literal admission (RFC-0052).
    fn pattern_primary(&mut self) -> Result<Pattern, ParseError> {
        match self.kind().clone() {
            Tok::QuoteHoleStart => self.quote_pattern_hole(),
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
            // (RFC-0052) A duration literal pattern (`1s`), carried as whole ms —
            // exact i64 equality, no float hazard.
            Tok::Duration(ms) => {
                self.advance();
                Ok(Pattern::Duration(ms))
            }
            // (RFC-0052) Float literal patterns are rejected — exact Float equality
            // is a precision trap (and Float is not Eq). Bind and guard instead.
            Tok::Float(_) => Err(self.error(
                "Float literals cannot be matched — exact Float equality is a precision \
                 trap; bind and guard instead (`x if math.float_abs(x - 1.5) < eps ->`)",
            )),
            Tok::Minus => {
                // Negative integer/duration literal pattern, e.g. `-1`, `-1s`
                // (RFC-0052: the sign folds into the literal, matching how `-1s`
                // types in expression position).
                self.advance();
                match self.kind().clone() {
                    Tok::Int(n) => {
                        self.advance();
                        Ok(Pattern::Int(n.wrapping_neg()))
                    }
                    Tok::Duration(ms) => {
                        self.advance();
                        Ok(Pattern::Duration(ms.wrapping_neg()))
                    }
                    Tok::Float(_) => Err(self.error(
                        "Float literals cannot be matched — exact Float equality is a \
                         precision trap; bind and guard instead",
                    )),
                    other => Err(self.error(format!(
                        "expected an integer or duration after `-` in a pattern, found `{other}`"
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
            Tok::Dot => {
                self.advance();
                let tag = match self.kind().clone() {
                    Tok::Ident(name) => {
                        if !name.chars().next().is_some_and(|c| c.is_uppercase()) {
                            return Err(self.error(
                                "anonymous union pattern tags must start with an uppercase letter",
                            ));
                        }
                        self.advance();
                        name
                    }
                    other => {
                        return Err(self.error(format!(
                            "expected an anonymous union tag after `.`, found `{other}`"
                        )));
                    }
                };
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
                Ok(Pattern::AnonCtor { tag, args })
            }
            Tok::Ident(name) => {
                self.advance();
                // (RFC-0042) A module-qualified constructor pattern: `iter.Item(x)`.
                // A lowercase first segment (module) followed by `.` and an
                // uppercase segment (constructor) — the linker keeps the canonical
                // `module.Ctor` name. Bare variant names still resolve against the
                // scrutinee's type in the checker (§4), so this qualified form is
                // only needed to disambiguate.
                let mut name = name;
                if self.at(&Tok::Dot)
                    && name.chars().next().is_some_and(|c| c.is_lowercase())
                    && matches!(self.toks.get(self.pos + 1).map(|t| &t.kind),
                        Some(Tok::Ident(n)) if n.chars().next().is_some_and(|c| c.is_uppercase()))
                {
                    self.advance(); // `.`
                    let ctor = self.ident()?;
                    name = format!("{name}.{ctor}");
                }
                let is_ctor = name
                    .rsplit('.')
                    .next()
                    .and_then(|s| s.chars().next())
                    .is_some_and(|c| c.is_uppercase());
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
        // `a..b` (half-open) and `a..=b` (inclusive) ranges bind loosest, so
        // `1..n+1` is `1..(n+1)` and arbitrary Int expressions work.
        DotDot | DotDotEq => (2, 3),
        // `??` (RFC-0048) is the loosest binary operator before ranges, and
        // RIGHT-associative (r_bp < l_bp): `a ?? b ?? c` is `a ?? (b ?? c)`,
        // which is what makes the natural chain type under the strict
        // `Option(T) ?? T -> T` rule (C# and Swift do the same).
        QuestionQuestion => (5, 4),
        OrOr => (6, 7),
        AndAnd => (8, 9),
        EqEq | NotEq | Lt | LtEq | Gt | GtEq => (10, 11),
        // Bitwise ops bind tighter than comparison (so `a & b == c` is
        // `(a & b) == c`) and looser than arithmetic, ordered `|` < `^` < `&` <
        // shifts. `Bar` here is bitwise-or; in pattern position it's an
        // or-pattern separator, consumed by `match_expr` before expressions run.
        Bar => (12, 13),
        Caret => (14, 15),
        Amp => (16, 17),
        Shl | Shr => (18, 19),
        Plus | Minus => (20, 21),
        Star | Slash | Percent => (22, 23),
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
        Tok::QuestionQuestion => BinOp::Coalesce,
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
/// private `__set_at` method call (UFCS-resolved to `list`/`dict`), a field becomes a
/// `RecordUpdate`, and nested places (`g[i][j] = v`) recurse outward — every
/// step reassigns a value, so the uniqueness pass keeps it in place. The base
/// must bottom out at a variable.
pub fn desugar_place_assign(place: Expr, value: Expr) -> Result<Stmt, String> {
    match place {
        Expr::Var(name) => Ok(Stmt::Assign { name, value }),
        Expr::Index { base, index } => {
            let new_base = Expr::MethodCall {
                receiver: base.clone(),
                method: "__set_at".to_string(),
                args: vec![*index, value],
            };
            desugar_place_assign(*base, new_base)
        }
        Expr::Field { base, field } => {
            let new_base = Expr::RecordUpdate {
                name: None,
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
fn desugar_for_var(n: usize, name: String, list_var: String, body: Block) -> Expr {
    // `n` is the parser's per-module comprehension counter, so a given `for var`
    // gets a stable `__fvN` within one parse (and the same one on a re-parse of
    // formatted output) — that is what lets `witchy fmt` round-trip the sugar.
    let idx = format!("__fv{n}");
    let elem = || Expr::Index {
        base: Box::new(Expr::Var(list_var.clone())),
        index: Box::new(Expr::Var(idx.clone())),
    };
    let bind = Stmt::Let { name: name.clone(), ty: None, mutable: true, value: elem() };
    // `xs[idx] = name` — desugar_place_assign turns it into `xs.set_at(idx, name)`.
    let writeback = desugar_place_assign(elem(), Expr::Var(name))
        .expect("an index place always desugars");
    // Keep the original body's source lines on the middle statements (the
    // synthetic bind/write-back borrow the body's first/last line) so `witchy
    // fmt` still sees the loop spanning its real extent — otherwise a phantom
    // blank line appears after the re-sugared loop.
    let first_line = body.lines.first().copied().unwrap_or(0);
    let last_line = body.lines.last().copied().unwrap_or(first_line);
    let mut stmts = Vec::with_capacity(body.stmts.len() + 2);
    let mut lines = Vec::with_capacity(body.lines.len() + 2);
    stmts.push(bind);
    lines.push(first_line);
    for (st, ln) in body.stmts.into_iter().zip(
        body.lines.iter().copied().chain(std::iter::repeat(first_line)),
    ) {
        stmts.push(st);
        lines.push(ln);
    }
    stmts.push(writeback);
    lines.push(last_line);
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
        | Stmt::LetPattern { value, .. }
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
        Expr::RecordUpdate { name: _, base, fields } => {
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
/// while i < end (or i <= end) { acc = @list_push(acc, i); i = i + 1 }; acc }`. `hi`
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
                    name: crate::intrinsics::GENERATED_LIST_PUSH.to_string(),
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
        name: crate::intrinsics::LIST_AT.into(),
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

/// Drop the (all-`None`) labels from a positional argument list — used where a
/// callee shape does not carry keyword labels (a constructor, an `Apply` through a
/// value, or a method call). The caller has already verified no label is present.
fn unlabel(args: Vec<(Option<String>, Expr)>) -> Vec<Expr> {
    args.into_iter().map(|(_, e)| e).collect()
}

/// (RFC-0056) Whether `e` is a *closed constant*: a literal (int/float/duration/
/// string/bool), `None`/`[]`, a list/tuple of closed constants, a constructor or
/// named-field record literal whose arguments are all closed constants, or a
/// unary op over one (so `-1` works). Deliberately the smallest useful set — no
/// calls, no variable/parameter references, no module state — which keeps a
/// default splice hygienic (nothing to capture) and evaluation-order-free. This
/// also excludes capability values: a capability cannot be minted from a literal,
/// so it can never be a default.
fn is_closed_const(e: &Expr) -> bool {
    match e {
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_) => true,
        Expr::List(xs) | Expr::Tuple(xs) => xs.iter().all(is_closed_const),
        Expr::Ctor { args, .. } => args.iter().all(is_closed_const),
        Expr::Record { fields, spread: None, .. } => {
            fields.iter().all(|(_, value)| is_closed_const(value))
        }
        Expr::Unary { expr, .. } => is_closed_const(expr),
        _ => false,
    }
}

/// Lower `while let PAT = SCRUT: body` to `while true` over a match whose
/// wildcard arm breaks the loop. A free function for the same reason as
/// [`desugar_range`]: the parser keeps `Expr::WhileLet` for the formatter, and
/// every other consumer lowers it here.
pub fn desugar_while_let(pattern: Pattern, scrutinee: Expr, body: Block) -> Expr {
    let dispatch = Expr::Match {
        scrutinee: Box::new(scrutinee),
        arms: vec![
            MatchArm { line: 0, pattern, guard: None, body: Expr::Block(body) },
            MatchArm {
                line: 0,
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
            | Stmt::LetPattern { value, .. }
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
        | Expr::Ctor { args: xs, .. }
        | Expr::AnonCtor { args: xs, .. } => {
            for x in xs {
                lower_sugar_expr(x);
            }
        }
        // Normally already lowered to `Call`/`Block` by `keyword_args::resolve`
        // during linking; recurse defensively over the argument values.
        Expr::LabeledCall { args, .. } => {
            for (_, x) in args {
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
        Expr::RecordUpdate { name: _, base, fields } => {
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

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
