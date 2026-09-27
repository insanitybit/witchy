//! Bounded data protocol for composing current-session expression handles.
//! No guest bytes are parsed as Witchy source.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Composition {
    Apply { callee: String, args: Vec<String> },
    Call { name: String, args: Vec<String> },
    Field { name: String, base: String },
    Record { name: String, fields: Vec<(String, String)> },
    TypedRecord { target: String, fields: Vec<(String, String)> },
    Try { value: String },
    Await { value: String },
    Unary { op: String, value: String },
    Range { lo: String, hi: String, inclusive: bool },
    Binary { op: String, lhs: String, rhs: String },
    Index { base: String, index: String },
    List { elements: Vec<String> },
    Tuple { elements: Vec<String> },
    Block { body: String },
    If { cond: String, then_body: String, else_body: Option<String> },
    For { binding: String, iter: String, body: String },
    While { cond: String, body: String },
    WhileLet { pattern: String, scrutinee: String, body: String },
    Match { scrutinee: String, arms: Vec<String> },
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn line(&mut self) -> Result<&str, String> {
        let end = self.0.iter().position(|byte| *byte == b'\n')
            .ok_or("malformed compiler expression data")?;
        let value = std::str::from_utf8(&self.0[..end])
            .map_err(|_| "compiler expression data is not UTF-8")?;
        self.0 = &self.0[end + 1..];
        Ok(value)
    }

    fn count(&mut self) -> Result<usize, String> {
        let count = self.line()?.parse::<usize>()
            .map_err(|_| "invalid compiler expression count")?;
        if count > 256 { return Err("compiler expression child limit exceeded".into()); }
        Ok(count)
    }

    fn handle(&mut self) -> Result<String, String> {
        let len = self.count()?;
        if len == 0 || len > self.0.len() {
            return Err("truncated compiler expression handle".into());
        }
        let handle = std::str::from_utf8(&self.0[..len])
            .map_err(|_| "compiler expression handle is not UTF-8")?;
        self.0 = &self.0[len..];
        Ok(handle.to_owned())
    }

    fn identifier(&mut self) -> Result<String, String> {
        let name = self.line()?;
        let mut bytes = name.bytes();
        if !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || name == "_" || name.contains("__") || name.len() > 1024 {
            return Err("invalid compiler expression identifier".into());
        }
        Ok(name.to_owned())
    }

    fn args(&mut self) -> Result<Vec<String>, String> {
        let count = self.count()?;
        (0..count).map(|_| self.handle()).collect()
    }
}

pub fn decode(bytes: &[u8]) -> Result<Composition, String> {
    if bytes.len() > 65_536 { return Err("compiler expression input limit exceeded".into()); }
    let mut reader = Reader(bytes);
    let value = match reader.line()? {
        "A" => Composition::Apply { callee: reader.handle()?, args: reader.args()? },
        "C" => Composition::Call { name: reader.identifier()?, args: reader.args()? },
        "F" => Composition::Field { name: reader.identifier()?, base: reader.handle()? },
        "R" => {
            let name = reader.identifier()?;
            let count = reader.count()?;
            let mut seen = std::collections::BTreeSet::new();
            let mut fields = Vec::with_capacity(count);
            for _ in 0..count {
                let field = reader.identifier()?;
                if !seen.insert(field.clone()) { return Err("duplicate compiler record field".into()); }
                fields.push((field, reader.handle()?));
            }
            Composition::Record { name, fields }
        }
        "Q" => {
            let target = reader.handle()?;
            let count = reader.count()?;
            let mut seen = std::collections::BTreeSet::new();
            let mut fields = Vec::with_capacity(count);
            for _ in 0..count {
                let field = reader.identifier()?;
                if !seen.insert(field.clone()) { return Err("duplicate compiler record field".into()); }
                fields.push((field, reader.handle()?));
            }
            Composition::TypedRecord { target, fields }
        }
        "T" => Composition::Try { value: reader.handle()? },
        "W" => Composition::Await { value: reader.handle()? },
        "U" => Composition::Unary { op: reader.line()?.to_owned(), value: reader.handle()? },
        "G" => {
            let inclusive = match reader.line()? {
                "0" => false, "1" => true,
                _ => return Err("invalid compiler range inclusive marker".into()),
            };
            Composition::Range { lo: reader.handle()?, hi: reader.handle()?, inclusive }
        }
        "Y" => Composition::Binary { op: reader.line()?.to_owned(), lhs: reader.handle()?, rhs: reader.handle()? },
        "K" => Composition::Index { base: reader.handle()?, index: reader.handle()? },
        "L" => Composition::List { elements: reader.args()? },
        "P" => Composition::Tuple { elements: reader.args()? },
        "B" => Composition::Block { body: reader.handle()? },
        "I" => {
            let cond = reader.handle()?;
            let then_body = reader.handle()?;
            let else_body = match reader.line()? {
                "0" => None,
                "1" => Some(reader.handle()?),
                _ => return Err("invalid compiler if else marker".into()),
            };
            Composition::If { cond, then_body, else_body }
        }
        "O" => Composition::For { binding: reader.identifier()?, iter: reader.handle()?, body: reader.handle()? },
        "H" => Composition::While { cond: reader.handle()?, body: reader.handle()? },
        "Z" => Composition::WhileLet { pattern: reader.handle()?, scrutinee: reader.handle()?, body: reader.handle()? },
        "M" => Composition::Match { scrutinee: reader.handle()?, arms: reader.args()? },
        _ => return Err("unknown compiler expression composition".into()),
    };
    if !reader.0.is_empty() { return Err("trailing compiler expression data".into()); }
    Ok(value)
}

impl Composition {
    pub fn build(self, mut resolve: impl FnMut(&str) -> Result<crate::ast::Expr, String>,
        mut resolve_type: impl FnMut(&str) -> Result<crate::ast::Type, String>,
        mut resolve_block: impl FnMut(&str) -> Result<crate::ast::Block, String>,
        mut resolve_arm: impl FnMut(&str) -> Result<crate::ast::MatchArm, String>,
        mut resolve_pattern: impl FnMut(&str) -> Result<crate::ast::Pattern, String>)
        -> Result<crate::ast::Expr, String> {
        use crate::ast::Expr;
        fn arguments(handles: Vec<String>, resolve: &mut impl FnMut(&str) -> Result<Expr, String>)
            -> Result<Vec<Expr>, String> {
            handles.iter().map(|handle| resolve(handle)).collect()
        }
        match self {
            Self::Apply { callee, args } => {
                let callee = resolve(&callee)?;
                let args = arguments(args, &mut resolve)?;
                Ok(match callee {
                    Expr::Ctor { name, args: existing } if existing.is_empty() => Expr::Ctor { name, args },
                    callee => Expr::Apply { func: Box::new(callee), args },
                })
            }
            Self::Call { name, args } => Ok(Expr::Call { name, args: arguments(args, &mut resolve)? }),
            Self::Field { name, base } => Ok(Expr::Field { base: Box::new(resolve(&base)?), field: name }),
            Self::Record { name, fields } => {
                let fields = fields.into_iter().map(|(field, handle)|
                    Ok((field, resolve(&handle)?))).collect::<Result<_, String>>()?;
                Ok(Expr::Record { name, fields, spread: None })
            }
            Self::TypedRecord { target, fields } => {
                let crate::ast::Type::Named(name, args) = resolve_type(&target)? else {
                    return Err("compiler record target must name a type".into());
                };
                if !args.is_empty() { return Err("compiler record target cannot have type arguments".into()); }
                let fields = fields.into_iter().map(|(field, handle)|
                    Ok((field, resolve(&handle)?))).collect::<Result<_, String>>()?;
                Ok(Expr::Record { name, fields, spread: None })
            }
            Self::Try { value } => Ok(Expr::Try(Box::new(resolve(&value)?))),
            Self::Await { value } => Ok(Expr::Unary {
                op: crate::ast::UnOp::Await, expr: Box::new(resolve(&value)?),
            }),
            Self::Unary { op, value } => {
                let op = match op.as_str() {
                    "-" => crate::ast::UnOp::Neg,
                    "!" => crate::ast::UnOp::Not,
                    "~" => crate::ast::UnOp::BitNot,
                    "&" => crate::ast::UnOp::Borrow,
                    "*" => crate::ast::UnOp::Deref,
                    _ => return Err(format!("unsupported compiler unary operator `{op}`")),
                };
                Ok(Expr::Unary { op, expr: Box::new(resolve(&value)?) })
            }
            Self::Range { lo, hi, inclusive } => Ok(crate::parser::desugar_range(
                resolve(&lo)?, resolve(&hi)?, inclusive,
            )),
            Self::Binary { op, lhs, rhs } => {
                use crate::ast::BinOp;
                let op = match op.as_str() {
                    "+" => BinOp::Add, "-" => BinOp::Sub, "*" => BinOp::Mul,
                    "/" => BinOp::Div, "%" => BinOp::Mod,
                    "==" => BinOp::Eq, "!=" => BinOp::NotEq,
                    "<" => BinOp::Lt, "<=" => BinOp::LtEq,
                    ">" => BinOp::Gt, ">=" => BinOp::GtEq,
                    "&&" => BinOp::And, "||" => BinOp::Or, "??" => BinOp::Coalesce,
                    "&" => BinOp::BitAnd, "|" => BinOp::BitOr, "^" => BinOp::BitXor,
                    "<<" => BinOp::Shl, ">>" => BinOp::Shr,
                    _ => return Err(format!("unsupported compiler binary operator `{op}`")),
                };
                Ok(Expr::Binary { op, lhs: Box::new(resolve(&lhs)?), rhs: Box::new(resolve(&rhs)?) })
            }
            Self::Index { base, index } => Ok(Expr::Index {
                base: Box::new(resolve(&base)?), index: Box::new(resolve(&index)?),
            }),
            Self::List { elements } => Ok(Expr::List(arguments(elements, &mut resolve)?)),
            Self::Tuple { elements } => Ok(Expr::Tuple(arguments(elements, &mut resolve)?)),
            Self::Block { body } => Ok(Expr::Block(resolve_block(&body)?)),
            Self::If { cond, then_body, else_body } => Ok(Expr::If {
                cond: Box::new(resolve(&cond)?),
                then_block: resolve_block(&then_body)?,
                else_block: else_body.as_deref().map(&mut resolve_block).transpose()?,
            }),
            Self::For { binding, iter, body } => Ok(Expr::For {
                var: binding, iter: Box::new(resolve(&iter)?), body: resolve_block(&body)?,
            }),
            Self::While { cond, body } => Ok(Expr::While {
                cond: Box::new(resolve(&cond)?), body: resolve_block(&body)?,
            }),
            Self::WhileLet { pattern, scrutinee, body } => Ok(crate::parser::desugar_while_let(
                resolve_pattern(&pattern)?, resolve(&scrutinee)?, resolve_block(&body)?,
            )),
            Self::Match { scrutinee, arms } => {
                if arms.is_empty() { return Err("compiler match requires an arm".into()); }
                Ok(Expr::Match { scrutinee: Box::new(resolve(&scrutinee)?),
                    arms: arms.iter().map(|handle| resolve_arm(handle)).collect::<Result<Vec<_>, _>>()? })
            }
        }
    }
}
