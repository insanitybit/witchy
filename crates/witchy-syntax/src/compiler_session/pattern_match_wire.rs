//! Bounded structural pattern and match-arm data for admitted comptime.
//! Nested syntax is resolved from typed handles in the current session.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternSpec {
    Wildcard,
    Var(String),
    Bool(bool),
    Int(i64),
    Str(String),
    Duration(i64),
    Range { lo: i64, hi: i64, inclusive: bool },
    Ctor { name: String, args: Vec<String> },
    CallSiteCtor { name: String, args: Vec<String> },
    AnonCtor { tag: String, args: Vec<String> },
    Tuple(Vec<String>),
    List { elems: Vec<String>, rest: Option<Option<String>> },
    Or(Vec<String>),
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

    fn payload(&mut self) -> Result<String, String> {
        let length = self.line()?.parse::<usize>()
            .map_err(|_| "invalid compiler pattern payload length")?;
        if length > self.0.len() { return Err("truncated compiler pattern payload".into()); }
        let value = std::str::from_utf8(&self.0[..length])
            .map_err(|_| "compiler pattern payload is not UTF-8")?;
        self.0 = &self.0[length..];
        Ok(value.to_owned())
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

    fn qualified_identifier(&mut self) -> Result<String, String> {
        let name = self.line()?;
        if name.len() > 1024 || name.split('.').any(|part| {
            let mut bytes = part.bytes();
            !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
                || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || part == "_"
        }) { return Err("invalid compiler qualified pattern identifier".into()); }
        Ok(name.to_owned())
    }

    fn call_site_constructor(&mut self) -> Result<String, String> {
        let name = self.line()?;
        let body = name.strip_prefix('.').unwrap_or(name);
        if name.len() > 1024 || body.split('.').any(|part| {
            let mut bytes = part.bytes();
            !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
                || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || part == "_"
        }) { return Err("invalid compiler call-site pattern constructor".into()); }
        Ok(name.to_owned())
    }

    fn handles(&mut self) -> Result<Vec<String>, String> {
        let count = self.count()?;
        (0..count).map(|_| self.handle()).collect()
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
        "S" => PatternSpec::Str(reader.payload()?),
        "D" => PatternSpec::Duration(reader.line()?.parse()
            .map_err(|_| "invalid compiler pattern duration")?),
        "R" => PatternSpec::Range {
            lo: reader.line()?.parse().map_err(|_| "invalid compiler pattern range start")?,
            hi: reader.line()?.parse().map_err(|_| "invalid compiler pattern range end")?,
            inclusive: match reader.line()? { "0" => false, "1" => true,
                _ => return Err("invalid compiler pattern range marker".into()) },
        },
        "C" => {
            let name = reader.qualified_identifier()?;
            PatternSpec::Ctor { name, args: reader.handles()? }
        }
        "K" => PatternSpec::CallSiteCtor { name: reader.call_site_constructor()?, args: reader.handles()? },
        "A" => PatternSpec::AnonCtor { tag: reader.identifier()?, args: reader.handles()? },
        "T" => PatternSpec::Tuple(reader.handles()?),
        "L" => {
            let elems = reader.handles()?;
            let rest = match reader.line()? {
                "0" => None, "1" => Some(None), "2" => Some(Some(reader.identifier()?)),
                _ => return Err("invalid compiler list rest marker".into()),
            };
            PatternSpec::List { elems, rest }
        }
        "O" => {
            let alternatives = reader.handles()?;
            if alternatives.is_empty() { return Err("compiler or pattern requires an alternative".into()); }
            PatternSpec::Or(alternatives)
        }
        "X" => return Err("call-site pattern construction is unavailable in admitted comptime".into()),
        _ => return Err("unknown compiler pattern builder".into()),
    };
    reader.end()?;
    Ok(pattern)
}

impl PatternSpec {
    pub fn build(self, mut resolve: impl FnMut(&str) -> Result<crate::ast::Pattern, String>)
        -> Result<crate::ast::Pattern, String> {
        use crate::ast::Pattern;
        fn children(handles: Vec<String>, resolve: &mut impl FnMut(&str) -> Result<Pattern, String>)
            -> Result<Vec<Pattern>, String> {
            handles.iter().map(|handle| resolve(handle)).collect()
        }
        Ok(match self {
            Self::Wildcard => Pattern::Wildcard,
            Self::Var(name) => Pattern::Var(name),
            Self::Bool(value) => Pattern::Bool(value),
            Self::Int(value) => Pattern::Int(value),
            Self::Str(value) => Pattern::Str(value),
            Self::Duration(value) => Pattern::Duration(value),
            Self::Range { lo, hi, inclusive } => Pattern::IntRange { lo, hi, inclusive },
            Self::Ctor { name, args } => Pattern::Ctor { name, args: children(args, &mut resolve)? },
            Self::CallSiteCtor { name, args } =>
                crate::linker::call_site_pattern(&name, children(args, &mut resolve)?),
            Self::AnonCtor { tag, args } => Pattern::AnonCtor { tag, args: children(args, &mut resolve)? },
            Self::Tuple(items) => Pattern::Tuple(children(items, &mut resolve)?),
            Self::List { elems, rest } => Pattern::List { elems: children(elems, &mut resolve)?, rest },
            Self::Or(items) => Pattern::Or(children(items, &mut resolve)?),
        })
    }
}

pub fn decode_arm(bytes: &[u8]) -> Result<ArmSpec, String> {
    if bytes.len() > 65_536 { return Err("compiler match arm input limit exceeded".into()); }
    let mut reader = Reader(bytes);
    let arm = ArmSpec { pattern: reader.handle()?, body: reader.handle()? };
    reader.end()?;
    Ok(arm)
}
