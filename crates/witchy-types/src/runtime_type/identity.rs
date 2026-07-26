use super::*;

use witchy_syntax::ast::Convention;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrimitiveType {
    Int,
    Float,
    Duration,
    String,
    Bytes,
    Bool,
    Unit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeConvention {
    Let,
    Borrow,
    Var,
    Own,
}

impl From<Convention> for RuntimeConvention {
    fn from(value: Convention) -> Self {
        match value {
            Convention::Let => Self::Let,
            Convention::Borrow => Self::Borrow,
            Convention::Var => Self::Var,
            Convention::Own => Self::Own,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnionVariantIdentity {
    pub tag: String,
    pub payloads: Vec<RuntimeTypeIdentity>,
}

/// Canonical semantic identity of one runtime-representable type.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeTypeIdentity {
    Primitive(PrimitiveType),
    List(Box<Self>),
    Tuple(Vec<Self>),
    Function {
        params: Vec<Self>,
        result: Box<Self>,
        conventions: Vec<RuntimeConvention>,
    },
    Nominal {
        declaration: DeclarationIdentity,
        arguments: Vec<Self>,
    },
    Existential {
        declaration: DeclarationIdentity,
        arguments: Vec<Self>,
    },
    Record(Vec<(String, Self)>),
    Union(Vec<UnionVariantIdentity>),
}

impl RuntimeTypeIdentity {
    /// Convert a fully resolved Witchy type into runtime identity.
    ///
    /// `resolve` is the sole nominal-identity authority. It receives the
    /// canonical compiler name plus the expected declaration kind; returning
    /// `None` is a loud error rather than a fallback to that name.
    pub fn from_resolved_type(
        ty: &Type,
        resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
    ) -> Result<Self, RuntimeTypeError> {
        match ty {
            Type::Qualified(_, inner) => Self::from_resolved_type(inner, resolve),
            Type::RecordCompose { .. } => Err(RuntimeTypeError::MalformedStructuralType(
                "compiler invariant violated: structural record composition reached runtime type identity before records::lower normalized it"
                    .to_string(),
            )),
            Type::Tuple(items) if items.is_empty() => Ok(Self::Primitive(PrimitiveType::Unit)),
            Type::Tuple(items) => Ok(Self::Tuple(
                items
                    .iter()
                    .map(|item| Self::from_resolved_type(item, resolve))
                    .collect::<Result<_, _>>()?,
            )),
            Type::Fn(params, result, conventions) => {
                if !conventions.is_empty() && conventions.len() != params.len() {
                    return Err(RuntimeTypeError::ConventionArity {
                        params: params.len(),
                        conventions: conventions.len(),
                    });
                }
                let conventions = if conventions.is_empty() {
                    vec![RuntimeConvention::Let; params.len()]
                } else {
                    conventions.iter().copied().map(Into::into).collect()
                };
                Ok(Self::Function {
                    params: params
                        .iter()
                        .map(|param| Self::from_resolved_type(param, resolve))
                        .collect::<Result<_, _>>()?,
                    result: Box::new(Self::from_resolved_type(result, resolve)?),
                    conventions,
                })
            }
            Type::Dyn(name, args) => Ok(Self::Existential {
                declaration: resolve(name, DeclarationKind::Trait).ok_or_else(|| {
                    RuntimeTypeError::UnresolvedDeclaration {
                        kind: DeclarationKind::Trait,
                        name: name.clone(),
                    }
                })?,
                arguments: convert_arguments(args, resolve)?,
            }),
            Type::Named(name, args) => {
                if capability_type(name) {
                    return Err(RuntimeTypeError::CapabilityType(name.clone()));
                }
                if let Some(primitive) = primitive(name, args.len()) {
                    return Ok(Self::Primitive(primitive));
                }
                if name == "List" && args.len() == 1 {
                    return Ok(Self::List(Box::new(Self::from_resolved_type(
                        &args[0], resolve,
                    )?)));
                }
                if let Some(fields) = decode_anon_record(name) {
                    if fields.len() != args.len() {
                        return Err(RuntimeTypeError::MalformedStructuralType(format!(
                            "anonymous record head has {} field(s) but {} type argument(s)",
                            fields.len(),
                            args.len()
                        )));
                    }
                    let mut fields = fields
                            .into_iter()
                            .zip(args)
                            .map(|(field, ty)| {
                                Ok((field, Self::from_resolved_type(ty, resolve)?))
                            })
                            .collect::<Result<Vec<_>, RuntimeTypeError>>()?;
                    fields.sort_by(|left, right| left.0.cmp(&right.0));
                    return Ok(Self::Record(fields));
                }
                if let Some(variants) = crate::typeck::anon_union_synthetic_variants(name) {
                    return convert_union(variants, args, resolve);
                }
                Ok(Self::Nominal {
                    declaration: resolve(name, DeclarationKind::Type).ok_or_else(|| {
                        RuntimeTypeError::UnresolvedDeclaration {
                            kind: DeclarationKind::Type,
                            name: name.clone(),
                        }
                    })?,
                    arguments: convert_arguments(args, resolve)?,
                })
            }
        }
    }
}

fn convert_arguments(
    args: &[Type],
    resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
) -> Result<Vec<RuntimeTypeIdentity>, RuntimeTypeError> {
    args.iter()
        .map(|arg| RuntimeTypeIdentity::from_resolved_type(arg, resolve))
        .collect()
}

fn convert_union(
    variants: Vec<(String, usize)>,
    args: &[Type],
    resolve: &impl Fn(&str, DeclarationKind) -> Option<DeclarationIdentity>,
) -> Result<RuntimeTypeIdentity, RuntimeTypeError> {
    let expected: usize = variants.iter().map(|(_, arity)| arity).sum();
    if expected != args.len() {
        return Err(RuntimeTypeError::MalformedStructuralType(format!(
            "anonymous union head requires {expected} payload type(s), got {}",
            args.len()
        )));
    }
    let mut at = 0;
    let mut converted = Vec::with_capacity(variants.len());
    for (tag, arity) in variants {
        let payloads = args[at..at + arity]
            .iter()
            .map(|ty| RuntimeTypeIdentity::from_resolved_type(ty, resolve))
            .collect::<Result<_, _>>()?;
        converted.push(UnionVariantIdentity { tag, payloads });
        at += arity;
    }
    converted.sort_by(|left, right| left.tag.cmp(&right.tag));
    Ok(RuntimeTypeIdentity::Union(converted))
}

pub(super) fn primitive(name: &str, arity: usize) -> Option<PrimitiveType> {
    if arity != 0 {
        return None;
    }
    match name {
        "Int" => Some(PrimitiveType::Int),
        "Float" => Some(PrimitiveType::Float),
        "Duration" => Some(PrimitiveType::Duration),
        "String" => Some(PrimitiveType::String),
        "Bytes" => Some(PrimitiveType::Bytes),
        "Bool" => Some(PrimitiveType::Bool),
        "Nil" | "()" => Some(PrimitiveType::Unit),
        _ => None,
    }
}

pub(super) fn capability_type(name: &str) -> bool {
    matches!(
        name,
        "Console"
            | "Clock"
            | "Rand"
            | "Env"
            | "Secret"
            | "Exec"
            | "Dir"
            | "File"
            | "Net"
            | "Socket"
            | "Listener"
            | "BuildOut"
            | "BuildRead"
            | "BuildEnv"
            | "BuildNet"
            | "BuildExec"
    )
}

pub(super) fn decode_anon_record(name: &str) -> Option<Vec<String>> {
    let mut at = "__anon".len();
    name.strip_prefix("__anon")?;
    let count = fixed_width(name, &mut at, 10)?;
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let bytes = fixed_width(name, &mut at, 10)?;
        let mut field = Vec::with_capacity(bytes);
        for _ in 0..bytes {
            let byte = fixed_width(name, &mut at, 3)?;
            field.push(u8::try_from(byte).ok()?);
        }
        fields.push(String::from_utf8(field).ok()?);
    }
    (at == name.len()).then_some(fields)
}

fn fixed_width(text: &str, at: &mut usize, width: usize) -> Option<usize> {
    let end = at.checked_add(width)?;
    let part = text.get(*at..end)?;
    if !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    *at = end;
    part.parse().ok()
}

