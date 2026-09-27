//! Bounded data frames for typed parameter and expression-body function items.
//! All syntax operands are current-session handles, resolved by the adapter.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamSpec {
    pub convention: String,
    pub name: String,
    pub ty: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSpec {
    pub public: bool,
    pub name: String,
    pub params: Vec<String>,
    pub ret: Option<String>,
    pub body: String,
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn line(&mut self) -> Result<&str, String> {
        let end = self.0.iter().position(|byte| *byte == b'\n')
            .ok_or("malformed compiler function data")?;
        let value = std::str::from_utf8(&self.0[..end])
            .map_err(|_| "compiler function data is not UTF-8")?;
        self.0 = &self.0[end + 1..];
        Ok(value)
    }

    fn count(&mut self) -> Result<usize, String> {
        let count = self.line()?.parse::<usize>()
            .map_err(|_| "invalid compiler function count")?;
        if count > 256 { return Err("compiler function child limit exceeded".into()); }
        Ok(count)
    }

    fn handle(&mut self) -> Result<String, String> {
        let length = self.count()?;
        if length == 0 || length > self.0.len() {
            return Err("truncated compiler function handle".into());
        }
        let handle = std::str::from_utf8(&self.0[..length])
            .map_err(|_| "compiler function handle is not UTF-8")?;
        self.0 = &self.0[length..];
        Ok(handle.to_owned())
    }

    fn identifier(&mut self) -> Result<String, String> {
        let name = self.line()?;
        let mut bytes = name.bytes();
        if !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || name == "_" || name.len() > 1024 {
            return Err("invalid compiler function identifier".into());
        }
        Ok(name.to_owned())
    }

    fn boolean(&mut self) -> Result<bool, String> {
        match self.line()? {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err("invalid compiler function flag".into()),
        }
    }

    fn end(&self) -> Result<(), String> {
        if self.0.is_empty() { Ok(()) } else { Err("trailing compiler function data".into()) }
    }
}

pub fn decode_param(bytes: &[u8]) -> Result<ParamSpec, String> {
    if bytes.len() > 65_536 { return Err("compiler parameter input limit exceeded".into()); }
    let mut reader = Reader(bytes);
    let convention = reader.line()?.to_owned();
    if !matches!(convention.as_str(), "default" | "let" | "var" | "own") {
        return Err("unsupported compiler parameter convention".into());
    }
    let name = reader.identifier()?;
    let ty = reader.handle()?;
    reader.end()?;
    Ok(ParamSpec { convention, name, ty })
}

pub fn decode_function(bytes: &[u8]) -> Result<FunctionSpec, String> {
    if bytes.len() > 65_536 { return Err("compiler function input limit exceeded".into()); }
    let mut reader = Reader(bytes);
    let public = reader.boolean()?;
    let name = reader.identifier()?;
    let count = reader.count()?;
    let params = (0..count).map(|_| reader.handle()).collect::<Result<_, _>>()?;
    let ret = reader.boolean()?.then(|| reader.handle()).transpose()?;
    let body = reader.handle()?;
    reader.end()?;
    Ok(FunctionSpec { public, name, params, ret, body })
}
