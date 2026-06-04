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
    Parser::new(tokens).module()
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
}

impl Parser {
    fn new(toks: Vec<Token>) -> Self {
        Self {
            toks,
            pos: 0,
            in_match_arm: false,
            compr_counter: 0,
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
        while self.at(&Tok::Import) {
            self.advance();
            imports.push(self.ident()?);
        }
        let mut items = Vec::new();
        while !self.at(&Tok::Eof) {
            items.push(self.item()?);
        }
        Ok(Module { imports, items })
    }

    fn item(&mut self) -> Result<Item, ParseError> {
        let public = self.eat(&Tok::Pub);
        if self.at(&Tok::Fn) {
            Ok(Item::Function(self.function(public)?))
        } else if self.at(&Tok::Actor) {
            Ok(Item::Actor(self.actor_def()?))
        } else if self.at(&Tok::Type) {
            Ok(Item::Type(self.type_def()?))
        } else {
            Err(self.error(format!(
                "expected a top-level item (`fn`, `actor`, or `type`), found `{}`",
                self.kind()
            )))
        }
    }

    fn type_def(&mut self) -> Result<TypeDef, ParseError> {
        self.expect(&Tok::Type)?;
        let name = self.ident()?;
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
            Ok(TypeDef {
                name: name.clone(),
                variants: vec![Variant {
                    name,
                    fields: rec_types,
                    field_names: rec_names,
                }],
            })
        } else {
            Ok(TypeDef { name, variants })
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
        let body = self.block()?;
        Ok(Function {
            public,
            name,
            params,
            ret,
            body,
        })
    }

    fn params(&mut self) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();
        while !self.at(&Tok::RParen) {
            let convention = if self.eat(&Tok::Inout) {
                Convention::Inout
            } else if self.eat(&Tok::Sink) {
                Convention::Sink
            } else {
                Convention::Let
            };
            let name = self.ident()?;
            let ty = if self.eat(&Tok::Colon) {
                Some(self.ty()?)
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
        Ok(Block { stmts, lines })
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
                lhs = self.desugar_range(lhs, rhs, op_tok == Tok::DotDotEq);
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
                    // `mod.func(args)` — a module-qualified call (only on a bare
                    // module name; witchy has no methods).
                    let args = self.call_args()?;
                    let modname = match e {
                        Expr::Var(name) => name,
                        _ => {
                            return Err(self.error(
                                "only module-qualified calls like `mod.func(...)` are allowed after `.`",
                            ))
                        }
                    };
                    e = Expr::Call {
                        name: format!("{modname}.{member}"),
                        args,
                    };
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
                let cond = self.expr(0)?;
                let body = self.block()?;
                Ok(Expr::While {
                    cond: Box::new(cond),
                    body,
                })
            }
            Tok::For => {
                self.advance();
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
                // Anonymous function: `fn(params) { body }`.
                self.advance();
                self.expect(&Tok::LParen)?;
                let params = self.params()?;
                self.expect(&Tok::RParen)?;
                let body = self.block()?;
                Ok(Expr::Lambda { params, body })
            }
            Tok::Update => {
                // `update <expr> { field: value, ... }` — a copy with overrides.
                self.advance();
                let base = self.expr(0)?;
                self.expect(&Tok::LBrace)?;
                let mut fields = Vec::new();
                while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
                    let name = self.ident()?;
                    self.expect(&Tok::Colon)?;
                    let value = self.expr(0)?;
                    fields.push((name, value));
                    self.eat(&Tok::Comma); // optional separator
                }
                self.expect(&Tok::RBrace)?;
                Ok(Expr::RecordUpdate {
                    base: Box::new(base),
                    fields,
                })
            }
            Tok::Match => self.match_expr(),
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

    /// Desugar `lo..hi` (half-open) or `lo..=hi` (inclusive) integer ranges into
    /// a block that builds the list: `{ var acc = []; var i = lo; let end = hi;
    /// while i < end (or i <= end) { acc = push(acc, i); i = i + 1 }; acc }`.
    /// `hi` is bound once so it isn't re-evaluated each iteration. Self-contained.
    fn desugar_range(&mut self, lo: Expr, hi: Expr, inclusive: bool) -> Expr {
        let n = self.compr_counter;
        self.compr_counter += 1;
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
                        name: "push".to_string(),
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
        })
    }

    /// Desugar a list comprehension with one or more generators and filters —
    /// `[elem for x in xs (if c)* (for y in ys)* ...]` — into a block that builds
    /// the list with nested loops/conditionals: `{ var acc = []; for x in xs {
    /// (if c) (for y in ys { ... acc = push(acc, elem) }) }; acc }`. The clauses
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
                name: "push".to_string(),
                args: vec![Expr::Var(acc.clone()), elem],
            },
        };
        // Wrap from the innermost clause outward.
        for clause in clauses.into_iter().rev() {
            let body = Block { stmts: vec![inner], lines: vec![0] };
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
        }))
    }

    fn if_expr(&mut self) -> Result<Expr, ParseError> {
        self.expect(&Tok::If)?;
        let cond = self.expr(0)?;
        let then_block = self.block()?;
        let else_block = if self.eat(&Tok::Else) {
            if self.at(&Tok::If) {
                // `else if` chains nest as a block containing one if-expression.
                let line = self.cur().line;
                Some(Block {
                    stmts: vec![Stmt::Expr(self.if_expr()?)],
                    lines: vec![line],
                })
            } else {
                Some(self.block()?)
            }
        } else {
            None
        };
        Ok(Expr::If {
            cond: Box::new(cond),
            then_block,
            else_block,
        })
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
            let mut alternatives = vec![self.pattern()?];
            while self.eat(&Tok::Bar) {
                alternatives.push(self.pattern()?);
            }
            let guard = if self.eat(&Tok::If) {
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
            for (i, pattern) in alternatives.into_iter().enumerate() {
                // Clone the shared guard/body for every alternative but the last.
                if i == last {
                    arms.push(MatchArm { pattern, guard, body });
                    break;
                }
                arms.push(MatchArm {
                    pattern,
                    guard: guard.clone(),
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
        let m = parse_module("fn add(a: Int, b: Int) -> Int { a + b }").unwrap();
        let Item::Function(f) = &m.items[0] else {
            panic!("expected a function");
        };
        assert_eq!(f.name, "add");
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.ret, Some(Type::Named("Int".into(), vec![])));
    }

    #[test]
    fn compound_assignment_desugars() {
        // `x += 2` becomes `x = x + 2`.
        let stmts = fn_body("fn f() { var x = 1  x += 2 }");
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
        let stmts = fn_body(r#"fn f(n: Int) -> Int { match n { 1 | 2 | 3 -> 0  _ -> 1 } }"#);
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
        let stmts = fn_body("fn f() { 1 + 2 * 3 }");
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
        let stmts = fn_body("fn f(x: Int) { x |> double() |> add(1) }");
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
        let stmts = fn_body("fn f() { Click(1, foo()) }");
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
            fn describe(e: Event) -> String {
              match e {
                Click(x, _) if x > 0 -> "right"
                Closed -> "bye"
                _ -> "other"
              }
            }
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
            "fn f(p: (Int, Int)) -> Int {\n  match p {\n    (a, b) -> a\n    (x, y) -> y\n  }\n}",
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
        let stmts = fn_body("fn f(n: Int) -> Int {\n  match n {\n    -1 -> 0\n    -2 -> 0\n    _ -> 1\n  }\n}");
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
        let stmts = fn_body("fn f(n: Int) -> Int { match n { 0 -> n - 1  _ -> n } }");
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
            actor Counter {
              console: Console
              var count: Int = 0
              on Inc(by: Int) {
                count = count + by
              }
            }
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
        let m = parse_module("fn f(inout a: Int, sink b: Int, c: Int) -> Int { c }").unwrap();
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
            let m = parse_module("fn main() { let a = spawn Logger(x) send(a, Log(\"hi\")) }").unwrap();
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
