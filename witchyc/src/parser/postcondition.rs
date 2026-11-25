use super::*;

impl Parser {
    fn spec_pattern(&mut self) -> Result<(String, Option<String>), Diagnostic> {
        let variant = self.name()?;
        let binding = if self.symbol("(") {
            let name = self.name()?;
            self.expect(Kind::Symbol(")"))?;
            Some(name)
        } else {
            None
        };
        Ok((variant, binding))
    }

    fn spec_conjunction(&mut self) -> Result<Expr, Diagnostic> {
        let mut clauses = self.spec_clause()?;
        while !self.at(&Kind::Dedent) {
            let next = self.spec_clause()?;
            let span = clauses.span.start..next.span.end;
            clauses = Expr { kind: Expression::Binary("&&", Box::new(clauses), Box::new(next)), span };
        }
        Ok(clauses)
    }

    /// Read a Boolean clause, including its newline or indented arms.
    pub(super) fn spec_clause(&mut self) -> Result<Expr, Diagnostic> {
        self.nested(|p| {
            let start = p.token().span.start;
            let guarded = p.keyword("when");
            let pattern = if guarded {
                let pattern = p.spec_pattern()?;
                p.expect(Kind::Symbol("="))?;
                Some(pattern)
            } else if p.keyword("match") {
                None
            } else {
                let e = p.expression(0)?;
                p.expect(Kind::Newline)?;
                return Ok(e);
            };
            let value = Box::new(p.expression(0)?);
            p.open_block()?;
            let mut arms = Vec::new();
            if let Some((tag, binding)) = pattern {
                arms.push((tag, binding, p.spec_conjunction()?));
            } else {
                while !p.at(&Kind::Dedent) {
                    let (tag, binding) = p.spec_pattern()?;
                    p.open_block()?;
                    arms.push((tag, binding, p.spec_conjunction()?));
                    p.expect(Kind::Dedent)?;
                }
            }
            p.expect(Kind::Dedent)?;
            let span = start..p.tokens[p.cursor - 1].span.end;
            Ok(Expr { kind: Expression::SpecCase { value, arms, guarded }, span })
        })
    }

    pub(super) fn postcondition(&mut self) -> Result<Postcondition, Diagnostic> {
        self.nested(|parser| {
            let start = parser.token().span.start;
            if !parser.keyword("match") {
                let expression = parser.spec_clause()?;
                let span = expression.span.clone();
                return Ok(Postcondition { kind: PostconditionKind::Predicate(expression), span });
            }
            let value = parser.expression(0)?;
            parser.open_block()?;
            let mut arms = Vec::new();
            while !parser.at(&Kind::Dedent) {
                let variant = parser.name()?;
                let binding = if parser.symbol("(") {
                    let binding = parser.name()?;
                    parser.expect(Kind::Symbol(")"))?;
                    Some(binding)
                } else { None };
                parser.open_block()?;
                let mut clauses = Vec::new();
                while !parser.at(&Kind::Dedent) { clauses.push(parser.postcondition()?); }
                parser.expect(Kind::Dedent)?;
                arms.push(PostconditionArm { variant, binding, clauses });
            }
            parser.expect(Kind::Dedent)?;
            Ok(Postcondition { kind: PostconditionKind::Match { value, arms },
                span: start..parser.tokens[parser.cursor - 1].span.end })
        })
    }
}
