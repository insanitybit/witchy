//! Bounded structural data for statements and blocks built during comptime.
//! Child syntax is always resolved through current-session handles.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatementSpec {
    Let { mutable: bool, name: String, ty: Option<String>, value: String },
    Assign { name: String, value: String },
    Expr(String),
    Return(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSpec {
    pub statements: Vec<String>,
    pub tail: Option<String>,
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn line(&mut self) -> Result<&str, String> {
        let end = self.0.iter().position(|byte| *byte == b'\n')
            .ok_or("malformed compiler block data")?;
        let value = std::str::from_utf8(&self.0[..end])
            .map_err(|_| "compiler block data is not UTF-8")?;
        self.0 = &self.0[end + 1..];
        Ok(value)
    }

    fn count(&mut self) -> Result<usize, String> {
        let count = self.line()?.parse::<usize>()
            .map_err(|_| "invalid compiler block count")?;
        if count > 256 { return Err("compiler block child limit exceeded".into()); }
        Ok(count)
    }

    fn handle(&mut self) -> Result<String, String> {
        let length = self.count()?;
        if length == 0 || length > self.0.len() { return Err("truncated compiler block handle".into()); }
        let handle = std::str::from_utf8(&self.0[..length])
            .map_err(|_| "compiler block handle is not UTF-8")?;
        self.0 = &self.0[length..];
        Ok(handle.to_owned())
    }

    fn optional_handle(&mut self) -> Result<Option<String>, String> {
        match self.line()? { "0" => Ok(None), "1" => self.handle().map(Some),
            _ => Err("invalid compiler block optional marker".into()) }
    }

    fn identifier(&mut self) -> Result<String, String> {
        let name = self.line()?;
        let mut bytes = name.bytes();
        if !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || name == "_" || name.len() > 1024 {
            return Err("invalid compiler block identifier".into());
        }
        Ok(name.to_owned())
    }

    fn boolean(&mut self) -> Result<bool, String> {
        match self.line()? { "0" => Ok(false), "1" => Ok(true),
            _ => Err("invalid compiler block flag".into()) }
    }

    fn end(&self) -> Result<(), String> {
        if self.0.is_empty() { Ok(()) } else { Err("trailing compiler block data".into()) }
    }
}

pub fn decode_statement(bytes: &[u8]) -> Result<StatementSpec, String> {
    if bytes.len() > 65_536 { return Err("compiler statement input limit exceeded".into()); }
    let mut reader = Reader(bytes);
    let spec = match reader.line()? {
        "L" => StatementSpec::Let { mutable: reader.boolean()?, name: reader.identifier()?,
            ty: reader.optional_handle()?, value: reader.handle()? },
        "A" => StatementSpec::Assign { name: reader.identifier()?, value: reader.handle()? },
        "E" => StatementSpec::Expr(reader.handle()?),
        "R" => StatementSpec::Return(reader.handle()?),
        _ => return Err("unknown compiler statement builder".into()),
    };
    reader.end()?;
    Ok(spec)
}

pub fn decode_block(bytes: &[u8]) -> Result<BlockSpec, String> {
    if bytes.len() > 65_536 { return Err("compiler block input limit exceeded".into()); }
    let mut reader = Reader(bytes);
    let count = reader.count()?;
    let statements = (0..count).map(|_| reader.handle()).collect::<Result<Vec<_>, _>>()?;
    let tail = reader.optional_handle()?;
    reader.end()?;
    if statements.is_empty() && tail.is_none() { return Err("compiler block must not be empty".into()); }
    Ok(BlockSpec { statements, tail })
}
