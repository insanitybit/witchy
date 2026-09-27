//! Bounded structural pattern and match-arm data for admitted comptime.
//! Nested syntax is resolved from typed handles in the current session.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternSpec {
    Wildcard,
    Var(String),
    Bool(bool),
    Int(i64),
    Ctor { name: String, args: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmSpec { pub pattern: String, pub body: String }

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn line(&mut self) -> Result<&str, String> {
        let end = self.0.iter().position(|byte| *byte == b'\n')
            .ok_or("malformed compiler pattern data")?;
        let value = std::str::from_utf8(&self.0[..end])
            .map_err(|_| "compiler pattern data is not UTF-8")?;
        self.0 = &self.0[end + 1..];
        Ok(value)
    }

    fn count(&mut self) -> Result<usize, String> {
        let count = self.line()?.parse::<usize>()
            .map_err(|_| "invalid compiler pattern count")?;
        if count > 256 { return Err("compiler pattern child limit exceeded".into()); }
        Ok(count)
    }

    fn handle(&mut self) -> Result<String, String> {
        let length = self.count()?;
        if length == 0 || length > self.0.len() { return Err("truncated compiler pattern handle".into()); }
        let handle = std::str::from_utf8(&self.0[..length])
            .map_err(|_| "compiler pattern handle is not UTF-8")?;
        self.0 = &self.0[length..];
        Ok(handle.to_owned())
    }

    fn identifier(&mut self) -> Result<String, String> {
        let name = self.line()?;
        let mut bytes = name.bytes();
        if !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || name == "_" || name.len() > 1024 {
            return Err("invalid compiler pattern identifier".into());
        }
        Ok(name.to_owned())
    }

    fn end(&self) -> Result<(), String> {
        if self.0.is_empty() { Ok(()) } else { Err("trailing compiler pattern data".into()) }
    }
}

pub fn decode_pattern(bytes: &[u8]) -> Result<PatternSpec, String> {
    if bytes.len() > 65_536 { return Err("compiler pattern input limit exceeded".into()); }
    let mut reader = Reader(bytes);
    let pattern = match reader.line()? {
        "W" => PatternSpec::Wildcard,
        "V" => PatternSpec::Var(reader.identifier()?),
        "B" => PatternSpec::Bool(match reader.line()? {
            "0" => false, "1" => true, _ => return Err("invalid compiler pattern boolean".into()),
        }),
        "I" => PatternSpec::Int(reader.line()?.parse()
            .map_err(|_| "invalid compiler pattern integer")?),
        "C" => {
            let name = reader.identifier()?;
            let count = reader.count()?;
            PatternSpec::Ctor { name, args: (0..count).map(|_| reader.handle())
                .collect::<Result<Vec<_>, _>>()? }
        }
        "X" => return Err("call-site pattern construction is unavailable in admitted comptime".into()),
        _ => return Err("unknown compiler pattern builder".into()),
    };
    reader.end()?;
    Ok(pattern)
}

pub fn decode_arm(bytes: &[u8]) -> Result<ArmSpec, String> {
    if bytes.len() > 65_536 { return Err("compiler match arm input limit exceeded".into()); }
    let mut reader = Reader(bytes);
    let arm = ArmSpec { pattern: reader.handle()?, body: reader.handle()? };
    reader.end()?;
    Ok(arm)
}
