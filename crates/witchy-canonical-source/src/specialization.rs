//! Logical identity of one generic callable specialization: the concrete type
//! arguments, alpha-normalized over borrow lifetimes, plus the source template
//! they were substituted into. The checker selects specializations; SafeWIR
//! and lowering only compare these identities (RFC-0170).

use witchy_syntax::ast::{Convention, Type, TypeQual};

/// One qualifier-preserving concrete type component of a generic callable
/// specialization. Unlike the trait/impl lookup key above, this identity keeps
/// ownership qualifiers because they are part of a callable's checked access
/// contract. Borrow lifetime spellings are alpha-normalized by first occurrence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpecializationTypeKey(String);

impl SpecializationTypeKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The generic signature from which one logical specialization was produced.
/// Codegen retains this source shape so it can independently substitute the
/// concrete arguments and compare them with the checked monomorphic function.
#[derive(Clone, Debug, PartialEq)]
pub struct LogicalSpecializationTemplate {
    type_parameters: Vec<String>,
    parameters: Vec<Option<Type>>,
    result: Option<Type>,
}

impl LogicalSpecializationTemplate {
    pub fn type_parameters(&self) -> &[String] {
        &self.type_parameters
    }

    pub fn parameters(&self) -> &[Option<Type>] {
        &self.parameters
    }

    pub fn result(&self) -> Option<&Type> {
        self.result.as_ref()
    }
}

/// Canonical logical identity for the concrete type arguments selected for one
/// generic callable. A single lifetime normalizer spans every component, so
/// relations between arguments survive while source binder names do not.
///
/// The concrete types and source template are retained only as codegen evidence:
/// equality, ordering, and hashing remain defined solely by the alpha-normalized
/// keys, preserving the established specialization and memoization identity.
#[derive(Clone, Debug)]
pub struct LogicalSpecializationIdentity {
    keys: Vec<SpecializationTypeKey>,
    concrete_types: Vec<Type>,
    template: Option<LogicalSpecializationTemplate>,
}

impl LogicalSpecializationIdentity {
    pub fn from_types(types: &[Type]) -> Self {
        let mut lifetimes = std::collections::BTreeMap::<String, u32>::new();
        let keys = types
            .iter()
            .map(|ty| SpecializationTypeKey(specialization_type_key(ty, &mut lifetimes)))
            .collect();
        Self {
            keys,
            concrete_types: types.to_vec(),
            template: None,
        }
    }

    pub fn from_types_with_template(
        types: &[Type],
        type_parameters: Vec<String>,
        parameters: Vec<Option<Type>>,
        result: Option<Type>,
    ) -> Self {
        let mut identity = Self::from_types(types);
        identity.template = Some(LogicalSpecializationTemplate {
            type_parameters,
            parameters,
            result,
        });
        identity
    }

    pub fn types(&self) -> &[SpecializationTypeKey] {
        &self.keys
    }

    pub fn concrete_types(&self) -> &[Type] {
        &self.concrete_types
    }

    pub fn template(&self) -> Option<&LogicalSpecializationTemplate> {
        self.template.as_ref()
    }
}

impl PartialEq for LogicalSpecializationIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.keys == other.keys
    }
}

impl Eq for LogicalSpecializationIdentity {}

impl PartialOrd for LogicalSpecializationIdentity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LogicalSpecializationIdentity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.keys.cmp(&other.keys)
    }
}

impl std::hash::Hash for LogicalSpecializationIdentity {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.keys, state);
    }
}

fn specialization_type_key(
    ty: &Type,
    lifetimes: &mut std::collections::BTreeMap<String, u32>,
) -> String {
    fn lifetime_id(
        lifetimes: &mut std::collections::BTreeMap<String, u32>,
        name: &str,
    ) -> u32 {
        if let Some(id) = lifetimes.get(name) {
            return *id;
        }
        let id = u32::try_from(lifetimes.len())
            .expect("a specialization cannot contain more than u32::MAX lifetime binders");
        lifetimes.insert(name.to_string(), id);
        id
    }

    fn render(
        ty: &Type,
        lifetimes: &mut std::collections::BTreeMap<String, u32>,
        key: &mut String,
    ) {
        match ty {
            Type::Qualified(qualifier, inner) => {
                match qualifier {
                    TypeQual::Frozen => key.push_str("qf<"),
                    TypeQual::Unique => key.push_str("qu<"),
                    TypeQual::View => key.push_str("qv<"),
                    TypeQual::Borrow(lifetime) | TypeQual::LegacyBorrow(lifetime) => {
                        let id = lifetime_id(lifetimes, lifetime);
                        key.push_str(&format!("qb{id}<"));
                    }
                    TypeQual::BorrowMut(lifetime) => {
                        let id = lifetime_id(lifetimes, lifetime);
                        key.push_str(&format!("qm{id}<"));
                    }
                }
                render(inner, lifetimes, key);
                key.push('>');
            }
            Type::Named(name, arguments) => {
                if let Some(lifetime) = witchy_syntax::ast::lifetime_param_name(name) {
                    let id = lifetime_id(lifetimes, lifetime);
                    key.push_str(&format!("life{id}"));
                    return;
                }
                key.push_str(name);
                if !arguments.is_empty() {
                    key.push('<');
                    for (index, argument) in arguments.iter().enumerate() {
                        if index > 0 {
                            key.push(',');
                        }
                        render(argument, lifetimes, key);
                    }
                    key.push('>');
                }
            }
            Type::Dyn(name, arguments) => {
                key.push_str("dyn ");
                key.push_str(name);
                if !arguments.is_empty() {
                    key.push('<');
                    for (index, argument) in arguments.iter().enumerate() {
                        if index > 0 {
                            key.push(',');
                        }
                        render(argument, lifetimes, key);
                    }
                    key.push('>');
                }
            }
            Type::Tuple(items) => {
                key.push_str(&format!("Tuple{}<", items.len()));
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        key.push(',');
                    }
                    render(item, lifetimes, key);
                }
                key.push('>');
            }
            Type::Slice(elem) => {
                key.push_str("Slice<");
                render(elem, lifetimes, key);
                key.push('>');
            }
            Type::Fn(parameters, result, conventions, qualifiers) => {
                key.push(if qualifiers.pure { 'p' } else { 'e' });
                key.push(if qualifiers.once { '1' } else { 'n' });
                key.push_str("fn[");
                for position in 0..parameters.len() {
                    key.push(match conventions.get(position).copied().unwrap_or_default() {
                        Convention::Let => 'l',
                        Convention::Borrow => 'b',
                        Convention::Var => 'v',
                        Convention::Own => 'o',
                    });
                }
                key.push_str("](");
                // A nested callable has an independent lifetime binder scope,
                // matching access verification. Its spellings cannot capture
                // or extend the surrounding specialization identity.
                let mut nested_lifetimes = std::collections::BTreeMap::new();
                for (index, parameter) in parameters.iter().enumerate() {
                    if index > 0 {
                        key.push(',');
                    }
                    render(parameter, &mut nested_lifetimes, key);
                }
                key.push_str(")->");
                render(result, &mut nested_lifetimes, key);
            }
            Type::RecordCompose { .. } => unreachable!(
                "compiler invariant violated: structural record composition reached generic specialization keys before records::lower normalized it"
            ),
        }
    }

    let mut key = String::new();
    render(ty, lifetimes, &mut key);
    key
}
