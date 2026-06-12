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
    Parser::new(tokens).module()
}

/// Move `on` handlers written in an inherent `impl Actor { ... }` block onto the
/// matching `ActorDef`, so the rest of the compiler sees handlers on the actor
/// regardless of whether they were written inline or in a separate impl block.
/// An impl left with neither methods nor handlers is dropped — and its source
/// line with it, in lockstep, so `item_lines` stays valid for the formatter's
/// comment placement (dropping the lines wholesale silently discarded every
/// comment in any file with an actor + impl pair).
fn merge_actor_impls(items: Vec<Item>, lines: Vec<u32>) -> (Vec<Item>, Vec<u32>) {
    use std::collections::HashSet;
    let mut paired: Vec<(Item, u32)> = if items.len() == lines.len() {
        items.into_iter().zip(lines).collect()
    } else {
        items.into_iter().map(|it| (it, 0)).collect()
    };
    let actors: HashSet<String> = paired
        .iter()
        .filter_map(|(it, _)| match it {
            Item::Actor(a) => Some(a.name.clone()),
            _ => None,
        })
        .collect();
    let mut pulled: Vec<(String, Vec<Handler>)> = Vec::new();
    for (it, _) in &mut paired {
        if let Item::Impl(im) = it {
            if actors.contains(&im.type_name) && !im.handlers.is_empty() {
                pulled.push((im.type_name.clone(), std::mem::take(&mut im.handlers)));
            }
        }
    }
    for (name, handlers) in pulled {
        for (it, _) in &mut paired {
            if let Item::Actor(a) = it {
                if a.name == name {
                    a.handlers.extend(handlers);
                    break;
                }
            }
        }
    }
    paired.retain(
        |(it, _)| !matches!(it, Item::Impl(im) if im.methods.is_empty() && im.handlers.is_empty()),
    );
    paired.into_iter().unzip()
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
    pending_impl_bounds: Vec<(String, String)>,
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
            imports: ["list", "string", "dict", "math", "option", "result"]
                .into_iter()
                .map(String::from)
                .collect(),
            pending_impl_bounds: Vec::new(),
        }
    }

    // --- token cursor helpers ---

    fn cur(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn kind(&self) -> &Tok {
        &self.toks[self.pos].kind
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
        // Imports come first: `import name` — declarations only, no code runs.
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
        let (items, item_lines) = merge_actor_impls(items, item_lines);
        Ok(Module {
            imports,
            items,
            import_lines,
            item_lines,
        })
    }

    fn item(&mut self) -> Result<Item, ParseError> {
        let public = self.eat(&Tok::Pub);
        if self.at(&Tok::Fn) || self.at(&Tok::Gen) {
            Ok(Item::Function(self.function(public)?))
        } else if self.at(&Tok::Actor) {
            Ok(Item::Actor(self.actor_def()?))
        } else if self.at(&Tok::Type) {
            self.type_def()
        } else if self.at(&Tok::Trait) {
            Ok(Item::Trait(self.trait_def()?))
        } else if self.at(&Tok::Impl) {
            Ok(Item::Impl(self.impl_def()?))
        } else if self.at(&Tok::Let) {
            // A module-level constant: `let NAME = EXPR`. Inlined at use sites.
            self.advance();
            let name = self.ident()?;
            self.expect(&Tok::Eq)?;
            let value = self.expr(0)?;
            Ok(Item::Const { name, value })
        } else {
            Err(self.error(format!(
                "expected a top-level item (`fn`, `actor`, `type`, `trait`, `impl`, or `let`), found `{}`",
                self.kind()
            )))
        }
    }

    /// `trait Name { fn m(self, ...) -> Ret  ... }` — method signatures only.
    fn trait_def(&mut self) -> Result<TraitDef, ParseError> {
        self.expect(&Tok::Trait)?;
        let name = self.ident()?;
        self.expect(&Tok::LBrace)?;
        let mut methods = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            methods.push(self.method_sig()?);
        }
        self.expect(&Tok::RBrace)?;
        Ok(TraitDef { name, methods })
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
        let (trait_name, type_name) = if self.eat(&Tok::For) {
            (Some(first), self.ident()?)
        } else {
            (None, first)
        };
        self.expect(&Tok::LBrace)?;
        let mut methods = Vec::new();
        let mut handlers = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            if self.at(&Tok::On) {
                handlers.push(self.handler()?);
            } else {
                methods.push(self.function(false)?);
            }
        }
        self.expect(&Tok::RBrace)?;
        Ok(ImplDef {
            trait_name,
            type_name,
            methods,
            handlers,
        })
    }

    fn type_def(&mut self) -> Result<Item, ParseError> {
        self.expect(&Tok::Type)?;
        let name = self.ident()?;
        // Optional explicit type parameters: `type Pair(a, b):`. The type checker
        // also infers the parameters from the variant field types, so these names
        // are accepted for clarity/documentation and the inferred set is used.
        if self.eat(&Tok::LParen) {
            while !self.at(&Tok::RParen) {
                self.ident()?;
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
                variants: vec![Variant {
                    name,
                    fields: rec_types,
                    field_names: rec_names,
                }],
            }))
        } else {
            Ok(Item::Type(TypeDef { name, variants }))
        }
    }

    fn actor_def(&mut self) -> Result<ActorDef, ParseError> {
        self.expect(&Tok::Actor)?;
        let name = self.ident()?;
        self.expect(&Tok::LBrace)?;
        let mut fields = Vec::new();
        let mut handlers = Vec::new();
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            if self.at(&Tok::On) {
                handlers.push(self.handler()?);
            } else {
                fields.push(self.field()?);
            }
        }
        self.expect(&Tok::RBrace)?;
        Ok(ActorDef {
            name,
            fields,
            handlers,
        })
    }

    fn field(&mut self) -> Result<Field, ParseError> {
        let mutable = self.eat(&Tok::Var);
        let name = self.ident()?;
        self.expect(&Tok::Colon)?;
        let ty = self.ty()?;
        let init = if self.eat(&Tok::Eq) {
            Some(self.expr(0)?)
        } else {
            None
        };
        Ok(Field {
            name,
            ty,
            mutable,
            init,
        })
    }

    fn handler(&mut self) -> Result<Handler, ParseError> {
        self.expect(&Tok::On)?;
        let message = self.ident()?;
        self.expect(&Tok::LParen)?;
        let params = self.params()?;
        self.expect(&Tok::RParen)?;
        let body = self.block()?;
        Ok(Handler {
            message,
            params,
            body,
        })
    }

    fn is_assignment(&self) -> bool {
        matches!(self.kind(), Tok::Ident(_))
            && matches!(
                self.toks.get(self.pos + 1).map(|t| &t.kind),
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
        let body = self.block()?;
        Ok(Function {
            public,
            name,
            params,
            ret,
            body,
            bounds,
            is_gen,
        })
    }

    /// An optional `where a: Trait, b: Trait2` clause after a function signature.
    fn where_clause(&mut self) -> Result<Vec<(String, String)>, ParseError> {
        let mut bounds = Vec::new();
        if self.eat(&Tok::Where) {
            loop {
                let var = self.ident()?;
                self.expect(&Tok::Colon)?;
                let trait_name = self.ident()?;
                bounds.push((var, trait_name));
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
            // `var`/`inout` mutate in place and write back; `own`/`sink` consume
            // (take ownership). `var`/`own` are the preferred spellings;
            // `inout`/`sink` remain as Hylo-style aliases.
            let convention = if self.eat(&Tok::Inout) || self.eat(&Tok::Var) {
                Convention::Inout
            } else if self.eat(&Tok::Sink) || self.eat(&Tok::Own) {
                Convention::Sink
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
                    self.pending_impl_bounds.push((var.clone(), trait_name));
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
        Ok(Block { stmts, lines, restrict: None, region: None })
    }

    fn stmt(&mut self) -> Result<Stmt, ParseError> {
        if self.eat(&Tok::Return) {
            // `return` alone (at a block's end) yields Nil; otherwise a value.
            let value = if self.at(&Tok::RBrace) {
                None
            } else {
                Some(self.expr(0)?)
            };
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
            if self.at(&Tok::LParen) {
                // Tuple destructure: `let (a, b) = e` (bindings are immutable).
                self.advance();
                let mut names = Vec::new();
                while !self.at(&Tok::RParen) {
                    names.push(self.ident()?);
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
            self.expect(&Tok::Eq)?;
            let value = self.expr(0)?;
            Ok(Stmt::Let { name, mutable, value })
        } else if self.is_assignment() {
            let name = self.ident()?;
            // `x op= e` desugars to `x = x op e`; plain `x = e` is unchanged.
            let op = self.advance();
            let rhs = self.expr(0)?;
            let value = match compound_assign_op(&op) {
                Some(binop) => Expr::Binary {
                    op: binop,
                    lhs: Box::new(Expr::Var(name.clone())),
                    rhs: Box::new(rhs),
                },
                None => rhs,
            };
            Ok(Stmt::Assign { name, value })
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
        self.postfix()
    }

    /// Postfix operators `?` (Result/Option propagation) and `.` (field access /
    /// module-qualified call) bind tighter than any prefix or infix operator, so
    /// `f(x)?` is `(f(x))?` and `p.x + 1` is `(p.x) + 1`.
    fn postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.atom()?;
        loop {
            if self.eat(&Tok::Question) {
                e = Expr::Try(Box::new(e));
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
            Tok::True => {
                self.advance();
                Ok(Expr::Bool(true))
            }
            Tok::False => {
                self.advance();
                Ok(Expr::Bool(false))
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
                // `for (k, v) in pairs:` — a tuple pattern destructures each
                // element: sugar for a fresh element variable plus a leading
                // `let (k, v) = element` in the body.
                if self.at(&Tok::LParen) {
                    self.advance();
                    let mut names = Vec::new();
                    loop {
                        names.push(self.ident()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::RParen)?;
                    self.expect(&Tok::In)?;
                    let iter = self.expr(0)?;
                    let mut body = self.block()?;
                    let var = {
                        let v = format!("__fortuple{}", self.compr_counter);
                        self.compr_counter += 1;
                        v
                    };
                    body.stmts.insert(
                        0,
                        Stmt::LetTuple { names, value: Expr::Var(var.clone()) },
                    );
                    if let Some(first) = body.lines.first().copied() {
                        body.lines.insert(0, first);
                    } else {
                        body.lines.push(0);
                    }
                    return Ok(Expr::For { var, iter: Box::new(iter), body });
                }
                let var = self.ident()?;
                self.expect(&Tok::In)?;
                let iter = self.expr(0)?;
                let body = self.block()?;
                Ok(Expr::For {
                    var,
                    iter: Box::new(iter),
                    body,
                })
            }
            Tok::Fn => {
                // Anonymous function. Brace-free single-expression form
                // `fn(params): expr` (used inline inside call parens, where the
                // off-side layout is suppressed), or an indented/`{ }` block body.
                self.advance();
                self.expect(&Tok::LParen)?;
                let params = self.params()?;
                self.expect(&Tok::RParen)?;
                let body = self.colon_or_block()?;
                Ok(Expr::Lambda { params, body })
            }
            Tok::Match => self.match_expr(),
            Tok::Region => self.region_block(),
            Tok::Retain => self.restrict_block(RestrictMode::Retain),
            Tok::Without => self.restrict_block(RestrictMode::Without),
            Tok::Spawn => {
                self.advance();
                let actor = self.ident()?;
                let args = if self.at(&Tok::LParen) {
                    self.call_args()?
                } else {
                    vec![]
                };
                Ok(Expr::Spawn { actor, args })
            }
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
        // In a match-arm body, a `(` that begins a new line is the next arm's
        // tuple pattern, not call arguments for this name. (Arms have no
        // separator; same rule as a leading `-`.)
        let paren_starts_next_arm = self.in_match_arm
            && *self.kind() == Tok::LParen
            && self.cur().line > self.toks[self.pos.saturating_sub(1)].line;
        if self.at(&Tok::LParen) && !paren_starts_next_arm {
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
            let body = Block { stmts: vec![inner], lines: vec![0], restrict: None, region: None };
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
                    mutable: true,
                    value: Expr::List(Vec::new()),
                },
                inner,
                Stmt::Expr(Expr::Var(acc)),
            ],
            lines: vec![0, 0, 0],
            restrict: None,
            region: None,
        }))
    }

    /// Parse a `retain a, b:` / `without a, b:` capability-firewall block. The
    /// keyword is already the current token. The names list may be empty (e.g.
    /// `retain:` drops every capability, fully sandboxing the block); a trailing
    /// comma is allowed. The body is an ordinary indented block; the restriction
    /// rides along on `Block.restrict` and is enforced by the type checker.
    /// `region:` / `region -> Type:` — a user-controlled allocation scope.
    /// The optional type ascribes the block's value (the copy-out shape).
    fn region_block(&mut self) -> Result<Expr, ParseError> {
        self.advance(); // `region`
        let ty = if self.eat(&Tok::RArrow) { Some(self.ty()?) } else { None };
        let mut block = self.block()?;
        block.region = Some(RegionAnn { ty });
        Ok(Expr::Block(block))
    }

    fn restrict_block(&mut self, mode: RestrictMode) -> Result<Expr, ParseError> {
        self.advance(); // `retain` / `without`
        let mut names = Vec::new();
        if !self.at(&Tok::LBrace) {
            loop {
                names.push(self.ident()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
                if self.at(&Tok::LBrace) {
                    break; // trailing comma
                }
            }
        }
        let mut block = self.block()?;
        block.restrict = Some(CapRestrict { mode, names });
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
                restrict: None,
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
                None => Expr::Block(Block { stmts: vec![], lines: vec![], restrict: None, region: None }),
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
                    restrict: None,
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
            let body = self.expr(0)?;
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
        Plus | Minus | Concat => (17, 18),
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
        Tok::Concat => BinOp::Concat,
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
pub(crate) fn desugar_range(lo: Expr, hi: Expr, inclusive: bool) -> Expr {
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
        restrict: None,
        region: None,
    };
    Expr::Block(Block {
        stmts: vec![
            Stmt::Let { name: acc.clone(), mutable: true, value: Expr::List(Vec::new()) },
            Stmt::Let { name: idx.clone(), mutable: true, value: lo },
            Stmt::Let { name: end, mutable: false, value: hi },
            Stmt::Expr(Expr::While { cond: Box::new(lt), body }),
            Stmt::Expr(Expr::Var(acc)),
        ],
        lines: vec![0, 0, 0, 0, 0],
        restrict: None,
        region: None,
    })
}

/// Lower `base[index]` to the call `list.at(base, index)`. A free function for the
/// same reason as [`desugar_range`]: the parser keeps subscripts as
/// `Expr::Index` for the formatter, and every other consumer lowers them here.
pub(crate) fn desugar_index(base: Expr, index: Expr) -> Expr {
    Expr::Call {
        name: "list.at".into(),
        args: vec![base, index],
    }
}

/// Lower `receiver.method(args)` to the call `method(receiver, args)` — exactly
/// what the parser used to build inline. The linker then resolves `method` by
/// the receiver's type just as for any call.
pub(crate) fn desugar_method(receiver: Expr, method: String, args: Vec<Expr>) -> Expr {
    let mut all = Vec::with_capacity(args.len() + 1);
    all.push(receiver);
    all.extend(args);
    Expr::Call { name: method, args: all }
}

/// Replace every `Expr::MethodCall` in a module with its `desugar_method`
/// lowering. The linker runs this before resolving names, so name resolution and
/// every later stage see the same plain `Call` the parser used to produce; the
/// formatter, which never links, keeps the node so it can print `r.m(args)`.
pub(crate) fn lower_methods_module(m: &mut Module) {
    for item in &mut m.items {
        match item {
            Item::Function(f) => lower_methods_block(&mut f.body),
            Item::Actor(a) => {
                for field in &mut a.fields {
                    if let Some(init) = &mut field.init {
                        lower_methods_expr(init);
                    }
                }
                for h in &mut a.handlers {
                    lower_methods_block(&mut h.body);
                }
            }
            Item::Impl(im) => {
                for meth in &mut im.methods {
                    lower_methods_block(&mut meth.body);
                }
                for h in &mut im.handlers {
                    lower_methods_block(&mut h.body);
                }
            }
            Item::Const { value, .. } => lower_methods_expr(value),
            Item::Type(_) | Item::Trait(_) | Item::TypeAlias { .. } => {}
        }
    }
}

fn lower_methods_block(b: &mut Block) {
    for stmt in &mut b.stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::LetTuple { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Expr(value)
            | Stmt::Yield(value)
            | Stmt::Return(Some(value)) => lower_methods_expr(value),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

fn lower_methods_expr(e: &mut Expr) {
    match e {
        Expr::MethodCall { receiver, method, args } => {
            lower_methods_expr(receiver);
            for a in args.iter_mut() {
                lower_methods_expr(a);
            }
            *e = desugar_method((**receiver).clone(), method.clone(), std::mem::take(args));
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::Var(_) => {}
        Expr::List(xs)
        | Expr::Tuple(xs)
        | Expr::Call { args: xs, .. }
        | Expr::Ctor { args: xs, .. }
        | Expr::Spawn { args: xs, .. } => {
            for x in xs {
                lower_methods_expr(x);
            }
        }
        Expr::Apply { func, args } => {
            lower_methods_expr(func);
            for a in args {
                lower_methods_expr(a);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::Field { base: expr, .. } => lower_methods_expr(expr),
        Expr::Index { base, index } => {
            lower_methods_expr(base);
            lower_methods_expr(index);
        }
        Expr::Range { lo, hi, .. } => {
            lower_methods_expr(lo);
            lower_methods_expr(hi);
        }
        Expr::RecordUpdate { base, fields } => {
            lower_methods_expr(base);
            for (_, v) in fields {
                lower_methods_expr(v);
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                lower_methods_expr(v);
            }
            if let Some(s) = spread {
                lower_methods_expr(s);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            lower_methods_expr(lhs);
            lower_methods_expr(rhs);
        }
        Expr::If { cond, then_block, else_block } => {
            lower_methods_expr(cond);
            lower_methods_block(then_block);
            if let Some(b) = else_block {
                lower_methods_block(b);
            }
        }
        Expr::While { cond, body } => {
            lower_methods_expr(cond);
            lower_methods_block(body);
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            lower_methods_expr(scrutinee);
            lower_methods_block(body);
        }
        Expr::For { iter, body, .. } => {
            lower_methods_expr(iter);
            lower_methods_block(body);
        }
        Expr::Match { scrutinee, arms } => {
            lower_methods_expr(scrutinee);
            for arm in arms.iter_mut() {
                if let Some(g) = &mut arm.guard {
                    lower_methods_expr(g);
                }
                lower_methods_expr(&mut arm.body);
            }
        }
        Expr::Lambda { body, .. } => lower_methods_block(body),
        Expr::Block(b) => lower_methods_block(b),
    }
}

/// Lower `while let PAT = SCRUT: body` to `while true` over a match whose
/// wildcard arm breaks the loop. A free function for the same reason as
/// [`desugar_range`]: the parser keeps `Expr::WhileLet` for the formatter, and
/// every other consumer lowers it here.
pub(crate) fn desugar_while_let(pattern: Pattern, scrutinee: Expr, body: Block) -> Expr {
    let dispatch = Expr::Match {
        scrutinee: Box::new(scrutinee),
        arms: vec![
            MatchArm { pattern, guard: None, body: Expr::Block(body) },
            MatchArm {
                pattern: Pattern::Wildcard,
                guard: None,
                body: Expr::Block(Block { stmts: vec![Stmt::Break], lines: vec![0], restrict: None, region: None }),
            },
        ],
    };
    Expr::While {
        cond: Box::new(Expr::Bool(true)),
        body: Block { stmts: vec![Stmt::Expr(dispatch)], lines: vec![0], restrict: None, region: None },
    }
}

/// Replace every sugar node the parser preserves for the formatter — `Expr::Range`
/// and `Expr::Index` — with its lowering. Codegen runs this once up front so its
/// multiple passes (local collection, then emission) agree on ranges' synthetic
/// loop-variable names and see subscripts as plain `at` calls; the formatter,
/// which never lowers, keeps the nodes so it can print `lo..hi` and `base[i]`.
pub(crate) fn lower_sugar_module(m: &mut Module) {
    for item in &mut m.items {
        match item {
            Item::Function(f) => lower_sugar_block(&mut f.body),
            Item::Actor(a) => lower_sugar_actor(a),
            Item::Impl(im) => {
                for meth in &mut im.methods {
                    lower_sugar_block(&mut meth.body);
                }
                for h in &mut im.handlers {
                    lower_sugar_block(&mut h.body);
                }
            }
            Item::Const { value, .. } => lower_sugar_expr(value),
            Item::Type(_) | Item::Trait(_) | Item::TypeAlias { .. } => {}
        }
    }
}

pub(crate) fn lower_sugar_actor(a: &mut ActorDef) {
    for f in &mut a.fields {
        if let Some(init) = &mut f.init {
            lower_sugar_expr(init);
        }
    }
    for h in &mut a.handlers {
        lower_sugar_block(&mut h.body);
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
        | Expr::Var(_) => {}
        Expr::List(xs)
        | Expr::Tuple(xs)
        | Expr::Call { args: xs, .. }
        | Expr::Ctor { args: xs, .. }
        | Expr::Spawn { args: xs, .. } => {
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
mod tests {
    use super::*;

    fn fn_body(src: &str) -> Vec<Stmt> {
        let m = parse_module(src).expect("should parse");
        match &m.items[0] {
            Item::Function(f) => f.body.stmts.clone(),
            _ => panic!("expected a function"),
        }
    }

    #[test]
    fn parses_function_with_params_and_return() {
        let m = parse_module(r#"
fn add(a: Int, b: Int) -> Int:
    (a + b)
"#).unwrap();
        let Item::Function(f) = &m.items[0] else {
            panic!("expected a function");
        };
        assert_eq!(f.name, "add");
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.ret, Some(Type::Named("Int".into(), vec![])));
    }

    #[test]
    fn impl_trait_param_desugars_to_a_bound() {
        // `fn f(x: impl Show)` becomes a fresh type-var param plus a `Show` bound,
        // so it reuses the whole generics path; two `impl` params get distinct vars.
        let m = parse_module("fn f(x: impl Show, y: impl Ord) -> Int:\n    0\n").unwrap();
        let Item::Function(f) = &m.items[0] else { panic!("expected a function") };
        // Distinct synthetic type vars, in order.
        let p0 = match &f.params[0].ty {
            Some(Type::Named(v, a)) if a.is_empty() => v.clone(),
            other => panic!("expected a type var, got {other:?}"),
        };
        let p1 = match &f.params[1].ty {
            Some(Type::Named(v, a)) if a.is_empty() => v.clone(),
            other => panic!("expected a type var, got {other:?}"),
        };
        assert_ne!(p0, p1, "each impl-Trait param gets its own type variable");
        assert!(f.bounds.contains(&(p0, "Show".to_string())));
        assert!(f.bounds.contains(&(p1, "Ord".to_string())));
        // It coexists with an explicit `where`.
        let m2 = parse_module("fn g(x: impl Show, y: a) -> Int where a: Ord:\n    0\n").unwrap();
        let Item::Function(g) = &m2.items[0] else { panic!() };
        assert!(g.bounds.iter().any(|(_, t)| t == "Show"));
        assert!(g.bounds.contains(&("a".to_string(), "Ord".to_string())));
    }

    #[test]
    fn parses_retain_and_without_firewalls() {
        // `without a, b:` and `retain a:` open an ordinary block carrying a
        // `CapRestrict` on `Block.restrict`.
        let stmts = fn_body(
            "fn main(console: Console, clock: Clock):\n    without clock:\n        print(console, \"x\")\n",
        );
        let Stmt::Expr(Expr::Block(b)) = &stmts[0] else {
            panic!("expected a block statement, got {:?}", stmts[0]);
        };
        assert_eq!(
            b.restrict,
            Some(CapRestrict { mode: RestrictMode::Without, names: vec!["clock".into()] })
        );

        let stmts = fn_body(
            "fn main(console: Console, clock: Clock):\n    retain console, clock:\n        print(console, \"x\")\n",
        );
        let Stmt::Expr(Expr::Block(b)) = &stmts[0] else {
            panic!("expected a block statement");
        };
        assert_eq!(
            b.restrict,
            Some(CapRestrict {
                mode: RestrictMode::Retain,
                names: vec!["console".into(), "clock".into()],
            })
        );

        // `retain:` with no names parses to an empty name list (a full sandbox).
        let stmts =
            fn_body("fn main(console: Console):\n    retain:\n        print(console, \"x\")\n");
        let Stmt::Expr(Expr::Block(b)) = &stmts[0] else {
            panic!("expected a block statement");
        };
        assert_eq!(
            b.restrict,
            Some(CapRestrict { mode: RestrictMode::Retain, names: vec![] })
        );
    }

    #[test]
    fn type_def_accepts_explicit_type_parameters() {
        // The conventional `type Name(a, b):` form parses; the parameter names are
        // accepted for clarity and the checker infers them from the field types.
        let m = parse_module(
            r#"
type Pair(a, b):
    Pair(a, b)
"#,
        )
        .expect("explicit type params should parse");
        let Item::Type(td) = &m.items[0] else {
            panic!("expected a type definition");
        };
        assert_eq!(td.name, "Pair");
        assert_eq!(td.variants.len(), 1);
        assert_eq!(td.variants[0].name, "Pair");
        assert_eq!(td.variants[0].fields.len(), 2);
    }

    #[test]
    fn if_let_desugars_to_match() {
        // `if let PAT = e: ... else: ...` becomes a two-arm match: the pattern
        // arm and a wildcard fallback carrying the else block.
        let stmts = fn_body(
            r#"
fn f(o: Option(Int)) -> Int:
    if let Some(x) = o:
        x
    else:
        0
"#,
        );
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("if let should desugar to a match, got {:?}", stmts[0]);
        };
        assert_eq!(arms.len(), 2);
        assert!(matches!(arms[0].pattern, Pattern::Ctor { .. }));
        assert_eq!(arms[1].pattern, Pattern::Wildcard);
    }

    #[test]
    fn while_let_parses_to_node_and_lowers_to_while_true_match() {
        // `while let PAT = e: body` parses to `Expr::WhileLet` (kept for the
        // formatter) and lowers to `while true` over a match whose wildcard arm
        // breaks the loop.
        let stmts = fn_body(
            r#"
fn f(o: Option(Int)):
    while let Some(x) = o:
        o = None
"#,
        );
        let Stmt::Expr(Expr::WhileLet { pattern, scrutinee, body }) = &stmts[0] else {
            panic!("expected a WhileLet node, got {:?}", stmts[0]);
        };
        assert!(matches!(pattern, Pattern::Ctor { .. }));
        assert_eq!(**scrutinee, Expr::Var("o".into()));
        // Lowering produces the `while true` / match / break form.
        let lowered = desugar_while_let(pattern.clone(), (**scrutinee).clone(), body.clone());
        let Expr::While { cond, body } = &lowered else {
            panic!("while let should lower to a while loop");
        };
        assert_eq!(**cond, Expr::Bool(true));
        let Stmt::Expr(Expr::Match { arms, .. }) = &body.stmts[0] else {
            panic!("while let body should be a match");
        };
        assert_eq!(arms.len(), 2);
        assert_eq!(arms[1].pattern, Pattern::Wildcard);
        let Expr::Block(b) = &arms[1].body else {
            panic!("wildcard arm should be a block");
        };
        assert_eq!(b.stmts, vec![Stmt::Break]);
    }

    #[test]
    fn range_pattern_desugars_to_guarded_binding() {
        // `lo..hi` becomes a fresh binding guarded by `>= lo && < hi`; `..=`
        // uses `<=` for the upper bound.
        let stmts = fn_body(
            r#"
fn f(n: Int) -> Int:
    match n:
        1..=3 -> 0
        _ -> 1
"#,
        );
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("expected a match");
        };
        // First arm: a fresh var bound, with an inclusive bounds guard.
        assert!(matches!(arms[0].pattern, Pattern::Var(_)));
        let Some(Expr::Binary { op: BinOp::And, lhs, rhs }) = &arms[0].guard else {
            panic!("range arm should carry an `&&` bounds guard");
        };
        assert!(matches!(**lhs, Expr::Binary { op: BinOp::GtEq, .. }));
        assert!(matches!(**rhs, Expr::Binary { op: BinOp::LtEq, .. }));
        // The wildcard arm is untouched.
        assert_eq!(arms[1].pattern, Pattern::Wildcard);
        assert!(arms[1].guard.is_none());
    }

    #[test]
    fn subscript_parses_to_index_and_lowers_to_at_call() {
        // `xs[i]` parses to `Expr::Index` (kept for the formatter) and lowers to
        // `list.at(xs, i)`; `grid[r][c]` nests.
        let stmts = fn_body(
            r#"
fn f(xs: List(Int)) -> Int:
    xs[2]
"#,
        );
        let Stmt::Expr(Expr::Index { base, index }) = &stmts[0] else {
            panic!("expected an Index node, got {:?}", stmts[0]);
        };
        assert_eq!(**base, Expr::Var("xs".into()));
        assert_eq!(**index, Expr::Int(2));
        // Lowering turns it into the `at` call the rest of the pipeline expects.
        let lowered = desugar_index((**base).clone(), (**index).clone());
        assert_eq!(
            lowered,
            Expr::Call {
                name: "list.at".into(),
                args: vec![Expr::Var("xs".into()), Expr::Int(2)],
            }
        );
        // `grid[0][1]` nests an Index inside an Index.
        let nested = fn_body(
            r#"
fn g(grid: List(List(Int))) -> Int:
    grid[0][1]
"#,
        );
        let Stmt::Expr(Expr::Index { base, .. }) = &nested[0] else {
            panic!("expected an Index node");
        };
        assert!(matches!(&**base, Expr::Index { .. }));
    }

    #[test]
    fn top_level_let_parses_as_const() {
        let m = parse_module(
            r#"
let MAX = 100
"#,
        )
        .expect("top-level let should parse");
        match &m.items[0] {
            Item::Const { name, value } => {
                assert_eq!(name, "MAX");
                assert_eq!(*value, Expr::Int(100));
            }
            other => panic!("expected a const item, got {other:?}"),
        }
    }

    #[test]
    fn type_alias_parses() {
        let m = parse_module(
            r#"
type Id = Int
"#,
        )
        .expect("type alias should parse");
        match &m.items[0] {
            Item::TypeAlias { name, ty } => {
                assert_eq!(name, "Id");
                assert_eq!(*ty, Type::Named("Int".into(), vec![]));
            }
            other => panic!("expected a type alias, got {other:?}"),
        }
    }

    #[test]
    fn compound_assignment_desugars() {
        // `x += 2` becomes `x = x + 2`.
        let stmts = fn_body(r#"
fn f():
    var x = 1
    x = (x + 2)
"#);
        assert_eq!(
            stmts[1],
            Stmt::Assign {
                name: "x".into(),
                value: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Var("x".into())),
                    rhs: Box::new(Expr::Int(2)),
                },
            }
        );
    }

    #[test]
    fn or_patterns_desugar_to_one_arm_per_alternative() {
        // `1 | 2 | 3 -> body` becomes three arms sharing the body.
        let stmts = fn_body(r#"
fn f(n: Int) -> Int:
    match n:
        1 -> 0
        2 -> 0
        3 -> 0
        _ -> 1
"#);
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("expected a match");
        };
        // 1, 2, 3, _  => 4 arms
        assert_eq!(arms.len(), 4);
        assert_eq!(arms[0].pattern, Pattern::Int(1));
        assert_eq!(arms[1].pattern, Pattern::Int(2));
        assert_eq!(arms[2].pattern, Pattern::Int(3));
        assert_eq!(arms[3].pattern, Pattern::Wildcard);
        // The shared body is duplicated to each alternative.
        assert_eq!(arms[0].body, arms[1].body);
        assert_eq!(arms[1].body, arms[2].body);
    }

    #[test]
    fn respects_operator_precedence() {
        let stmts = fn_body(r#"
fn f():
    (1 + (2 * 3))
"#);
        assert_eq!(
            stmts,
            vec![Stmt::Expr(Expr::Binary {
                op: BinOp::Add,
                lhs: Box::new(Expr::Int(1)),
                rhs: Box::new(Expr::Binary {
                    op: BinOp::Mul,
                    lhs: Box::new(Expr::Int(2)),
                    rhs: Box::new(Expr::Int(3)),
                }),
            })]
        );
    }

    #[test]
    fn desugars_pipeline_into_first_argument() {
        let stmts = fn_body(r#"
fn f(x: Int):
    add(double(x), 1)
"#);
        // x |> double() |> add(1)  ==  add(double(x), 1)
        assert_eq!(
            stmts,
            vec![Stmt::Expr(Expr::Call {
                name: "add".into(),
                args: vec![
                    Expr::Call {
                        name: "double".into(),
                        args: vec![Expr::Var("x".into())],
                    },
                    Expr::Int(1),
                ],
            })]
        );
    }

    #[test]
    fn parses_constructors_vs_calls_by_case() {
        let stmts = fn_body(r#"
fn f():
    Click(1, foo())
"#);
        assert_eq!(
            stmts,
            vec![Stmt::Expr(Expr::Ctor {
                name: "Click".into(),
                args: vec![
                    Expr::Int(1),
                    Expr::Call { name: "foo".into(), args: vec![] },
                ],
            })]
        );
    }

    #[test]
    fn parses_match_with_guard_and_ctor_patterns() {
        let src = r#"
fn describe(e: Event) -> String:
    match e:
        Click(x, _) if (x > 0) -> "right"
        Closed -> "bye"
        _ -> "other"
"#;
        let stmts = fn_body(src);
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("expected a match expression");
        };
        assert_eq!(arms.len(), 3);
        assert!(matches!(arms[0].pattern, Pattern::Ctor { .. }));
        assert!(arms[0].guard.is_some());
        assert!(matches!(arms[2].pattern, Pattern::Wildcard));
    }

    #[test]
    fn tuple_pattern_after_ident_body_parses() {
        // A bare-identifier arm body must not swallow the next arm's `(..)`.
        let stmts = fn_body(
            r#"
fn f(p: (Int, Int)) -> Int:
    match p:
        (a, b) -> a
        (x, y) -> y
"#,
        );
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("expected a match expression");
        };
        assert_eq!(arms.len(), 2);
        assert!(matches!(arms[0].pattern, Pattern::Tuple(_)));
        assert!(matches!(arms[0].body, Expr::Var(_)));
    }

    #[test]
    fn parses_negative_patterns_across_newlines() {
        // The `-2` on the next line is a pattern, not `0 - 2` continuing arm 1.
        let stmts = fn_body(r#"
fn f(n: Int) -> Int:
    match n:
        -1 -> 0
        -2 -> 0
        _ -> 1
"#);
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("expected a match expression");
        };
        assert_eq!(arms.len(), 3);
        assert_eq!(arms[0].pattern, Pattern::Int(-1));
        assert_eq!(arms[1].pattern, Pattern::Int(-2));
    }

    #[test]
    fn subtraction_in_an_arm_body_still_parses() {
        // A `-` on the *same* line is ordinary subtraction.
        let stmts = fn_body(r#"
fn f(n: Int) -> Int:
    match n:
        0 -> (n - 1)
        _ -> n
"#);
        let Stmt::Expr(Expr::Match { arms, .. }) = &stmts[0] else {
            panic!("expected a match expression");
        };
        assert!(matches!(arms[0].body, Expr::Binary { op: BinOp::Sub, .. }));
    }

    #[test]
    fn reports_friendly_error_with_location() {
        let err = parse_module("fn f( {").unwrap_err();
        assert!(err.line >= 1);
        assert!(!err.message.is_empty());
    }

    #[test]
    fn parses_actor_with_fields_handlers_and_assignment() {
        let src = r#"
actor Counter:
    console: Console
    var count: Int = 0

impl Counter:
    on Inc(by: Int):
        count = (count + by)
"#;
        let m = parse_module(src).unwrap();
        let Item::Actor(a) = &m.items[0] else {
            panic!("expected an actor");
        };
        assert_eq!(a.name, "Counter");
        assert_eq!(a.fields.len(), 2);
        assert!(!a.fields[0].mutable && a.fields[0].init.is_none()); // capability field
        assert!(a.fields[1].mutable && a.fields[1].init.is_some()); // var with default
        assert_eq!(a.handlers.len(), 1);
        assert_eq!(a.handlers[0].message, "Inc");
        assert!(matches!(a.handlers[0].body.stmts[0], Stmt::Assign { .. }));
    }

    #[test]
    fn parses_parameter_conventions() {
        let m = parse_module(r#"
fn f(inout a: Int, sink b: Int, c: Int) -> Int:
    c
"#).unwrap();
        let Item::Function(func) = &m.items[0] else {
            panic!("expected a function");
        };
        assert_eq!(func.params[0].convention, Convention::Inout);
        assert_eq!(func.params[1].convention, Convention::Sink);
        assert_eq!(func.params[2].convention, Convention::Let);
    }

    #[test]
    fn parses_spawn_and_send() {
        let stmts = {
            let m = parse_module(r#"
fn main():
    let a = spawn Logger(x)
    send(a, Log("hi"))
"#).unwrap();
            match &m.items[0] {
                Item::Function(f) => f.body.stmts.clone(),
                _ => panic!("expected a function"),
            }
        };
        assert!(matches!(
            stmts[0],
            Stmt::Let {
                value: Expr::Spawn { .. },
                ..
            }
        ));
    }
}
