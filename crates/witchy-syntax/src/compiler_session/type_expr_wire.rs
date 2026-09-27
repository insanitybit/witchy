//! Bounded structural transport for `meta.TypeExpr` into the compiler-owned
//! type arena. The bytes encode data, never Witchy source or an authority.
use crate::ast::{CallableQualifiers, Convention, Type, TypeQual};

const MAX_INPUT: usize = 64 * 1024;
const MAX_NODES: usize = 4096;
const MAX_DEPTH: usize = 64;
const MAX_CHILDREN: usize = 256;

pub fn decode(input: &[u8]) -> Result<Type, String> {
    if input.len() > MAX_INPUT { return Err("reflected type data exceeds byte limit".into()); }
    let mut reader = Reader { remaining: input, nodes: 0 };
    let ty = reader.ty(0)?;
    if !reader.remaining.is_empty() { return Err("trailing reflected type data".into()); }
    Ok(ty)
}

struct Reader<'a> { remaining: &'a [u8], nodes: usize }

impl<'a> Reader<'a> {
    fn line(&mut self) -> Result<&'a str, String> {
        let end = self.remaining.iter().position(|byte| *byte == b'\n')
            .ok_or("malformed reflected type data")?;
        let line = std::str::from_utf8(&self.remaining[..end])
            .map_err(|_| "reflected type data is not UTF-8")?;
        self.remaining = &self.remaining[end + 1..];
        Ok(line)
    }

    fn count(&mut self) -> Result<usize, String> {
        let count = self.line()?.parse::<usize>()
            .map_err(|_| "invalid reflected type count")?;
        if count > MAX_CHILDREN { return Err("reflected type child limit exceeded".into()); }
        Ok(count)
    }

    fn string(&mut self) -> Result<&'a str, String> {
        let length = self.line()?.parse::<usize>()
            .map_err(|_| "invalid reflected type string length")?;
        if length > self.remaining.len() || length > MAX_INPUT {
            return Err("truncated reflected type string".into());
        }
        let value = std::str::from_utf8(&self.remaining[..length])
            .map_err(|_| "reflected type string is not UTF-8")?;
        self.remaining = &self.remaining[length..];
        Ok(value)
    }

    fn children(&mut self, depth: usize) -> Result<Vec<Type>, String> {
        let count = self.count()?;
        (0..count).map(|_| self.ty(depth + 1)).collect()
    }

    fn ty(&mut self, depth: usize) -> Result<Type, String> {
        if depth >= MAX_DEPTH { return Err("reflected type depth limit exceeded".into()); }
        self.nodes += 1;
        if self.nodes > MAX_NODES { return Err("reflected type node limit exceeded".into()); }
        match self.line()? {
            "N" => {
                let name = self.string()?.to_owned();
                let args = self.children(depth)?;
                if name == "View" && args.len() == 2
                    && matches!(&args[1], Type::Named(lifetime, nested)
                        if nested.is_empty() && lifetime.starts_with('\'')) {
                    return Err("`View(T, 'a)` is retired; use `&T` or `&[T]`".into());
                }
                Ok(Type::Named(name, args))
            }
            "T" => Ok(Type::Tuple(self.children(depth)?)),
            "F" => {
                let params = self.children(depth)?;
                let result = self.ty(depth + 1)?;
                let count = self.count()?;
                if count != 0 && count != params.len() {
                    return Err("reflected function convention count differs from parameters".into());
                }
                let mut conventions = Vec::with_capacity(params.len());
                if count == 0 { conventions.resize(params.len(), Convention::Let); }
                for _ in 0..count {
                    conventions.push(match self.string()? {
                        "" | "let" => Convention::Let,
                        "borrow" => Convention::Borrow,
                        "var" => Convention::Var,
                        "own" => Convention::Own,
                        _ => return Err("unknown reflected function convention".into()),
                    });
                }
                let pure = match self.line()? { "0" => false, "1" => true,
                    _ => return Err("invalid reflected function pure flag".into()) };
                let once = match self.line()? { "0" => false, "1" => true,
                    _ => return Err("invalid reflected function once flag".into()) };
                Ok(Type::Fn(params, Box::new(result), conventions,
                    CallableQualifiers::new(pure, once)))
            }
            "Q" => {
                let qualifier = match self.string()? {
                    "frozen" => TypeQual::Frozen,
                    "unique" => TypeQual::Unique,
                    _ => return Err("unknown reflected type qualifier".into()),
                };
                Ok(Type::Qualified(qualifier, Box::new(self.ty(depth + 1)?)))
            }
            "V" => Ok(Type::Qualified(TypeQual::View, Box::new(self.ty(depth + 1)?))),
            _ => Err("unknown reflected type tag".into()),
        }
    }
}
