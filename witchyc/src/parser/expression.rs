use super::*;

impl Parser {
    /// A text literal (`"pen"`): an index into the program's literal table, the same text once.
    pub(super) fn text_literal(&mut self, s: String, span: std::ops::Range<usize>) -> Result<Expression, Diagnostic> {
        // The same text written twice is one literal.
        let index = match self.texts.iter().position(|text| *text == s) {
            Some(index) => index,
            None => {
                if self.texts.len() == text_literal::COUNT {
                    return Err(Diagnostic::new("W0100", span.clone(), "a program holds at most 4096 different text literals"));
                }
                self.texts.push(s);
                self.texts.len() - 1
            }
        };
        let callee = Expr { kind: Expression::Name(text_literal::TYPE.into()), span: span.clone() };
        Ok(Expression::Call(Box::new(callee), vec![Expr { kind: Expression::Integer(index as u64), span: span.clone() }]))
    }
    pub(super) fn expression(&mut self, minimum: u8) -> Result<Expr, Diagnostic> {
        // The expression read here is as deep as the deepest one inside it and its own
        // operators; beside the ones read before it, the deeper counts (`expression_depth`).
        let beside = std::mem::take(&mut self.expression_depth);
        let result = self.nested(|p| p.expression_inner(minimum));
        self.expression_depth = self.expression_depth.max(beside);
        result
    }
    /// One more level on the expression being read. Nesting and a chain are each limited where
    /// they are read, but a chain inside brackets inside a chain multiplied the two: 60 groups
    /// of 100 `+` made a tree 6000 deep, and the checker, which walks it by recursion, ran out
    /// of stack. No expression that is compiled is nearly this deep.
    fn deeper(&mut self, span: &std::ops::Range<usize>) -> Result<(), Diagnostic> {
        self.expression_depth += 1;
        if self.expression_depth > 256 {
            return Err(Diagnostic::new("W0102", span.clone(), "expression nesting limit exceeded: this expression is more than 256 operators and brackets deep; give its parts names with `let`"));
        }
        Ok(())
    }
    fn expression_inner(&mut self, minimum: u8) -> Result<Expr, Diagnostic> {
        let token = self.take().clone();
        let start = token.span.start;
        let kind = match token.kind {
            Kind::Integer(n) => Expression::Integer(n),
            // `30s` is `Duration.from_ms(30000)` (`import time`).
            Kind::Duration(ms) => {
                let at = |kind| Expr { kind, span: token.span.clone() };
                let callee = at(Expression::Field(Box::new(at(Expression::Name("Duration".into()))), "from_ms".into()));
                Expression::Call(Box::new(callee), vec![at(Expression::Integer(ms))])
            }
            Kind::Float(bits) => Expression::Float(bits),
            // `"total ${t}"`: a format value (interpolation.rs).
            Kind::Text(s) if s.contains("${") => self.format_value(s, token.span.clone())?,
            Kind::Text(s) => self.text_literal(s.replace(crate::lexer::ESCAPED_DOLLAR, "$"), token.span.clone())?,
            Kind::Name(s) if s == "source_site" && self.symbol("(") => {
                let mut name = String::new();
                if let Kind::Text(label)=self.token().kind.clone() {
                    if !label.starts_with(|c:char|c.is_ascii_alphabetic() || c=='_') || label.len()>48 || !label.bytes().all(|b| b.is_ascii_alphanumeric() || b==b'_') {
                        return Err(self.error("a source site label is an ASCII identifier of up to 48 bytes"));
                    }
                    self.take(); name.push_str(&label);
                }
                self.expect(Kind::Symbol(")"))?;
                Expression::SourceSite(name)
            }
            Kind::Name(s) if s == "true" => Expression::Bool(true),
            Kind::Name(s) if s == "false" => Expression::Bool(false),
            Kind::Name(s) if s == "forall" => {
                let variable = self.name()?;
                if !self.keyword("in") { return Err(self.error("expected in after quantified variable")); }
                self.expect(Kind::Symbol("("))?;
                let lower = self.expression(0)?;
                self.expect(Kind::Symbol(","))?;
                let upper = self.expression(0)?;
                self.expect(Kind::Symbol(")"))?;
                self.expect(Kind::Symbol(":"))?;
                Expression::Forall { variable, lower: Box::new(lower), upper: Box::new(upper), body: Box::new(self.expression(0)?) }
            }
            // `if c: a else: b`, all on one line.
            Kind::Name(s) if s == "if" => {
                // `if let Some(v) = value: v else: 0`: the condition is that the value matches.
                let condition = if self.keyword("let") {
                    let start = self.token().span.start;
                    let pattern = self.pattern()?;
                    self.expect(Kind::Symbol("="))?;
                    let value = self.expression(0)?;
                    let crate::ast::Pattern { variant, binding, nested, open } = pattern;
                    let arm = crate::ast::MatchArm { variant, binding, body: Vec::new(), nested, open };
                    let end = value.span.end;
                    Expr { kind: Expression::Matches(Box::new(value), Box::new(arm)), span: start..end }
                } else { self.expression(0)? };
                self.expect(Kind::Symbol(":"))?;
                let yes = self.expression(0)?;
                if !self.keyword("else") { return Err(self.error("an `if` used as a value on one line needs `else`: `if c: a else: b`")); }
                self.expect(Kind::Symbol(":"))?;
                let no = self.expression(0)?;
                Expression::Conditional(Box::new(condition), Box::new(yes), Box::new(no))
            }
            // `fn(n: U64) -> U64: n * n`: a closure (closure.rs).
            Kind::Name(s) if s == "fn" && self.at(&Kind::Symbol("(")) => self.closure(token.span.clone())?,
            Kind::Name(s) if s == "move" => {
                Expression::Unary("move", Box::new(self.expression(17)?))
            }
            Kind::Name(s) => match self.constants.get(&s) { Some(value) => value.clone(), None => Expression::Name(s) },
            Kind::Symbol("(") => {
                if self.symbol(")") {
                    Expression::Unit
                } else {
                    let expression = self.expression(0)?;
                    if self.at(&Kind::Symbol(",")) {
                        // `(a, b)`: a tuple, the anonymous record of its values by position
                        // (`.0`, `.1`), as a variant's `.Tag(a, b)` payload is.
                        let mut values = vec![expression];
                        while self.symbol(",") { if self.at(&Kind::Symbol(")")) { break; } values.push(self.expression(0)?); }
                        self.expect(Kind::Symbol(")"))?;
                        if values.len() > 8 { return Err(Diagnostic::new("W0100", token.span.clone(), "a tuple holds at most 8 values: name them in a record")); }
                        Expression::Record(values.into_iter().enumerate().map(|(index, value)| (index.to_string(), value)).collect())
                    } else {
                        self.expect(Kind::Symbol(")"))?;
                        expression.kind
                    }
                }
            }
            Kind::Symbol(".") if matches!(self.token().kind, Kind::Name(_)) => {
                let tag = self.name()?;
                let payload = if self.symbol("(") {
                    let mut value = self.expression(0)?;
                    // `.Tag(a, b)`: the values together, by position.
                    if self.at(&Kind::Symbol(",")) {
                        let span = value.span.clone();
                        let mut values = vec![value];
                        while self.symbol(",") { values.push(self.expression(0)?); }
                        value = Expr { kind: Expression::Record(values.into_iter().enumerate().map(|(index, value)| (index.to_string(), value)).collect()), span };
                    }
                    self.expect(Kind::Symbol(")"))?;
                    Some(Box::new(value))
                } else { None };
                Expression::Inject(tag, payload)
            }
            Kind::Symbol(".") => {
                self.expect(Kind::Symbol("{"))?;
                Expression::Record(self.parameters("}", |parser| {
                    let at = parser.token().span.clone();
                    let label = parser.name()?;
                    // `.{left, right}` is `.{left: left, right: right}`.
                    let value = if parser.symbol(":") { parser.expression(0)? } else { Expr { kind: Expression::Name(label.clone()), span: at } };
                    Ok((label, value))
                })?)
            }
            Kind::Symbol("[") => {
                if self.symbol("]") {
                    Expression::Array(Vec::new())
                } else {
                    let first = self.expression(0)?;
                    if self.symbol(";") {
                        let count = self.integer()?;
                        self.expect(Kind::Symbol("]"))?;
                        Expression::Repeat(Box::new(first), count)
                    } else {
                        let mut values = vec![first];
                        while self.symbol(",") {
                            if self.at(&Kind::Symbol("]")) {
                                break;
                            }
                            values.push(self.expression(0)?);
                        }
                        self.expect(Kind::Symbol("]"))?;
                        Expression::Array(values)
                    }
                }
            }
            Kind::Symbol(op @ ("-" | "!" | "~" | "*" | "&")) => {
                let op = if op == "&" && self.keyword("mut") {
                    "&mut"
                } else {
                    op
                };
                Expression::Unary(op, Box::new(self.expression(17)?))
            }
            _ => {
                // Say what is there instead.
                let found = match &token.kind {
                    Kind::Symbol(symbol) => format!(": found `{symbol}`"),
                    Kind::Newline | Kind::End => ": the line ends here".to_string(),
                    Kind::Indent => ": found an indented block; a line is indented further only under one that opens a block (ending with `:`)".to_string(),
                    Kind::Dedent => ": the block ends here".to_string(),
                    _ => String::new(),
                };
                return Err(Diagnostic::new(
                    "W0101",
                    token.span,
                    format!("expected an expression{found}"),
                ));
            }
        };
        let mut left = Expr {
            kind,
            span: start..self.tokens[self.cursor - 1].span.end,
        };
        self.deeper(&left.span)?;
        let mut operations = 0;
        loop {
            if operations >= 128 {
                return Err(Diagnostic::new(
                    "W0102",
                    left.span,
                    "expression chain limit exceeded",
                ));
            }
            operations += 1;
            // Each pass but the last puts one more operator above what is read so far.
            if operations > 1 { self.deeper(&left.span)?; }
            if minimum <= 19 && matches!(self.token().kind, Kind::Symbol("(" | "." | "[" | "?")) {
                let kind = match self.take().kind.clone() {
                    Kind::Symbol("(") => {
                        // A parameter of a function type, or a local closure: `f(x)` is `f.call(x)`.
                        let callee = match &left.kind {
                            Expression::Name(n) if self.fn_locals.iter().any(|(f, _)| f == n) => Expr { span: left.span.clone(), kind: Expression::Field(Box::new(left), "call".into()) },
                            _ => left,
                        };
                        // `name: value` labels an argument with the parameter it is given to.
                        Expression::Call(Box::new(callee), self.parameters(")", |p| {
                            // `..base`: a record's other fields from `base` (the checker's label pass).
                            if p.at(&Kind::Symbol("..")) {
                                let start = p.token().span.start;
                                p.take();
                                let value = p.expression(0)?;
                                let end = value.span.end;
                                return Ok(Expr { kind: Expression::Labelled("..".into(), Box::new(value)), span: start..end });
                            }
                            if matches!(p.token().kind, Kind::Name(_)) && matches!(p.tokens.get(p.cursor + 1).map(|t| &t.kind), Some(Kind::Symbol(":"))) {
                                let start = p.token().span.start;
                                let label = p.name()?;
                                p.take();
                                let value = p.expression(0)?;
                                let end = value.span.end;
                                return Ok(Expr { kind: Expression::Labelled(label, Box::new(value)), span: start..end });
                            }
                            p.expression(0)
                        })?)
                    }
                    Kind::Symbol(".") => {
                        let at = self.token().span.clone();
                        // `pair.0`: a tuple's value by position.
                        if let Kind::Integer(index) = self.token().kind {
                            self.take();
                            left = Expr { span: left.span.start..at.end, kind: Expression::Field(Box::new(left), index.to_string()) };
                            continue;
                        }
                        if matches!(self.token().kind, Kind::Float(_)) { return Err(Diagnostic::new("W0100", at, "`t.0.1` reads as a decimal number: write `(t.0).1`")); }
                        let field = self.name()?;
                        // `shapes.Box(3, 4)`, `json.JsonInt(1)`: a type or constructor of an
                        // imported module, which is the bare name once checked.
                        match &left.kind {
                            Expression::Name(module) if field.starts_with(|c: char| c.is_ascii_uppercase()) && let Some(file) = self.qualifier(module) => {
                                self.offered(module, file, &field, at)?;
                                Expression::Name(field)
                            }
                            // `self.count` in a method with a view receiver: that field's reference.
                            Expression::Name(receiver) if receiver == "self" && !self.view.is_empty() => {
                                if !self.view.iter().any(|(n, _)| *n == field) {
                                    return Err(Diagnostic::new("W0401", at, format!("this method's receiver borrows only {}: add `&{field}` or `&mut {field}` to `self: Self {{ ... }}`", self.view.iter().map(|(n, m)| format!("`{}{n}`", if *m { "&mut " } else { "&" })).collect::<Vec<_>>().join(", "))));
                                }
                                let reference = Expr { span: left.span.clone(), kind: Expression::Name(format!("self#{field}")) };
                                Expression::Unary("*", Box::new(reference))
                            }
                            _ => Expression::Field(Box::new(left), field),
                        }
                    }
                    Kind::Symbol("[") => {
                        let index = self.expression(0)?;
                        self.expect(Kind::Symbol("]"))?;
                        Expression::Index(Box::new(left), Box::new(index))
                    }
                    Kind::Symbol("?") => Expression::Try(Box::new(left)),
                    _ => unreachable!("postfix token was checked"),
                };
                left = Expr {
                    kind,
                    span: start..self.tokens[self.cursor - 1].span.end,
                };
                continue;
            }
            // `<<` and `>>` are two `<` or `>` with nothing between, so `Vec<Vec<U64>>` still
            // closes two type lists. Looser to tighter, as the book has them: `|`, `^`, `&`, shifts.
            let doubled = |parser: &Self, symbol: &str| matches!(parser.token().kind, Kind::Symbol(s) if s == symbol)
                && parser.tokens.get(parser.cursor + 1).is_some_and(|next| matches!(next.kind, Kind::Symbol(s) if s == symbol) && next.span.start == parser.token().span.end);
            let shift = if doubled(self, "<") { Some("<<") } else if doubled(self, ">") { Some(">>") } else { None };
            let (op, precedence) = match (shift, &self.token().kind) {
                (Some(op), _) => (op, 11),
                (None, Kind::Symbol(op @ "||")) => (*op, 1),
                (None, Kind::Symbol(op @ "&&")) => (*op, 3),
                (None, Kind::Symbol(op @ ("==" | "!="))) => (*op, 5),
                (None, Kind::Symbol(op @ ("<" | "<=" | ">" | ">="))) => (*op, 7),
                (None, Kind::Symbol(op @ "|")) => (*op, 8),
                (None, Kind::Symbol(op @ "^")) => (*op, 9),
                (None, Kind::Symbol(op @ "&")) => (*op, 10),
                (None, Kind::Symbol(op @ ("+" | "-"))) => (*op, 13),
                (None, Kind::Symbol(op @ ("*" | "/" | "%"))) => (*op, 15),
                _ => break,
            };
            if precedence < minimum {
                break;
            }
            self.take();
            if shift.is_some() { self.take(); }
            let right = self.expression(precedence + 1)?;
            let end = right.span.end;
            left = Expr {
                kind: Expression::Binary(op, Box::new(left), Box::new(right)),
                span: start..end,
            };
        }
        Ok(left)
    }
}
