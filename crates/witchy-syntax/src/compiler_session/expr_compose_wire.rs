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
    Block { body: String },
    If { cond: String, then_body: String, else_body: Option<String> },
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
        _ => return Err("unknown compiler expression composition".into()),
    };
    if !reader.0.is_empty() { return Err("trailing compiler expression data".into()); }
    Ok(value)
}

impl Composition {
    pub fn build(self, mut resolve: impl FnMut(&str) -> Result<crate::ast::Expr, String>,
        mut resolve_type: impl FnMut(&str) -> Result<crate::ast::Type, String>,
        mut resolve_block: impl FnMut(&str) -> Result<crate::ast::Block, String>)
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
            Self::Block { body } => Ok(Expr::Block(resolve_block(&body)?)),
            Self::If { cond, then_body, else_body } => Ok(Expr::If {
                cond: Box::new(resolve(&cond)?),
                then_block: resolve_block(&then_body)?,
                else_block: else_body.as_deref().map(&mut resolve_block).transpose()?,
            }),
        }
    }
}
