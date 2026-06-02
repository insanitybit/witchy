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
}

impl Parser {
    fn new(toks: Vec<Token>) -> Self {
        Self { toks, pos: 0 }
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
        let mut items = Vec::new();
        while !self.at(&Tok::Eof) {
            items.push(self.item()?);
        }
        Ok(Module { items })
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
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            let vname = self.ident()?;
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
            variants.push(Variant { name: vname, fields });
            self.eat(&Tok::Comma); // optional separator
        }
        self.expect(&Tok::RBrace)?;
        Ok(TypeDef { name, variants })
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
            && self.toks.get(self.pos + 1).map(|t| &t.kind) == Some(&Tok::Eq)
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
        while !self.at(&Tok::RBrace) && !self.at(&Tok::Eof) {
            stmts.push(self.stmt()?);
        }
        self.expect(&Tok::RBrace)?;
        Ok(Block { stmts })
    }

    fn stmt(&mut self) -> Result<Stmt, ParseError> {
        if self.at(&Tok::Let) || self.at(&Tok::Var) {
            let mutable = self.advance() == Tok::Var;
            let name = self.ident()?;
            self.expect(&Tok::Eq)?;
            let value = self.expr(0)?;
            Ok(Stmt::Let { name, mutable, value })
        } else if self.is_assignment() {
            let name = self.ident()?;
            self.expect(&Tok::Eq)?;
            let value = self.expr(0)?;
            Ok(Stmt::Assign { name, value })
        } else {
            Ok(Stmt::Expr(self.expr(0)?))
        }
    }

    // --- expressions (Pratt) ---

    fn expr(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.prefix()?;
        loop {
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
        self.atom()
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
                let inner = self.expr(0)?;
                self.expect(&Tok::RParen)?;
                Ok(inner)
            }
            Tok::LBracket => self.list(),
            Tok::LBrace => Ok(Expr::Block(self.block()?)),
            Tok::If => self.if_expr(),
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

    /// Resolve a bare name into a variable, call, or constructor.
    fn name_application(&mut self, name: String) -> Result<Expr, ParseError> {
        let is_ctor = name.chars().next().is_some_and(|c| c.is_uppercase());
        if self.at(&Tok::LParen) {
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
        let mut items = Vec::new();
        while !self.at(&Tok::RBracket) {
            items.push(self.expr(0)?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RBracket)?;
        Ok(Expr::List(items))
    }

    fn if_expr(&mut self) -> Result<Expr, ParseError> {
        self.expect(&Tok::If)?;
        let cond = self.expr(0)?;
        let then_block = self.block()?;
        let else_block = if self.eat(&Tok::Else) {
            if self.at(&Tok::If) {
                // `else if` chains nest as a block containing one if-expression.
                Some(Block {
                    stmts: vec![Stmt::Expr(self.if_expr()?)],
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
            let pattern = self.pattern()?;
            let guard = if self.eat(&Tok::If) {
                Some(self.expr(0)?)
            } else {
                None
            };
            self.expect(&Tok::RArrow)?;
            let body = self.expr(0)?;
            arms.push(MatchArm { pattern, guard, body });
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
            Tok::Underscore => {
                self.advance();
                Ok(Pattern::Wildcard)
            }
            Tok::Int(n) => {
                self.advance();
                Ok(Pattern::Int(n))
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
        EqEq | NotEq | Lt | LtEq | Gt | GtEq => (3, 4),
        Plus | Minus | Concat => (5, 6),
        Star | Slash => (7, 8),
        _ => return None,
    })
}

fn bin_op(t: &Tok) -> BinOp {
    match t {
        Tok::Plus => BinOp::Add,
        Tok::Minus => BinOp::Sub,
        Tok::Star => BinOp::Mul,
        Tok::Slash => BinOp::Div,
        Tok::Concat => BinOp::Concat,
        Tok::EqEq => BinOp::Eq,
        Tok::NotEq => BinOp::NotEq,
        Tok::Lt => BinOp::Lt,
        Tok::LtEq => BinOp::LtEq,
        Tok::Gt => BinOp::Gt,
        Tok::GtEq => BinOp::GtEq,
        other => unreachable!("not a binary operator: {other:?}"),
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
