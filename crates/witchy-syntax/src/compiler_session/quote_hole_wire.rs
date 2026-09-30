//! Bounded, typed transport for compiler-owned quotation holes.
//! This is data, not authority: the receiving compiler session must authenticate
//! the checked template, source site, and every category-specific child handle.

const MAX_INPUT: usize = 65_536;
const MAX_HOLES: usize = 256;
const MAX_HANDLE: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Expr,
    Item,
    Type,
    Pattern,
    Stmt,
    Block,
}

impl Category {
    fn tag(self) -> u8 {
        match self {
            Self::Expr => b'E',
            Self::Item => b'I',
            Self::Type => b'T',
            Self::Pattern => b'P',
            Self::Stmt => b'S',
            Self::Block => b'B',
        }
    }
    fn from_tag(tag: u8) -> Result<Self, String> {
        match tag {
            b'E' => Ok(Self::Expr),
            b'I' => Ok(Self::Item),
            b'T' => Ok(Self::Type),
            b'P' => Ok(Self::Pattern),
            b'S' => Ok(Self::Stmt),
            b'B' => Ok(Self::Block),
            _ => Err("unknown compiler quotation category".into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoleCategory {
    Expr,
    Type,
    Pattern,
}

impl HoleCategory {
    fn tag(self) -> u8 {
        match self {
            Self::Expr => b'E',
            Self::Type => b'T',
            Self::Pattern => b'P',
        }
    }
    fn from_tag(tag: u8) -> Result<Self, String> {
        match tag {
            b'E' => Ok(Self::Expr),
            b'T' => Ok(Self::Type),
            b'P' => Ok(Self::Pattern),
            _ => Err("unknown compiler quotation hole category".into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hole {
    pub category: HoleCategory,
    pub handle: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub category: Category,
    pub template: String,
    pub source_line: u32,
    pub holes: Vec<Hole>,
}

fn write_field(out: &mut Vec<u8>, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_HANDLE {
        return Err("compiler quotation handle length is out of range".into());
    }
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

pub fn encode(plan: &Plan) -> Result<Vec<u8>, String> {
    if plan.source_line == 0 || plan.holes.is_empty() || plan.holes.len() > MAX_HOLES {
        return Err("compiler quotation hole plan is out of range".into());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"QH1");
    out.push(plan.category.tag());
    out.extend_from_slice(&plan.source_line.to_le_bytes());
    write_field(&mut out, &plan.template)?;
    out.extend_from_slice(&(plan.holes.len() as u16).to_le_bytes());
    for hole in &plan.holes {
        out.push(hole.category.tag());
        write_field(&mut out, &hole.handle)?;
    }
    if out.len() > MAX_INPUT {
        return Err("compiler quotation hole input limit exceeded".into());
    }
    Ok(out)
}

struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        if count > self.rest.len() {
            return Err("truncated compiler quotation hole plan".into());
        }
        let (value, rest) = self.rest.split_at(count);
        self.rest = rest;
        Ok(value)
    }
    fn field(&mut self) -> Result<String, String> {
        let length = u32::from_le_bytes(self.take(4)?.try_into().unwrap()) as usize;
        if length == 0 || length > MAX_HANDLE {
            return Err("compiler quotation handle length is out of range".into());
        }
        let value = std::str::from_utf8(self.take(length)?)
            .map_err(|_| "compiler quotation handle is not UTF-8")?;
        Ok(value.to_owned())
    }
}

pub fn decode(bytes: &[u8]) -> Result<Plan, String> {
    if bytes.len() > MAX_INPUT {
        return Err("compiler quotation hole input limit exceeded".into());
    }
    let mut reader = Reader { rest: bytes };
    if reader.take(3)? != b"QH1" {
        return Err("unknown compiler quotation hole plan version".into());
    }
    let category = Category::from_tag(reader.take(1)?[0])?;
    let source_line = u32::from_le_bytes(reader.take(4)?.try_into().unwrap());
    if source_line == 0 {
        return Err("compiler quotation source line is out of range".into());
    }
    let template = reader.field()?;
    let count = u16::from_le_bytes(reader.take(2)?.try_into().unwrap()) as usize;
    if count == 0 || count > MAX_HOLES {
        return Err("compiler quotation hole count is out of range".into());
    }
    let mut holes = Vec::with_capacity(count);
    for _ in 0..count {
        let category = HoleCategory::from_tag(reader.take(1)?[0])?;
        let handle = reader.field()?;
        holes.push(Hole { category, handle });
    }
    if !reader.rest.is_empty() {
        return Err("trailing compiler quotation hole data".into());
    }
    Ok(Plan {
        category,
        template,
        source_line,
        holes,
    })
}
