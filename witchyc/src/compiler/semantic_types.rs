//! Semantic template/type interning. No layout, byte offsets or carrier ranges.
use super::{logical_body::{Ty, FunctionId}, properties::Properties, scalar::Scalar};
use crate::{ast as a, diagnostic::Diagnostic};
use std::collections::{BTreeMap, BTreeSet};
type Result<T> = std::result::Result<T, Diagnostic>;
/// What a record over the size limit is told: what a slot is and what to do instead.
pub(super) const SLOTS: &str = "this type would hold more than 32 numbers, the most a record, or an array of records or arrays, may hold (aggregate exceeds 32 scalar slots: a slot is one number, so an array field counts once per element and a record field once per number inside it); keep a long array in a variable of its own (an array of numbers alone may have 256 elements) or in a `Vec`, beside the record and not in it";
fn error(message: impl Into<std::borrow::Cow<'static, str>>) -> Diagnostic { Diagnostic::new("W0403", 0..0, message) }
/// `message`, saying which type: these refusals have no place in the source of their own.
/// Edits from one name to another.
fn edits(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut previous = row[0]; row[0] = i;
        for j in 1..=b.len() { let here = row[j]; row[j] = (row[j] + 1).min(row[j - 1] + 1).min(previous + usize::from(a[i - 1] != b[j - 1])); previous = here; }
    }
    row[b.len()]
}
fn named(message: &str, name: &str) -> Diagnostic {
    let shown = name.split('@').next().unwrap_or(name);
    Diagnostic::new("W0403", 0..0, format!("{message} (`{shown}`)"))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Kind { Record(String), Array(Ty, u32) }
#[derive(Clone, Debug)]
pub(super) struct Field { pub name: String, pub ty: Ty }
#[derive(Clone, Debug)]
pub(super) struct Aggregate { pub kind: Kind, pub fields: Vec<Field>, pub origin: Option<(usize,Vec<Ty>)>, concrete: bool, properties: Properties,
    /// The fields are still being resolved; and whether the record was named meanwhile.
    building: bool, recursive: bool }
#[derive(Clone, Debug)]
pub(super) struct Sum { pub arms: Vec<Ty>,
    /// Variant names, in tag order.
    pub names: Vec<String>,
    /// The declaration and its type arguments; `Result` has none and is known by its arms.
    pub origin: Option<(usize,Vec<Ty>)>, concrete: bool, properties: Properties,
    /// `.[Tag | Tag(T)]`: known by its tags and what they hold, with the tags in name order.
    pub anonymous: bool,
    /// The variants are still being resolved.
    building: bool,
    /// The sum was named while its own variants were being resolved.
    recursive: bool }
/// The `Ok` and `Err` payload types of a `Result`.
#[derive(Clone, Copy, Debug)]
pub(super) struct ResultSum { pub arms: [Ty; 2] }
#[derive(Clone, Debug)]
struct SumDefinition { name: String, parameters: Vec<String>, variants: Vec<(String, TemplateType)> }
#[derive(Clone, Debug)]
/// `dense`: a `PrefixBuffer<T>`, whose first cells are the full ones (the standard Vec's
/// storage). It issues no key, link or weak reference, and is a type of its own.
pub(super) struct Buffer { pub element: Ty, pub dense: bool }
/// Resolved template syntax is semantic, not a second AST type-definition map.
#[derive(Clone, Debug)]
enum TemplateType { Base(Ty), Param(usize), Record(usize, Vec<Self>), Buffer(Box<Self>), Prefix(Box<Self>), Boxed(Box<Self>), Weak(Box<Self>),
    Array(Box<Self>, u64), Result(Box<Self>, Box<Self>),
    /// `&mut T` as a value: a unique reference to a storable value.
    Unique(Box<Self>),
    /// `&T`: a shared reference to a storable value.
    Shared(Box<Self>),
    /// A declared sum and its type arguments.
    Sum(usize, Vec<Self>),
    /// Labelled fields in label order.
    Anonymous(Vec<(String, Self)>),
    /// Tagged variants in tag order.
    Union(Vec<(String, Self)>),
    /// `[T]`: valid only directly behind `&` (docs/design-slices.md).
    Slice(Box<Self>),
    Str }
#[derive(Clone, Debug)]
struct Definition { name: String, parameters: Vec<String>, fields: Vec<(String, TemplateType)>, owner: bool, sealed: Option<u32>, invariants: Vec<a::Expr> }
#[derive(Clone, Copy)]
struct Role { leaves: usize, owners: usize }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SliceType {
    Elements(Ty),
    Utf8,
}
#[derive(Clone, Default, Debug)]
pub(super) struct Types {
    definitions: Vec<Definition>, names: BTreeMap<String, usize>,
    pub aggregates: Vec<Aggregate>, sums: Vec<Sum>, buffers: Vec<Buffer>,
    /// The element type of each slice type `[T]`, by its number.
    slices: Vec<SliceType>,
    text_literals: Vec<std::sync::Arc<str>>,
    /// `grantable capability` names: reported by `wasm-caps` as user capabilities.
    pub grantable: BTreeSet<String>,
    pub order: Vec<Ty>,
    instances: Vec<(usize, Vec<Ty>, Ty)>,
    sum_definitions: Vec<SumDefinition>, sum_names: BTreeMap<String, usize>,
    /// Each variant name, with every sum that has a variant of that name and its tag there.
    variants: BTreeMap<String,Vec<(usize,usize)>>,
    console: bool, input: bool, allocator: bool, heap: bool,
    /// `type Name<T> = Type` declarations, expanded wherever the name is written.
    aliases: BTreeMap<String,(Vec<String>,a::Type)>,
    /// Function type parameters declared `T: Any`: not assumed Copy or droppable.
    any: BTreeSet<(u32,u32)>,
    /// Each brand an opening made (`brand roads.nodes() as nodes:`), with the name of what was
    /// opened, for messages (docs/design-brands.md).
    brands: Vec<(Ty, String)>,
    brand_sites: BTreeMap<(u32, usize), Ty>,
}
fn reserved(name: &str) -> bool { Scalar::named(name).is_some() || matches!(name, "str" | "Bool" | "Console" | "Dir" | "File" | "Clock" | "Rand" | "Env" | "Args" | "Net" | "Socket" | "Listener" | "Sqlite" | "LocalService" | "TlsSigner" | "Mldsa87Signer" | "P384Key" | "Group" | "Task" | "Exec" | "Run" | "Secret" | "SecretStore" | "main") || super::sum::reserved(name) }
impl Types {
    pub fn new(program: &a::Program) -> Result<Self> {
        // A refusal with no place of its own, made while checking one type, is placed at its name.
        let mut at = None;
        Self::built(program, &mut at).map_err(|mut e| {
            if e.span == (0..0) && let Some(span) = at { e.span = span; }
            e
        })
    }
    fn built(program: &a::Program, at: &mut Option<std::ops::Range<usize>>) -> Result<Self> {
        let place = |span: &std::ops::Range<usize>| (*span != (0..0)).then(|| span.clone());
        // A type parameter may share its name with a type private to another file, which cannot
        // be named where the parameter is (a program's `type T` beside the library's `Vec<T>`).
        let elsewhere = |name: &str, module: u32| program.private.get(name).is_some_and(|file| *file != module);
        let record_spans = program.declarations.iter().filter_map(|d| match d { a::Declaration::Record { span, .. } => Some(span.clone()), _ => None }).collect::<Vec<_>>();
        let sum_spans = program.declarations.iter().filter_map(|d| match d { a::Declaration::Sum { span, .. } => Some(span.clone()), _ => None }).collect::<Vec<_>>();
        let mut types = Self {
            text_literals: program.text_literals.iter().map(|text| text.as_str().into()).collect(),
            ..Self::default()
        };
        // Borrowed UTF-8 exists in every program: a literal is one.
        types.str_borrow();
        for d in &program.declarations {
            if let a::Declaration::Record { name, generics, conditions, sealed, module, span, .. } = d {
                *at = place(span);
                if !conditions.is_empty() && !sealed { return Err(named("only a sealed type can state an invariant: anyone could break it otherwise", name)); }
                if reserved(name) || types.names.insert(name.clone(), types.definitions.len()).is_some() {
                    return Err(error("duplicate or reserved record name"));
                }
                types.definitions.push(Definition { name: name.clone(), parameters: generics.clone(), fields: vec![], owner: false, sealed: sealed.then_some(*module), invariants: conditions.clone() });
            }
        }
        for d in &program.declarations {
            if let a::Declaration::Sum { name, generics, variants, span, .. } = d {
                *at = place(span);
                let id = types.sum_definitions.len();
                if reserved(name) || types.names.contains_key(name) || types.sum_names.insert(name.clone(), id).is_some() {
                    return Err(error("duplicate or reserved type name"));
                }
                if generics.len() > 8 || variants.len() > 32 { return Err(error("a sum type has at most 8 type parameters and 32 variants")); }
                // Two types may each have a variant of one name (`FsError.Io`, `NetError.Io`): the
                // type expected where one is built says which (`variant`).
                for (tag, (variant, _)) in variants.iter().enumerate() {
                    let sums = types.variants.entry(variant.clone()).or_default();
                    if reserved(variant) || types.names.contains_key(variant) || sums.iter().any(|(sum, _)| *sum == id) {
                        return Err(error("a variant name is used once in its type: it cannot repeat or match a type or reserved name"));
                    }
                    sums.push((id, tag));
                }
                types.sum_definitions.push(SumDefinition { name: name.clone(), parameters: generics.clone(), variants: vec![] });
            }
        }
        *at = None;
        for d in &program.declarations {
            if let a::Declaration::Alias { name, generics, ty } = d {
                if reserved(name) || types.names.contains_key(name) || types.sum_names.contains_key(name) || types.variants.contains_key(name) || generics.len()>8 || types.aliases.insert(name.clone(),(generics.clone(),ty.clone())).is_some() {
                    return Err(error("duplicate or reserved type alias name"));
                }
            }
        }
        for d in &program.declarations {
            if let a::Declaration::Record { name, generics, fields, span, module, .. } = d {
                *at = place(span);
                let mut params = BTreeMap::new();
                if generics.len() > 8 { return Err(error("at most 8 record type parameters")); }
                for (i, p) in generics.iter().enumerate() {
                    if reserved(p) || (types.names.contains_key(p) && !elsewhere(p, *module)) || params.insert(p.clone(), i).is_some() {
                        return Err(error("duplicate or conflicting record type parameter"));
                    }
                }
                let resolved = fields.iter().map(|(n, t)| Ok((n.clone(), types.resolve(t, &params)?))).collect::<Result<Vec<_>>>()?;
                types.definitions[types.names[name]].fields = resolved;
            }
        }
        for d in &program.declarations {
            if let a::Declaration::Sum { name, generics, variants, span, module } = d {
                *at = place(span);
                let mut params = BTreeMap::new();
                for (i, p) in generics.iter().enumerate() {
                    if reserved(p) || (!types.parameter_name_available(p) && !elsewhere(p, *module)) || params.insert(p.clone(), i).is_some() {
                        return Err(error("duplicate or conflicting sum type parameter"));
                    }
                }
                let resolved = variants.iter().map(|(n, t)| Ok((n.clone(), match t { Some(t) => types.resolve(t, &params)?, None => TemplateType::Base(Ty::Unit) }))).collect::<Result<Vec<_>>>()?;
                types.sum_definitions[types.sum_names[name]].variants = resolved;
            }
        }
        for id in 0..types.definitions.len() {
            *at = record_spans.get(id).and_then(place);
            let params = vec![Role { leaves: 1, owners: 0 }; types.definitions[id].parameters.len()];
            let role = types.roles(id, &params, &mut BTreeSet::new(), 0, &mut 0)?;
            types.definitions[id].owner = role.owners != 0;
        }
        // A generic record nothing uses is never instantiated, so what instantiating would refuse
        // whatever its arguments is checked on the template itself: a `()` field, and holding
        // itself other than behind a Buffer or a reference (integration review: both were
        // accepted until the record was used).
        for id in 0..types.definitions.len() {
            if types.definitions[id].parameters.is_empty() { continue; }
            *at = record_spans.get(id).and_then(place);
            if types.definitions[id].fields.iter().any(|(_, t)| matches!(t, TemplateType::Base(Ty::Unit))) { return Err(error("unit or duplicate aggregate field")); }
            if types.holds_itself(id) { return Err(error("a record type can contain itself only inside a Buffer: its values have a fixed size")); }
        }
        for id in 0..types.definitions.len() {
            *at = record_spans.get(id).and_then(place);
            if types.definitions[id].parameters.is_empty() { types.record(id, &[])?; }
        }
        for id in 0..types.sum_definitions.len() {
            *at = sum_spans.get(id).and_then(place);
            if types.sum_definitions[id].parameters.is_empty() { types.declared_sum(id, &[])?; }
        }
        *at = None;
        types.validate_reference_storage()?;
        Ok(types)
    }
    fn resolve(&self, source: &a::Type, params: &BTreeMap<String, usize>) -> Result<TemplateType> {
        self.resolve_inner(source,params,0,&mut 0)
    }
    /// A type name nothing declares: the nearest declared record or sum names.
    fn unknown_type(&self, name: &str) -> Diagnostic {
        let shown = name.split('@').next().unwrap_or(name);
        let mut near: Vec<(usize, String)> = self.names.keys().chain(self.sum_names.keys())
            .map(|n| n.split('@').next().unwrap_or(n).to_string())
            .filter(|n| !n.contains('#') && !n.is_empty() && n != shown)
            .map(|n| (edits(&n, shown), n)).filter(|(d, _)| *d <= 2.max(shown.len() / 4)).collect();
        near.sort(); near.dedup();
        match near.first() {
            Some((_, n)) => Diagnostic::new("W0403", 0..0, format!("unknown type `{shown}`: no type of this name is declared or imported here; the nearest declared type is `{n}`")),
            // A standard library type the program has not imported.
            None => match super::files::module_declaring_type(shown) {
                Some(module) => Diagnostic::new("W0403", 0..0, format!("unknown type `{shown}`: no type of this name is declared or imported here; the standard library declares it in `{module}`: add `import {module}`")),
                None => Diagnostic::new("W0403", 0..0, format!("unknown type `{shown}`: no type of this name is declared or imported here")),
            },
        }
    }
    fn resolve_inner(&self, source: &a::Type, params: &BTreeMap<String, usize>, depth: usize, visits: &mut usize) -> Result<TemplateType> {
        *visits += 1;
        if depth>16 || *visits>4096 { return Err(error("record template complexity limit")); }
        use TemplateType as T;
        Ok(match source {
            a::Type::Unit => T::Base(Ty::Unit),
            a::Type::Named(name, args) => {
                if let Some(id) = params.get(name) {
                    if !args.is_empty() { return Err(error("type parameter cannot take arguments")); }
                    T::Param(*id)
                } else if let Some(s) = Scalar::named(name) {
                    if !args.is_empty() { return Err(error("scalar type cannot take arguments")); } T::Base(Ty::Unsigned(s))
                } else if name == "F64" {
                    if !args.is_empty() { return Err(error("scalar type cannot take arguments")); } T::Base(Ty::F64)
                } else if name == "I64" {
                    if !args.is_empty() { return Err(error("scalar type cannot take arguments")); } T::Base(Ty::I64)
                } else { match name.as_str() {
                    "Clock" | "Rand" | "Env" | "Args" | "Net" | "Socket" | "Listener" | "Group" | "Task" | "Exec" | "Run" => { if !args.is_empty() { return Err(error("scalar/capability type cannot take arguments")); }
                        T::Base(Ty::Host(match name.as_str() { "Clock" => super::host::Host::Clock, "Rand" => super::host::Host::Rand, "Env" => super::host::Host::Env, "Args" => super::host::Host::Args,
                            "Net" => super::host::Host::Net, "Listener" => super::host::Host::Listener, "Group" => super::host::Host::Group, "Task" => super::host::Host::Task, "Exec" => super::host::Host::Exec, "Run" => super::host::Host::Run, _ => super::host::Host::Socket })) },
                    "P384Key" if args.is_empty() => T::Base(Ty::Host(super::host::Host::P384Key)),
                    "LocalService" if args.is_empty() => T::Base(Ty::Host(super::host::Host::LocalService)),
                    "Sqlite" if args.is_empty() => T::Base(Ty::Host(super::host::Host::Sqlite)),
                    "#Grants" if args.is_empty() => T::Base(Ty::Host(super::host::Host::Grants)),
                    "Bool" | "Allocator" => { if !args.is_empty() { return Err(error("scalar/capability type cannot take arguments")); }
                        T::Base(if name == "Bool" { Ty::Bool } else { Ty::Allocator }) },
                    "Console" => match args.as_slice() {
                        [a::Type::Named(n,a)] if n=="Write" && a.is_empty() => T::Base(Ty::Console),
                        [a::Type::Named(n,a)] if n=="Read" && a.is_empty() => T::Base(Ty::Input),
                        _ => return Err(error("a Console is `Console<Write>` or `Console<Read>`")),
                    },
                    "Mldsa87Signer" if args.is_empty() => T::Base(Ty::Host(super::host::Host::Mldsa87Signer)),
                    "TlsSigner" if args.is_empty() => T::Base(Ty::Host(super::host::Host::TlsSigner)),
                    "Signer" => { if !args.is_empty() { return Err(error("a Signer takes no arguments: its role is the grant's (`use = \"sign/witchy-2/ROLE\"`)")); } T::Base(Ty::Host(super::host::Host::Signer)) },
                    "SecretStore" => { if !args.is_empty() { return Err(error("scalar/capability type cannot take arguments")); } T::Base(Ty::Host(super::host::Host::SecretStore)) },
                    // A bare `Secret` can be both revealed and used to sign.
                    "Secret" if args.is_empty() => T::Base(Ty::Host(super::host::Host::Secret(super::host::Right::All))),
                    "Secret" => match args.as_slice() {
                        [a::Type::Named(right,inner)] if inner.is_empty() => T::Base(Ty::Host(super::host::Host::secret(right)
                            .ok_or_else(|| error("a Secret has one right: `Secret<Reveal>` or `Secret<Seal>`"))?)),
                        _ => return Err(error("a Secret has one right: `Secret<Reveal>` or `Secret<Seal>`")),
                    },
                    // The book's bare `Dir`: both rights, standing in for either.
                    "Dir" if args.is_empty() => T::Base(Ty::Host(super::host::Host::Dir(super::host::Right::All))),
                    "Dir" | "File" => match args.as_slice() {
                        [a::Type::Named(right,inner)] if inner.is_empty() => T::Base(Ty::Host(super::host::Host::named(name,right)
                            .ok_or_else(|| error("a Dir or File has one right: `Dir<Read>`, `Dir<Write>`, `File<Read>` or `File<Write>`"))?)),
                        _ => return Err(error("a Dir or File has one right: `Dir<Read>`, `Dir<Write>`, `File<Read>` or `File<Write>`")),
                    },
                    "str" if args.is_empty() => T::Str,
                    "str" => return Err(error("str takes no type arguments")),
                    "Weak" => { let [t] = args.as_slice() else { return Err(error("Weak takes one stored type")); };
                        T::Weak(Box::new(self.resolve_inner(t, params, depth+1, visits)?)) },
                    "Boxed" => { let [t] = args.as_slice() else { return Err(error("Boxed takes one stored type")); };
                        T::Boxed(Box::new(self.resolve_inner(t, params, depth+1, visits)?)) },
                    "Buffer" => { let [t] = args.as_slice() else { return Err(error("Buffer requires one concrete element type")); };
                        T::Buffer(Box::new(self.resolve_inner(t, params, depth+1, visits)?)) },
                    "PrefixBuffer" => { let [t] = args.as_slice() else { return Err(error("PrefixBuffer requires one concrete element type")); };
                        T::Prefix(Box::new(self.resolve_inner(t, params, depth+1, visits)?)) },
                    "Result" => { let [ok,err] = args.as_slice() else { return Err(error("Result requires two concrete arm types")); };
                        T::Result(Box::new(self.resolve_inner(ok,params,depth+1,visits)?), Box::new(self.resolve_inner(err,params,depth+1,visits)?)) },
                    _ if self.aliases.contains_key(name) => { let (generics,body)=&self.aliases[name];
                        if args.len()!=generics.len() { return Err(error("type alias argument count mismatch")); }
                        let args=args.iter().map(|a| self.resolve_inner(a,params,depth+1,visits)).collect::<Result<Vec<_>>>()?;
                        // The body is read against the alias's own parameters, then they are replaced by the arguments.
                        // A cycle of aliases runs into the depth limit.
                        let own=generics.iter().cloned().enumerate().map(|(i,n)|(n,i)).collect();
                        let body=self.resolve_inner(body,&own,depth+1,visits)?; Self::instantiate(body,&args) },
                    _ if self.sum_names.contains_key(name) => { let id = self.sum_names[name];
                        if args.len() != self.sum_definitions[id].parameters.len() { return Err(error("sum type argument count mismatch")); }
                        T::Sum(id, args.iter().map(|a| self.resolve_inner(a,params,depth+1,visits)).collect::<Result<_>>()?) },
                    _ => { let id = *self.names.get(name).ok_or_else(|| self.unknown_type(name))?;
                        if args.len() != self.definitions[id].parameters.len() { return Err(error("record type argument count mismatch")); }
                        T::Record(id, args.iter().map(|a| self.resolve_inner(a,params,depth+1,visits)).collect::<Result<_>>()?) },
                }}
            },
            a::Type::Array { element, length } => T::Array(Box::new(self.resolve_inner(element,params,depth+1,visits)?), *length),
            a::Type::Record(fields) => { let mut fields=fields.iter().map(|(n,t)| Ok((n.clone(),self.resolve_inner(t,params,depth+1,visits)?))).collect::<Result<Vec<_>>>()?;
                fields.sort_by(|a,b| a.0.cmp(&b.0)); T::Anonymous(fields) },
            a::Type::Union(variants) => { let mut variants=variants.iter().map(|(n,t)| Ok((n.clone(),match t { Some(t)=>self.resolve_inner(t,params,depth+1,visits)?, None=>T::Base(Ty::Unit) }))).collect::<Result<Vec<_>>>()?;
                variants.sort_by(|a,b| a.0.cmp(&b.0)); T::Union(variants) },
            a::Type::Borrow { mutable: false, inner } => T::Shared(Box::new(self.resolve_inner(inner,params,depth+1,visits)?)),
            a::Type::Borrow { inner, .. } => T::Unique(Box::new(self.resolve_inner(inner,params,depth+1,visits)?)),
            a::Type::Slice(element) => T::Slice(Box::new(self.resolve_inner(element,params,depth+1,visits)?)),
        })
    }
    /// Whether record `start`'s fields reach `start` again through what is held inline: records,
    /// sums, arrays, Results, anonymous records and unions, with type arguments put in place.
    /// A Buffer or a reference holds a fixed-size handle, so the search stops there.
    fn holds_itself(&self, start: usize) -> bool {
        use TemplateType as T;
        let mut pending: Vec<TemplateType> = self.definitions[start].fields.iter().map(|(_, t)| t.clone()).collect();
        let (mut records, mut sums) = (BTreeSet::new(), BTreeSet::new());
        let mut visits = 0;
        while let Some(t) = pending.pop() {
            visits += 1;
            if visits > 4096 { return false; }
            match t {
                T::Record(id, args) => {
                    if id == start { return true; }
                    if records.insert(id) { pending.extend(self.definitions[id].fields.iter().map(|(_, f)| Self::instantiate(f.clone(), &args))); }
                }
                T::Sum(id, args) => if sums.insert(id) { pending.extend(self.sum_definitions[id].variants.iter().map(|(_, v)| Self::instantiate(v.clone(), &args))); },
                T::Array(e, _) => pending.push(*e),
                T::Result(a, b) => { pending.push(*a); pending.push(*b); }
                T::Anonymous(fields) | T::Union(fields) => pending.extend(fields.into_iter().map(|(_, f)| f)),
                T::Base(_) | T::Param(_) | T::Buffer(_) | T::Prefix(_) | T::Weak(_) | T::Boxed(_) | T::Shared(_) | T::Unique(_) | T::Slice(_) | T::Str => {}
            }
        }
        false
    }
    fn instantiate(t: TemplateType, args: &[TemplateType]) -> TemplateType {
        use TemplateType as T;
        match t {
            T::Param(i) => args[i].clone(),
            T::Record(id,inner) => T::Record(id,inner.into_iter().map(|t| Self::instantiate(t,args)).collect()),
            T::Sum(id,inner) => T::Sum(id,inner.into_iter().map(|t| Self::instantiate(t,args)).collect()),
            T::Weak(e) => T::Weak(Box::new(Self::instantiate(*e,args))),
            T::Boxed(e) => T::Boxed(Box::new(Self::instantiate(*e,args))),
            T::Buffer(e) => T::Buffer(Box::new(Self::instantiate(*e,args))),
            T::Prefix(e) => T::Prefix(Box::new(Self::instantiate(*e,args))),
            T::Shared(e) => T::Shared(Box::new(Self::instantiate(*e,args))),
            T::Unique(e) => T::Unique(Box::new(Self::instantiate(*e,args))),
            T::Slice(e) => T::Slice(Box::new(Self::instantiate(*e,args))),
            T::Array(e,n) => T::Array(Box::new(Self::instantiate(*e,args)),n),
            T::Result(a,b) => T::Result(Box::new(Self::instantiate(*a,args)),Box::new(Self::instantiate(*b,args))),
            T::Anonymous(fields) => T::Anonymous(fields.into_iter().map(|(n,t)|(n,Self::instantiate(t,args))).collect()),
            T::Union(variants) => T::Union(variants.into_iter().map(|(n,t)|(n,Self::instantiate(t,args))).collect()),
            t => t,
        }
    }
    fn roles(&self, id: usize, params: &[Role], stack: &mut BTreeSet<usize>, depth: usize, visits: &mut usize) -> Result<Role> {
        // A record met inside itself is valid only behind a Buffer, where its shape does not matter.
        if !stack.insert(id) { return Ok(Role { leaves: 1, owners: 1 }); }
        let fields = &self.definitions[id].fields;
        if fields.is_empty() || fields.len()>32 { return Err(error(format!("aggregate shape limit: a record has 1 to 32 fields, and this one has {}", fields.len()))); }
        let mut names = BTreeSet::new(); let mut role = Role { leaves: 0, owners: 0 };
        for (name, ty) in fields {
            if !names.insert(name) { return Err(error(format!("duplicate aggregate field: `{name}` is declared twice"))); }
            let field = self.role(ty,params,stack,depth,visits)?;
            role.leaves += field.leaves; role.owners += field.owners;
            if role.leaves>32 { return Err(error(SLOTS)); }
        }
        stack.remove(&id); Ok(role)
    }
    fn role(&self, ty: &TemplateType, params: &[Role], stack: &mut BTreeSet<usize>, depth: usize, visits: &mut usize) -> Result<Role> {
        *visits+=1; if depth>16 || *visits>4096 { return Err(error("record template complexity limit")); }
        use TemplateType as T;
        match ty {
            T::Base(Ty::Unsigned(_) | Ty::I64 | Ty::F64 | Ty::Bool) => Ok(Role { leaves: 1, owners: 0 }),
            // A capability may sit in a record, which then holds that authority: the record is not
            // Copy, so the authority is moved, never duplicated, and no source can make a capability.
            T::Base(Ty::Console | Ty::Input | Ty::Allocator | Ty::Host(_)) => Ok(Role { leaves: 1, owners: 0 }),
            T::Param(id) => Ok(params[*id]),
            T::Weak(_) => Ok(Role { leaves: 1, owners: 0 }),
            T::Buffer(element) | T::Prefix(element) | T::Boxed(element) => { self.role(element,params,stack,depth+1,visits)?; Ok(Role { leaves: 1, owners: 1 }) },
            T::Record(id,args) => { let args = args.iter().map(|t| self.role(t,params,stack,depth+1,visits)).collect::<Result<Vec<_>>>()?;
                self.roles(*id,&args,stack,depth+1,visits) },
            T::Array(element,n) if (1..=256).contains(n) => { let element=self.role(element,params,stack,depth+1,visits)?;
                if element.owners!=0 { return Err(error("arrays require Copy elements without ownership")); }
                let leaves=element.leaves * *n as usize;
                // An array alone may hold up to 256 numbers: indexed in place it lives in the
                // frame (concrete.rs). Within a record or sum the 32 limit still holds.
                if leaves>256 { return Err(error("an array holds at most 256 numbers")); } Ok(Role { leaves, owners: 0 }) },
            T::Anonymous(fields) => { let mut role=Role { leaves: 0, owners: 0 };
                for (_,t) in fields { let field=self.role(t,params,stack,depth+1,visits)?; role.leaves+=field.leaves; role.owners+=field.owners; }
                if role.leaves>32 { return Err(error(SLOTS)); } Ok(role) },
            T::Base(Ty::Unit) => Ok(Role { leaves: 0, owners: 0 }),
            T::Result(ok,err) => { let mut role=Role { leaves: 1, owners: 0 };
                for arm in [ok,err] { let arm=self.role(arm,params,stack,depth+1,visits)?; role.leaves+=arm.leaves; role.owners+=arm.owners; }
                if role.leaves>32 { return Err(error(SLOTS)); } Ok(role) },
            T::Union(variants) => { let mut role=Role { leaves: 1, owners: 0 };
                for (_,arm) in variants { let arm=self.role(arm,params,stack,depth+1,visits)?; role.leaves+=arm.leaves; role.owners+=arm.owners; }
                if role.leaves>32 { return Err(error(SLOTS)); } Ok(role) },
            T::Sum(id,args) => {
                // A sum met inside itself is valid only behind a Buffer, where its shape does not matter.
                let key=*id+(1<<40);
                if stack.contains(&key) { return Ok(Role { leaves: 1, owners: 1 }); }
                let args = args.iter().map(|t| self.role(t,params,stack,depth+1,visits)).collect::<Result<Vec<_>>>()?;
                stack.insert(key);
                let mut role=Role { leaves: 1, owners: 0 };
                for (_,arm) in &self.sum_definitions[*id].variants { let arm=self.role(arm,&args,stack,depth+1,visits)?; role.leaves+=arm.leaves; role.owners+=arm.owners; }
                stack.remove(&key);
                if role.leaves>32 { return Err(error(SLOTS)); } Ok(role) },
            // A `&` field makes the record a view: a value that lives in locals and keeps what
            // it was made from lent. What it points at is judged where the type is made.
            // A `&[T]` field: address and count, and the record is a view (docs/design-slices.md).
            T::Shared(inner) if matches!(**inner, T::Slice(_) | T::Str) => Ok(Role { leaves: 2, owners: 0 }),
            T::Shared(_) => Ok(Role { leaves: 1, owners: 0 }),
            T::Unique(_) => Err(error("a record field holds a `&` reference, not `&mut`: a record that could change what it points at would need loans counted by field")),
            _ => Err(error("record fields require stored values: loans and capabilities are unsupported")),
        }
    }
    fn materialize(&mut self, ty: &TemplateType, params: &[Ty]) -> Result<Ty> {
        use TemplateType as T;
        match ty {
            T::Base(t) => { self.console |= *t==Ty::Console; self.input |= *t==Ty::Input; self.allocator |= *t==Ty::Allocator; Ok(*t) },
            T::Param(id) => params.get(*id).copied().ok_or_else(|| error("unbound semantic type parameter has no concrete type")),
            T::Record(id,args) => { let args=args.iter().map(|t| self.materialize(t,params)).collect::<Result<Vec<_>>>()?; self.record(*id,&args) },
            T::Weak(element) | T::Boxed(element) => {
                let storage=self.materialize(&T::Buffer(element.clone()),params)?;
                let Ty::Buffer(id)=storage else { unreachable!() };
                Ok(if matches!(ty,T::Weak(_)) { Ty::Weak(id) } else { Ty::Boxed(id) })
            },
            T::Buffer(element) | T::Prefix(element) => { let dense=matches!(ty,T::Prefix(_)); let element=self.materialize(element,params)?; self.storable(element)?;
                // Heap cells hold data, never authority (book: capabilities-optional.md).
                if self.authority_in(element,0) { return Err(error("a Buffer does not hold a capability, or a value that holds one")); }
                if let Some(id)=self.buffers.iter().position(|b| b.element==element && b.dense==dense) { return Ok(Ty::Buffer(id as u32)); }
                let id=self.buffers.len(); self.buffers.push(Buffer { element, dense }); let ty=Ty::Buffer(id as u32); self.order.push(ty); Ok(ty) },
            T::Array(element,n) => { let element=self.materialize(element,params)?; self.array(element,*n) },
            T::Result(ok,err) => { let ok=self.materialize(ok,params)?; let err=self.materialize(err,params)?; self.result([ok,err]) },
            T::Sum(id,args) => { let args=args.iter().map(|t| self.materialize(t,params)).collect::<Result<Vec<_>>>()?; self.declared_sum(*id,&args) },
            // `[T]` behind a reference: a run of initialized elements, which hold no reference.
            T::Shared(inner) | T::Unique(inner) if matches!(**inner, T::Slice(_)) => {
                let T::Slice(element)=&**inner else { unreachable!() };
                let element=self.materialize(element,params)?;
                let Ty::Slice(id)=self.slice_of(element)? else { unreachable!() };
                Ok(Ty::Borrow{mutable:matches!(ty,T::Unique(_)),pointee:super::logical_body::Pointee::Slice(id)}) },
            T::Shared(inner) if matches!(**inner, T::Str) => Ok(self.str_borrow()),
            T::Unique(inner) if matches!(**inner, T::Str) => Err(error("str may only be borrowed as &str")),
            T::Str => Err(error("str is dynamically sized and is used behind a shared reference: &str")),
            T::Slice(_) => Err(error("a slice `[T]` is used behind a reference: `&[T]`")),
            T::Shared(inner) => { let inner=self.materialize(inner,params)?;
                // A shared reference to a shared view keeps the view's loans while in use
                // (docs/design-references-to-views.md, step 1).
                if self.lends(inner) && !self.shared_view(inner) { return Err(error("a reference points at a storable value, not at a value that holds a reference")); }
                Ok(Ty::Borrow{mutable:false,pointee:super::logical_body::Pointee::from_ty(inner).ok_or_else(||error("a reference points at a storable value, not at a unit or another reference"))?}) },
            T::Unique(inner) => { let inner=self.materialize(inner,params)?;
                // `&mut` to a shared view: its scalar fields may change, never its references
                // (docs/design-references-to-views.md, step 2; check.rs refuses those writes).
                if self.lends(inner) && !self.shared_view(inner) { return Err(error("a reference points at a storable value, not at a value that holds a reference")); }
                if matches!(inner,Ty::Console|Ty::Input|Ty::Allocator|Ty::Host(_)) && inner!=Ty::Host(super::host::Host::Task) { return Err(error("mutable Console/Allocator/Dir/File references are unsupported")); }
                Ok(Ty::Borrow{mutable:true,pointee:super::logical_body::Pointee::from_ty(inner).ok_or_else(||error("a reference points at a storable value, not at a unit or another reference"))?}) }
            T::Union(variants) => { let variants=variants.iter().map(|(n,t)| Ok((n.clone(),self.materialize(t,params)?))).collect::<Result<Vec<_>>>()?; self.union(variants) },
            T::Anonymous(fields) => { let fields=fields.iter().map(|(n,t)| Ok((n.clone(),self.materialize(t,params)?))).collect::<Result<Vec<_>>>()?; self.anonymous(fields) },
        }
    }
    /// The record with exactly these labelled fields. Label order does not matter and there is one per shape.
    pub fn anonymous(&mut self, mut fields: Vec<(String,Ty)>) -> Result<Ty> {
        fields.sort_by(|a,b| a.0.cmp(&b.0));
        if fields.is_empty() || fields.len()>32 { return Err(error(format!("aggregate shape limit: a record has 1 to 32 fields, and this one has {}", fields.len()))); }
        if let Some(w) = fields.windows(2).find(|w| w[0].0==w[1].0) { return Err(error(format!("duplicate aggregate field: `{}` is given twice", w[0].0))); }
        // A `&` reference, or a record that holds one, makes this record a view too; a reference
        // to a view stays a local's (docs/design-references-to-views.md).
        if fields.iter().any(|(_,t)| matches!(t, Ty::Borrow{pointee,..} if self.lends(pointee.ty()))) {
            return Err(error("a reference points at a storable value, not at a value that holds a reference: a record field cannot hold a reference to a view"));
        }
        for (_,t) in &fields { if !(matches!(t,Ty::Borrow{mutable:false,..}) || (matches!(t,Ty::Aggregate(_)) && self.lends(*t) && !self.lends_unique(*t))) { self.storable(*t)?; } }
        let same=|a:&Aggregate| a.origin.is_none() && matches!(a.kind,Kind::Record(_)) && a.fields.len()==fields.len() && a.fields.iter().zip(&fields).all(|(f,(n,t))| f.name==*n && f.ty==*t);
        if let Some(id)=self.aggregates.iter().position(same) { return Ok(Ty::Aggregate(id as u32)); }
        let properties=self.combine(fields.iter().map(|(_,t)|*t)); let concrete=fields.iter().all(|(_,t)|self.concrete(*t));
        let fields=fields.into_iter().map(|(name,ty)| Field { name, ty }).collect();
        let ty=Ty::Aggregate(self.aggregates.len() as u32); self.aggregates.push(Aggregate { kind:Kind::Record(String::new()), fields, origin:None, concrete, properties, building:false, recursive:false });
        self.order.push(ty); Ok(ty)
    }
    /// A brand new at one opening: a record of no fields that no source can name, Copy and of no
    /// size. Equal only to itself.
    pub fn brand(&mut self, origin: String) -> Ty {
        let ty=Ty::Aggregate(self.aggregates.len() as u32);
        let name=format!("Brand#{}",self.brands.len());
        self.aggregates.push(Aggregate { kind:Kind::Record(name), fields:Vec::new(), origin:None, concrete:true, properties:self.combine(std::iter::empty()), building:false, recursive:false });
        // Not in `order`: a brand is a type argument only, never a value laid out in memory.
        self.brands.push((ty, origin)); ty
    }
    /// The brand of the opening at this place (function, source offset): the same however often
    /// it is read.
    pub fn brand_at(&mut self, site: (u32, usize), origin: String) -> Ty {
        if let Some(t) = self.brand_sites.get(&site) { return *t; }
        let t = self.brand(origin); self.brand_sites.insert(site, t); t
    }
    /// What an opening's brand came from (`roads`), when `t` is a brand.
    pub fn brand_origin(&self, t: Ty) -> Option<&str> { self.brands.iter().find(|(b,_)| *b==t).map(|(_,o)| o.as_str()) }
    /// `t` with every brand in it, at any depth, the one brand all are lowered as once checking
    /// is done (`Option<Node<Brand#3>>` becomes `Option<Node<Brand#0>>`).
    pub fn erased_brand(&mut self, t: Ty) -> Result<Ty> { self.erase_inner(t, 0) }
    fn mentions_brand(&self, t: Ty, depth: usize) -> bool {
        // Past the limit, say it may: `erase_inner` then refuses rather than keeping a brand.
        if depth > 16 { return true; }
        if self.brand_origin(t).is_some() { return true; }
        match t {
            Ty::Borrow{pointee,..}=>self.mentions_brand(pointee.ty(),depth+1),
            Ty::Weak(i) | Ty::Boxed(i) | Ty::Buffer(i)=>self.mentions_brand(self.buffers[i as usize].element,depth+1),
            Ty::Slice(i)=>self.mentions_brand(self.slice_element(i),depth+1),
            Ty::Aggregate(i)=>{let a=&self.aggregates[i as usize]; match (&a.origin,&a.kind) {
                (Some((_,args)),_)=>args.iter().any(|t|self.mentions_brand(*t,depth+1)),
                (None,Kind::Array(e,_))=>self.mentions_brand(*e,depth+1),
                (None,_)=>a.fields.iter().any(|f|self.mentions_brand(f.ty,depth+1)) }},
            Ty::Sum(i)=>{let s=&self.sums[i as usize]; s.origin.as_ref().is_some_and(|(_,args)|args.iter().any(|t|self.mentions_brand(*t,depth+1))) || (s.origin.is_none() && s.arms.iter().any(|t|self.mentions_brand(*t,depth+1)))},
            _=>false,
        }
    }
    fn erase_inner(&mut self, t: Ty, depth: usize) -> Result<Ty> {
        if self.brand_origin(t).is_some() { return Ok(self.brands[0].0); }
        if !self.mentions_brand(t, 0) { return Ok(t); }
        // A type nested deeper than this is refused, not left with its brands (review).
        if depth > 16 { return Err(error("a type nests brands too deeply to erase")); }
        Ok(match t {
            Ty::Borrow{mutable,pointee}=>{let inner=self.erase_inner(pointee.ty(),depth+1)?; match super::logical_body::Pointee::from_ty(inner) { Some(pointee)=>Ty::Borrow{mutable,pointee}, None=>t }},
            Ty::Weak(i)=>{let e=self.erase_inner(self.buffers[i as usize].element,depth+1)?; self.materialize(&TemplateType::Weak(Box::new(TemplateType::Base(e))),&[])?},
            Ty::Boxed(i)=>{let e=self.erase_inner(self.buffers[i as usize].element,depth+1)?; self.materialize(&TemplateType::Boxed(Box::new(TemplateType::Base(e))),&[])?},
            Ty::Buffer(i)=>{let e=Box::new(TemplateType::Base(self.erase_inner(self.buffers[i as usize].element,depth+1)?));
                self.materialize(&if self.buffers[i as usize].dense { TemplateType::Prefix(e) } else { TemplateType::Buffer(e) },&[])?},
            Ty::Slice(i) if self.is_str(i) => t,
            Ty::Slice(i)=>{let e=self.erase_inner(self.slice_element(i),depth+1)?; self.slice_of(e)?},
            Ty::Aggregate(i)=>{let a=&self.aggregates[i as usize];let (origin,kind)=(a.origin.clone(),a.kind.clone());
                if let Some((id,args))=origin {let args=args.into_iter().map(|t|self.erase_inner(t,depth+1)).collect::<Result<Vec<_>>>()?;self.record(id,&args)?}
                else if let Kind::Array(e,n)=kind {let e=self.erase_inner(e,depth+1)?;self.array(e,n as u64)?}
                // An anonymous record (a tuple is one): rebuilt by its labels (review: a brand inside
                // `(Row<G>, Bool)` made an instance per opening).
                else {let fields=self.aggregates[i as usize].fields.clone();let fields=fields.into_iter().map(|f|Ok((f.name,self.erase_inner(f.ty,depth+1)?))).collect::<Result<Vec<_>>>()?;self.anonymous(fields)?}},
            Ty::Sum(i)=>{let sum=&self.sums[i as usize];let (origin,arms)=(sum.origin.clone(),sum.arms.clone());
                if let Some((id,args))=origin {let args=args.into_iter().map(|t|self.erase_inner(t,depth+1)).collect::<Result<Vec<_>>>()?;self.declared_sum(id,&args)?}
                else if sum.anonymous {let names=sum.names.clone();let variants=names.into_iter().zip(arms).map(|(n,t)|Ok((n,self.erase_inner(t,depth+1)?))).collect::<Result<Vec<_>>>()?;self.union(variants)?}
                else if arms.len()==2 {let a=self.erase_inner(arms[0],depth+1)?;let b=self.erase_inner(arms[1],depth+1)?;self.result([a,b])?}
                else {return Err(error("a sum holding a brand has no way to be rebuilt without it"))}},
            _=>t,
        })
    }
    pub fn is_anonymous(&self,t:Ty)->bool { self.get(t).is_some_and(|a| a.origin.is_none() && matches!(a.kind,Kind::Record(_))) }
    pub fn ty(&mut self, source:&a::Type)->Result<Ty> { self.ty_in(source,FunctionId(u32::MAX),&[]) }
    pub fn ty_in(&mut self, source:&a::Type, owner:FunctionId, names:&[String])->Result<Ty> {
        let params=names.iter().cloned().enumerate().map(|(i,n)|(n,i)).collect();
        let resolved=self.resolve(source,&params)?;
        let args=(0..names.len()).map(|i|Ty::Param{owner,index:i as u32}).collect::<Vec<_>>();
        self.materialize(&resolved,&args)
    }
    pub fn parameter_name_available(&self,name:&str)->bool { !reserved(name)&&!self.names.contains_key(name)&&!self.aliases.contains_key(name)&&!self.sum_names.contains_key(name)&&!self.variants.contains_key(name) }
    /// The same, apart from variant names: a type parameter is written where types are, a
    /// variant where values are (check.rs asks for the variants of the parameter's own file).
    pub fn parameter_name_free_of_types(&self,name:&str)->bool { !reserved(name)&&!self.names.contains_key(name)&&!self.aliases.contains_key(name)&&!self.sum_names.contains_key(name) }
    /// Whether a value of this type holds a `&mut` reference.
    /// Whether two types written in one function's signature are the same type inside that
    /// function. Its type parameters are opaque there: one is equal only to itself.
    fn same_inside(&self, a: Ty, b: Ty, depth: u32) -> bool {
        if a == b { return true; }
        // Past a fixed depth the answer is the careful one.
        if depth > 16 { return true; }
        let all = |this: &Self, xs: Vec<Ty>, ys: Vec<Ty>| xs.len() == ys.len() && xs.into_iter().zip(ys).all(|(x, y)| this.same_inside(x, y, depth + 1));
        match (a, b) {
            (Ty::Weak(i), Ty::Weak(j)) | (Ty::Boxed(i), Ty::Boxed(j)) | (Ty::Buffer(i), Ty::Buffer(j)) => self.buffers[i as usize].dense == self.buffers[j as usize].dense && self.same_inside(self.buffers[i as usize].element, self.buffers[j as usize].element, depth + 1),
            (Ty::Aggregate(i), Ty::Aggregate(j)) => {
                let (x, y) = (&self.aggregates[i as usize], &self.aggregates[j as usize]);
                match (&x.origin, &y.origin) {
                    (Some((left, xs)), Some((right, ys))) => left == right && all(self, xs.clone(), ys.clone()),
                    (None, None) => all(self, x.fields.iter().map(|f| f.ty).collect(), y.fields.iter().map(|f| f.ty).collect()),
                    _ => false,
                }
            }
            (Ty::Sum(i), Ty::Sum(j)) => {
                let (x, y) = (&self.sums[i as usize], &self.sums[j as usize]);
                match (&x.origin, &y.origin) {
                    (Some((left, xs)), Some((right, ys))) => left == right && all(self, xs.clone(), ys.clone()),
                    (None, None) => all(self, x.arms.clone(), y.arms.clone()),
                    _ => false,
                }
            }
            _ => false,
        }
    }
    /// The type parameters a type mentions: in its type arguments and in its parts.
    pub(super) fn parameters_of(&self, t: Ty) -> Vec<Ty> {
        let mut pending = vec![t];
        let mut seen = Vec::new();
        let mut out = Vec::new();
        while let Some(t) = pending.pop() {
            if seen.contains(&t) { continue; }
            seen.push(t);
            match t {
                Ty::Param { .. } => out.push(t),
                Ty::Sum(i) => { let sum = &self.sums[i as usize]; pending.extend(sum.arms.iter().copied()); if let Some((_, args)) = &sum.origin { pending.extend(args.iter().copied()); } }
                Ty::Aggregate(i) => { let record = &self.aggregates[i as usize]; pending.extend(record.fields.iter().map(|f| f.ty)); if let Some((_, args)) = &record.origin { pending.extend(args.iter().copied()); } }
                Ty::Boxed(i) | Ty::Buffer(i) => pending.push(self.buffers[i as usize].element),
                Ty::Borrow { pointee, .. } => pending.push(pointee.ty()),
                _ => {}
            }
        }
        out
    }
    /// Whether, inside a function, a reference to a `part` can point into a value of type
    /// `whole`, both as the function's signature writes them. It can when `whole` is a `part`
    /// or has one as a field, a variant's value, or an array or buffer element, at any depth.
    /// A part of `whole` whose type is a type parameter is opaque to the function, which can
    /// reach inside it only through a trait method. A trait's signature names `Self`, fixed
    /// types and the trait's own type arguments, so such a part may yield a reference to any
    /// `part` that mentions no type parameter but that one and those its bounds give their
    /// traits as type arguments (`open`): with `B: Leak<A>`, `leak` can hand out an `&A`.
    pub fn can_hold(&self, whole: Ty, part: Ty, open: &dyn Fn(Ty) -> Vec<Ty>) -> bool {
        let named = self.parameters_of(part);
        let mut pending = vec![whole];
        let mut seen = Vec::new();
        while let Some(t) = pending.pop() {
            if seen.contains(&t) { continue; }
            seen.push(t);
            if self.same_inside(t, part, 0) { return true; }
            match t {
                Ty::Param { .. } => { let reached = open(t); if named.iter().all(|parameter| *parameter == t || reached.contains(parameter)) { return true; } },
                Ty::Sum(i) => pending.extend(self.sums[i as usize].arms.iter().copied()),
                Ty::Aggregate(i) => pending.extend(self.aggregates[i as usize].fields.iter().map(|f| f.ty)),
                Ty::Boxed(i) | Ty::Buffer(i) => pending.push(self.buffers[i as usize].element),
                // A reference field reaches what it points at: a view lent by `&` lends what it
                // borrows (docs/design-references-to-views.md).
                Ty::Borrow { pointee, .. } => pending.push(pointee.ty()),
                // A slice holds its elements.
                Ty::Slice(i) => pending.push(self.slice_element(i)),
                _ => {}
            }
        }
        false
    }
    /// Every reference type a value of this type is or holds: whether it is `&mut`, and what it points at.
    pub fn references(&self, t: Ty) -> Vec<(bool, Ty)> {
        let mut pending = vec![t];
        let mut seen = Vec::new();
        let mut out = Vec::new();
        while let Some(t) = pending.pop() {
            if seen.contains(&t) { continue; }
            seen.push(t);
            match t {
                // A reference to a view also holds the view's own references; a slice reaches its
                // elements (adversarial review round 18: a concrete `-> &[U64]` wrapper of
                // `Vec.as_slice` dropped the Vec's loan, since a Vec holds no `[U64]` itself).
                Ty::Borrow { mutable, pointee } => {
                    out.push((mutable, pointee.ty()));
                    if let Ty::Slice(i) = pointee.ty() { out.push((mutable, self.slice_element(i))); }
                    pending.push(pointee.ty());
                }
                Ty::Sum(i) => pending.extend(self.sums[i as usize].arms.iter().copied()),
                Ty::Aggregate(i) => pending.extend(self.aggregates[i as usize].fields.iter().map(|f| f.ty)),
                Ty::Boxed(i) | Ty::Buffer(i) => pending.push(self.buffers[i as usize].element),
                _ => {}
            }
        }
        out
    }
    /// Whether a reference to `pointee` (a `[T]` reaching its elements) could point into the
    /// storage of a value of type `t` itself: its fields, elements and arms, and what its Buffers
    /// own, but not past a reference it holds, which points at another's storage. A type
    /// parameter could be anything, so it could.
    pub fn owns_storage_of(&self, t: Ty, pointee: Ty) -> bool {
        let element = if let Ty::Slice(i) = pointee { Some(self.slice_element(i)) } else { None };
        let mut pending = vec![t];
        let mut seen = Vec::new();
        while let Some(t) = pending.pop() {
            if seen.contains(&t) { continue; }
            seen.push(t);
            if t == pointee || Some(t) == element { return true; }
            match t {
                Ty::Param { .. } => return true,
                Ty::Borrow { .. } => {}
                Ty::Sum(i) => pending.extend(self.sums[i as usize].arms.iter().copied()),
                Ty::Aggregate(i) => { let a = &self.aggregates[i as usize]; if let Kind::Array(e, _) = a.kind { pending.push(e); } pending.extend(a.fields.iter().map(|f| f.ty)); }
                Ty::Boxed(i) | Ty::Buffer(i) => pending.push(self.buffers[i as usize].element),
                Ty::Slice(i) => pending.push(self.slice_element(i)),
                _ => {}
            }
        }
        false
    }
    /// The fields of a record `pointee` (both types as a signature writes them, its type
    /// parameters opaque) that no reference in `result` can point at or into: a field that
    /// is a number or a Bool, of a type no reference in the result points at, when no reference
    /// in the result points at the record itself. A reference reaches the storage of a value of
    /// its own type, never a sibling field's: so a write through what `result` holds, however
    /// late, leaves these fields as they were. Empty when `pointee` is not a record.
    pub fn unreachable_scalar_fields(&self, pointee: Ty, result: Ty) -> Vec<usize> {
        let Some(record) = self.get(pointee) else { return Vec::new() };
        if !matches!(record.kind, Kind::Record(_)) { return Vec::new(); }
        let parts = self.references(result);
        if parts.iter().any(|(_, part)| self.same_inside(pointee, *part, 0)) { return Vec::new(); }
        record.fields.iter().enumerate()
            .filter(|(_, field)| matches!(field.ty, Ty::Unsigned(_) | Ty::I64 | Ty::Bool) && !parts.iter().any(|(_, part)| self.same_inside(field.ty, *part, 0)))
            .map(|(ordinal, _)| ordinal).collect()
    }
    /// Whether a reference in a function's result can point into what one of its reference
    /// parameters borrows, both types as the signature writes them. A `&mut` comes only from
    /// a `&mut`; a reference to an `A` points only into a value that can hold an `A`. A
    /// parameter that is not a reference is not judged here.
    /// `open`: for a type parameter, the type parameters its bounds name as type arguments.
    pub fn may_lend_to(&self, parameter: Ty, result: Ty, open: &dyn Fn(Ty) -> Vec<Ty>) -> bool {
        let Ty::Borrow { mutable, pointee } = parameter else { return true };
        self.references(result).into_iter().any(|(unique, part)| (mutable || !unique) && self.can_hold(pointee.ty(), part, open))
    }
    /// Whether a value of this type is, or holds, a `&` reference.
    pub fn lends_shared(&self, t: Ty) -> bool {
        // Most types asked about are numbers: answered without the walk's two allocations.
        match t { Ty::Borrow { mutable, .. } => return !mutable, Ty::Sum(_) | Ty::Aggregate(_) | Ty::Boxed(_) | Ty::Buffer(_) => {}, _ => return false }
        let mut pending = vec![t];
        let mut seen = Vec::new();
        while let Some(t) = pending.pop() {
            if seen.contains(&t) { continue; }
            seen.push(t);
            match t {
                Ty::Borrow { mutable: false, .. } => return true,
                Ty::Sum(i) => pending.extend(self.sums[i as usize].arms.iter().copied()),
                Ty::Aggregate(i) => pending.extend(self.aggregates[i as usize].fields.iter().map(|f| f.ty)),
                Ty::Boxed(i) | Ty::Buffer(i) => pending.push(self.buffers[i as usize].element),
                _ => {}
            }
        }
        false
    }
    pub fn lends_unique(&self, t: Ty) -> bool {
        match t { Ty::Borrow { mutable, .. } => return mutable, Ty::Sum(_) | Ty::Aggregate(_) | Ty::Boxed(_) | Ty::Buffer(_) => {}, _ => return false }
        let mut pending = vec![t];
        let mut seen = Vec::new();
        while let Some(t) = pending.pop() {
            if seen.contains(&t) { continue; }
            seen.push(t);
            match t {
                Ty::Borrow { mutable: true, .. } => return true,
                Ty::Sum(i) => pending.extend(self.sums[i as usize].arms.iter().copied()),
                Ty::Aggregate(i) => pending.extend(self.aggregates[i as usize].fields.iter().map(|f| f.ty)),
                Ty::Boxed(i) | Ty::Buffer(i) => pending.push(self.buffers[i as usize].element),
                _ => {}
            }
        }
        false
    }
    /// Whether a buffer inside this type holds elements that own something (a `Vec<String>`):
    /// such a value cannot be closed as it is, only discarded element by element.
    pub fn holds_owning_buffer(&self, t: Ty) -> bool {
        let mut pending = vec![t];
        let mut seen = Vec::new();
        while let Some(t) = pending.pop() {
            if seen.contains(&t) { continue; }
            seen.push(t);
            match t {
                Ty::Boxed(i) | Ty::Buffer(i) => if !self.properties(self.buffers[i as usize].element).copy { return true; },
                Ty::Sum(i) => pending.extend(self.sums[i as usize].arms.iter().copied()),
                Ty::Aggregate(i) => pending.extend(self.aggregates[i as usize].fields.iter().map(|f| f.ty)),
                _ => {}
            }
        }
        false
    }
    /// Whether a value of this type is, or holds, a record with a reference field.
    pub fn views(&self, t: Ty) -> bool {
        if !matches!(t, Ty::Sum(_) | Ty::Aggregate(_)) { return false; }
        let mut pending = vec![t];
        let mut seen = Vec::new();
        while let Some(t) = pending.pop() {
            if seen.contains(&t) { continue; }
            seen.push(t);
            match t {
                Ty::Sum(i) => pending.extend(self.sums[i as usize].arms.iter().copied()),
                Ty::Aggregate(i) => { let fields = &self.aggregates[i as usize].fields;
                    if fields.iter().any(|f| matches!(f.ty, Ty::Borrow { .. })) { return true; }
                    pending.extend(fields.iter().map(|f| f.ty)) },
                _ => {}
            }
        }
        false
    }
    /// Whether a value of this type holds a reference, and so keeps its source lent.
    pub fn lends(&self, t: Ty) -> bool {
        // Most types asked about are numbers: answered without the walk's two allocations.
        match t { Ty::Borrow { .. } => return true, Ty::Sum(_) | Ty::Aggregate(_) | Ty::Boxed(_) | Ty::Buffer(_) => {}, _ => return false }
        // Walk identities, not expanded type names. Recursive owners and shared
        // subgraphs must terminate, and a reference at any depth must be seen.
        let mut pending = vec![t];
        let mut seen = Vec::new();
        while let Some(t) = pending.pop() {
            if seen.contains(&t) { continue; }
            seen.push(t);
            match t {
                Ty::Borrow { .. } => return true,
                Ty::Sum(i) => pending.extend(self.sums[i as usize].arms.iter().copied()),
                Ty::Aggregate(i) => pending.extend(self.aggregates[i as usize].fields.iter().map(|f| f.ty)),
                Ty::Boxed(i) | Ty::Buffer(i) => pending.push(self.buffers[i as usize].element),
                _ => {}
            }
        }
        false
    }
    /// Storage does not carry loan origins. In particular, recursive placeholders
    /// can have been accepted before their fields/variants were materialized;
    /// recheck the completed graph before admitting a checked program.
    pub fn validate_reference_storage(&self) -> Result<()> {
        // A record may hold `&` references; it is then a view and is never stored. So what must
        // hold of the finished graph is that nothing stored, and nothing a reference points at,
        // holds a reference.
        let pointee = |t: &Ty| matches!(t, Ty::Borrow { pointee, .. } if self.lends(pointee.ty()));
        if self.buffers.iter().any(|b| self.lends(b.element))
            || self.aggregates.iter().any(|a| match a.kind {
                Kind::Record(_) => a.fields.iter().any(|f| pointee(&f.ty) || matches!(f.ty, Ty::Borrow { mutable: true, .. })),
                _ => a.fields.iter().any(|f| self.lends(f.ty)) })
            || self.sums.iter().any(|s| s.arms.iter().any(pointee)) {
            return Err(error("stored values cannot contain references, including inside nested sums"));
        }
        for (index, a) in self.aggregates.iter().enumerate() {
            if self.lends(Ty::Aggregate(index as u32)) && a.origin.as_ref().is_some_and(|(id, _)| !self.definitions[*id].invariants.is_empty()) {
                return Err(error("a type that holds a reference states no invariant: no proof reads it yet"));
            }
        }
        Ok(())
    }
    /// The type a proof reads in place of this one: a shared reference stands for the value
    /// it points at, which cannot change while the reference is in use.
    pub fn plain(&mut self,t:Ty)->Result<Ty> { match t {
        Ty::Borrow{pointee,..}=>Ok(pointee.ty()),
        Ty::Sum(_) if self.lends(t)=>{ let Some(sum)=self.result_sum(t) else { return Ok(t); };
            let mut arms=sum.arms; for arm in &mut arms { if let Ty::Borrow{pointee,..}=*arm { *arm=pointee.ty(); } }
            self.result(arms) },
        _=>Ok(t) } }
    pub fn copy_storage(&self, ty: Ty) -> Result<()> {
        if !self.properties(ty).copy || matches!(ty,Ty::Unit|Ty::Borrow{..}) || self.lends(ty) {
            // A closure that took over an owner is called once and consumed; a `fn(...)` parameter
            // may be called any number of times.
            if self.record_name(ty).is_some_and(|n| n.starts_with("Closure#")) && !self.properties(ty).copy {
                return Err(error("record/Buffer type arguments require non-unit Copy storage without ownership: this closure took over what it captured, so it is called once, by itself (`f(x)`); it cannot be given where a `fn(...)` is expected, which may be called again"));
            }
            // Name the type, and say what to do instead (most often an element of a Vec of owners:
            // `keys.get(i)` copies one out).
            if !self.properties(ty).copy && !matches!(ty, Ty::Unit | Ty::Borrow { .. }) && let Ok(shown) = self.type_name(ty) {
                return Err(error(format!("this copies a `{shown}`, which is not copied (it owns something): lend an element instead (`v.cell(i)`, `&v[i]`), or move it out (`take_last_mut`, `replace_mut`); to give back a whole container of owners, call `discard()` (it closes each element), not `close()`. (In the checker's terms: record/Buffer type arguments require non-unit Copy storage without ownership)")));
            }
            return Err(error("record/Buffer type arguments require non-unit Copy storage without ownership")); } Ok(())
    }
    /// The file that owns a sealed record type.
    pub fn sealed_in(&self,t:Ty)->Option<u32> { self.get(t)?.origin.as_ref().and_then(|(id,_)|self.definitions[*id].sealed) }
    /// The declaring file and stated invariants of a sealed record type that has any.
    pub fn invariants(&self,t:Ty)->Option<(u32,&[a::Expr])> { let (id,_)=self.get(t)?.origin.as_ref()?;let d=&self.definitions[*id];
        if d.invariants.is_empty(){None}else{Some((d.sealed?,&d.invariants))} }
    pub fn declare_any(&mut self,owner:FunctionId,index:u32) { self.any.insert((owner.0,index)); }
    /// A type argument of a generic function. `()` is one too: a function stores nothing by
    /// itself, and a record or Buffer it builds from the argument is checked where it is made.
    pub fn function_argument(&self, ty: Ty, any: bool) -> Result<()> { self.argument(ty, any, false) }
    /// Whether a function's type parameter `index` appears only as the whole type of parameters
    /// `&T`, and nowhere in its result. Generic code that holds a `T` only behind a shared
    /// reference cannot copy, store or return one, so a view may be that `T` (`map.get(&span)`).
    pub fn only_lent(&self, params: &[Ty], result: Ty, owner: FunctionId, index: u32) -> bool {
        let this = Ty::Param { owner, index };
        // `parameters_in` marks every parameter of `owner` it meets: room for all of them.
        let mentions = |t: Ty| { let mut used = vec![false; 4096]; self.parameters_in(t, owner, &mut used); used.get(index as usize).copied().unwrap_or(true) };
        if mentions(result) { return false; }
        params.iter().all(|p| match p {
            Ty::Borrow { mutable: false, pointee } if pointee.ty() == this => true,
            other => !mentions(*other),
        })
    }
    /// A type argument for a parameter of a function type (`f: fn(U64) -> U64`, or `Self` of
    /// a call trait's `call`): it may hold authority, which `wasm-caps` follows through the
    /// call's type arguments (docs/design-closure-authority.md).
    pub fn callable_argument(&self, ty: Ty, any: bool) -> Result<()> { self.argument(ty, any, true) }
    /// A type argument for work to run once (`W: WitchyOnce0<()>`): moved in and consumed, so it
    /// may hold authority and owners; never a reference, which a task could outlive.
    /// Without `Any` the parameter is copied like any other, so its argument must be Copy: a
    /// closure that took over an owner or a capability is not (review: `W: WitchyOnce0<()>` ran
    /// one twice).
    pub fn work_argument(&self, ty: Ty, any: bool) -> Result<()> {
        if self.shared_view(ty) || matches!(ty, Ty::Borrow { .. }) { return Err(error("work to run once takes over what it needs, so it holds no reference")); }
        if !any { return self.copy_storage(ty).map_err(|_| error("work that takes over an owner or a capability is given to `W: Any + WitchyOnce0<R>`: without `Any` it could be copied and run twice")); }
        self.storable(ty)
    }
    fn argument(&self, ty: Ty, any: bool, callable: bool) -> Result<()> {
        if ty==Ty::Unit { return Ok(()); }
        // Generic code is given no authority its signature does not show: what `wasm-caps` and
        // `pure fn` read from signatures depends on it.
        // A closure holding a reference (a view) is passed by value only; lending it would need
        // loans two levels deep (adversarial review round 20: the old message pointed nowhere).
        if callable && self.shared_view(ty) && self.authority_in(ty,0) { return Err(error("a closure that holds a reference to a capability is called where it is made, not passed on yet: capture the capability itself (it is moved in) and lend the closure as `&fn(...)`")); }
        if any && self.shared_view(ty) { return Err(error("a value that holds a reference (a closure that captured one, a Span) is passed by value, not lent through `&` to a type parameter that may be anything (`T: Any`, `&fn(...)`)")); }
        if callable && !any && self.authority_in(ty,0) { return Err(error("a closure that holds a capability is lent, not handed over: take it as `&fn(...)` and pass `&closure`")); }
        // A closure's argument may be a shared reference (`fn(&String) -> Bool`): the call
        // gives a result that holds none (closure.rs), so nothing it points at escapes.
        if callable && matches!(ty, Ty::Borrow { mutable: false, .. }) { return Ok(()); }
        if !callable && self.authority_in(ty,0) { return Err(error("a capability, or a value that holds one, is not a type argument: generic code would hold authority its signature does not show")); }
        if !any && self.shared_view(ty) { return Ok(()); }
        // Borrowed UTF-8 is a view as a record holding it is: `console.show(text)`, `"${text}"`.
        if !any && self.str_view(ty) { return Ok(()); }
        if any { self.storable(ty) } else { self.copy_storage(ty) }
    }
    /// A value that can live in a Buffer cell or a record field: Copy storage, or one owner.
    /// The element type of a slice type.
    pub fn slice_element(&self, id: u32) -> Ty {
        match self.slices[id as usize] {
            SliceType::Elements(element) => element,
            SliceType::Utf8 => Ty::Unsigned(Scalar::U8),
        }
    }
    pub fn literal_bytes(&self, id: u64) -> Option<std::sync::Arc<str>> {
        self.text_literals.get(usize::try_from(id).ok()?).cloned()
    }
    pub fn str_borrow(&mut self) -> Ty {
        let id = self.intern_slice(SliceType::Utf8);
        Ty::Borrow { mutable: false, pointee: super::logical_body::Pointee::Slice(id) }
    }
    /// `&str`, named without interning: `built` interned it first.
    pub fn known_str_borrow(&self) -> Option<Ty> {
        let id = self.slices.iter().position(|kind| *kind == SliceType::Utf8)? as u32;
        Some(Ty::Borrow { mutable: false, pointee: super::logical_body::Pointee::Slice(id) })
    }
    pub fn is_str(&self, id: u32) -> bool { self.slices[id as usize] == SliceType::Utf8 }
    fn intern_slice(&mut self, slice: SliceType) -> u32 {
        if let Some(index) = self.slices.iter().position(|kind| *kind == slice) {
            return index as u32;
        }
        let id = self.slices.len() as u32;
        self.slices.push(slice);
        id
    }
    /// The slice type `[element]`: its elements are stored values, never references.
    pub fn slice_of(&mut self, element: Ty) -> Result<Ty> {
        if matches!(element,Ty::Unit|Ty::Borrow{..}|Ty::Slice(_)) || self.lends(element) { return Err(error("a slice's elements are stored values, not references")); }
        // Stored data, never authority: no array or heap cell holds a capability either.
        if self.authority_in(element,0) { return Err(error("a slice's elements are data, never capabilities")); }
        Ok(Ty::Slice(self.intern_slice(SliceType::Elements(element))))
    }
    pub fn storable(&self, ty: Ty) -> Result<()> {
        // A reference as `T` (`Result<&Json, U64>.is_err()`, whose `T: Any` is a stored value).
        if matches!(ty,Ty::Borrow{..}|Ty::Slice(_)) {
            return Err(error("a type argument here is a stored value, and this one is a reference (as in `Result<&T, E>`): `match` on it instead of calling a generic method")); }
        // A view that holds a reference (`Vec<Span>`).
        if self.lends(ty) {
            return Err(error("this type holds a reference (a view such as `Span`), and a Vec or record stores values: store an owner (a `String`) or the positions instead")); }
        if matches!(ty,Ty::Unit) || !(self.properties(ty).copy || self.supported_linear(ty)) {
            return Err(error("record/Buffer type arguments require non-unit storage: Copy values or one owner")); } Ok(())
    }
    /// A record that holds shared references and no unique one (`Span`): a view, copied
    /// freely, whose loans the checker follows. It may be a field (`anonymous`), a generic
    /// record's type argument, and a function's copied type argument; never a Buffer element.
    /// It is copied, so it holds no owner beside its references (found by an automated
    /// security review: a view beside a Buffer would be duplicated by generic code).
    /// `&str`: borrowed UTF-8, which generic code takes by value as it takes a `Span`.
    pub fn str_view(&self, t: Ty) -> bool { matches!(t, Ty::Borrow { mutable: false, pointee: super::logical_body::Pointee::Slice(id) } if self.is_str(id)) }
    /// `str` itself, which generic code holds only behind `&` (`map.get(text)` with `Q = str`).
    pub fn unsized_str(&self, t: Ty) -> bool { matches!(t, Ty::Slice(id) if self.is_str(id)) }
    pub fn shared_view(&self, t: Ty) -> bool { matches!(t, Ty::Aggregate(_)) && self.lends(t) && !self.lends_unique(t) && self.properties(t).copy }
    fn record(&mut self, id: usize, args: &[Ty]) -> Result<Ty> {
        for t in args { if !self.shared_view(*t) && !self.str_view(*t) { self.storable(*t)?; } }
        const ITSELF:&str="a record type can contain itself only inside a Buffer: its values have a fixed size";
        if let Some((_,_,ty))=self.instances.iter().find(|(d,a,_)| *d==id && a==args) {
            let Ty::Aggregate(found)=*ty else { unreachable!() };
            if self.aggregates[found as usize].building { self.aggregates[found as usize].recursive=true; }
            return Ok(*ty);
        }
        if self.aggregates.iter().filter(|a| a.building).count()>=16 { return Err(error(ITSELF)); }
        // Symbolic identity is (definition,args); never expand argument names while interning.
        // The type exists before its fields do, so that a Buffer inside it can name it.
        let name=self.definitions[id].name.clone();
        let concrete=args.iter().all(|t|self.concrete(*t));
        let found=self.aggregates.len(); let ty=Ty::Aggregate(found as u32);
        self.aggregates.push(Aggregate { kind:Kind::Record(name), fields:vec![], origin:Some((id,args.to_vec())), concrete,
            properties:Properties{copy:false,implicit_discard:false}, building:true, recursive:false });
        self.instances.push((id,args.to_vec(),ty));
        let fields=self.definitions[id].fields.clone().into_iter().map(|(name,t)| Ok(Field { name, ty:self.materialize(&t,args)? })).collect::<Result<Vec<_>>>()?;
        // A record holds references to stored values only: a reference to a view is a local's
        // or a parameter's, so loans stay at most two levels deep
        // (docs/design-references-to-views.md).
        if fields.iter().any(|f| matches!(f.ty, Ty::Borrow{pointee,..} if self.lends(pointee.ty()))) {
            return Err(error("a reference points at a storable value, not at a value that holds a reference: a record field cannot hold a reference to a view"));
        }
        let recursive=self.aggregates[found].recursive;
        if recursive && fields.iter().any(|f| self.holds_inline(f.ty,ty,0)) { return Err(error(ITSELF)); }
        let properties=if recursive { Properties{copy:false,implicit_discard:false} } else { self.combine(fields.iter().map(|f|f.ty)) };
        let record=&mut self.aggregates[found]; record.fields=fields; record.properties=properties; record.building=false;
        self.order.push(ty); Ok(ty)
    }
    pub fn has_record(&self,name:&str)->bool { self.names.contains_key(name) }
    pub fn has_alias(&self,name:&str)->bool { self.aliases.contains_key(name) }
    /// A declared record's field names, in order: what labels in `Record(name: value)` name.
    pub fn record_field_names(&self,name:&str)->Option<Vec<String>> { let id=*self.names.get(name)?; Some(self.definitions[id].fields.iter().map(|(n,_)|n.clone()).collect()) }
    pub fn named_record(&self,name:&str)->Option<Ty> { let id=self.names.get(name)?;
        self.instances.iter().find(|(d,a,_)| d==id && a.is_empty()).map(|(_,_,t)| *t) }
    pub fn constructor(&self,name:&str,expected:Option<Ty>)->Result<Ty> {
        if let Some(t)=self.named_record(name) { return Ok(t); }
        let id=self.names.get(name);
        // A `${...}` text is such a record (parser/interpolation.rs): where something else is
        // expected, say what it is rather than how to settle a generic record.
        if name.starts_with("Format#") && self.instances.iter().all(|(d,_,t)| !(Some(d)==id && Some(*t)==expected)) {
            return Err(error("scalar expression type mismatch: a text with `${...}` is a format, not a `&str`; print it with `console.print(...)`, write it without an end of line with `console.write_format(...)`, or add it to a String with `push_format`"));
        }
        self.instances.iter().find(|(d,_,t)| Some(d)==id && Some(*t)==expected).map(|(_,_,t)| *t)
            .ok_or_else(|| error("generic record constructor requires a matching concrete expected type: write the type on the `let`, or give field values whose types settle every type parameter"))
    }
    /// A generic record's type from the types of the values given for its fields, when they
    /// settle every type parameter: `Pack(move data, 0)` with `data` a `Buffer<U64>` is a
    /// `Pack<U64>`. `None` when a parameter is left open. The constructor's own check then
    /// holds each value to its field's type as usual.
    pub fn constructed(&mut self,name:&str,fields:&[Option<Ty>])->Result<Option<Ty>> {
        let Some(&id)=self.names.get(name) else { return Ok(None) };
        let d=&self.definitions[id];
        if d.parameters.is_empty()||d.fields.len()!=fields.len() { return Ok(None); }
        let mut solved=vec![None;d.parameters.len()];
        let patterns=d.fields.iter().map(|(_,t)|t.clone()).collect::<Vec<_>>();
        for(pattern,actual)in patterns.iter().zip(fields){ if let Some(actual)=actual { self.settle(pattern,*actual,&mut solved,0); } }
        let Some(args)=solved.into_iter().collect::<Option<Vec<_>>>() else { return Ok(None) };
        self.record(id,&args).map(Some)
    }
    /// The type parameters a field's declared type fixes, given the type of the value for it.
    fn settle(&self,pattern:&TemplateType,actual:Ty,solved:&mut [Option<Ty>],depth:usize) {
        use TemplateType as T;
        if depth>16 { return; }
        match pattern {
            T::Param(i) => { if let Some(slot)=solved.get_mut(*i) { if slot.is_none() { *slot=Some(actual); } } }
            T::Weak(element) => if let Ty::Weak(b)=actual { let held=self.buffers[b as usize].element; self.settle(element,held,solved,depth+1); },
            T::Boxed(element) => if let Ty::Boxed(b)=actual { let held=self.buffers[b as usize].element; self.settle(element,held,solved,depth+1); },
            T::Buffer(element) | T::Prefix(element) => if let Ty::Buffer(b)=actual { if self.buffers[b as usize].dense==matches!(pattern,T::Prefix(_)) { let held=self.buffers[b as usize].element; self.settle(element,held,solved,depth+1); } },
            T::Record(id,args) => if let Some(Aggregate{origin:Some((origin,given)),..})=self.get(actual) {
                if origin==id { for(pattern,actual)in args.iter().zip(given.clone()){ self.settle(pattern,actual,solved,depth+1); } }
            },
            // Through a reference, and through a slice to its elements: `Cursor(numbers.slice(), 0)`.
            T::Shared(inner)|T::Unique(inner) => match (&**inner,actual) {
                (T::Slice(element),Ty::Borrow{pointee:super::logical_body::Pointee::Slice(i),..}) if !self.is_str(i) => self.settle(element,self.slice_element(i),solved,depth+1),
                (inner,Ty::Borrow{pointee,..}) => self.settle(inner,pointee.ty(),solved,depth+1),
                _ => {}
            },
            _ => {}
        }
    }
    pub fn array(&mut self,element:Ty,n:u64)->Result<Ty> {
        if n==0 || n>256 { return Err(error("fixed array length must be 1..256")); } self.copy_storage(element)?;
        let kind=Kind::Array(element,n as u32);
        if let Some(id)=self.aggregates.iter().position(|a| a.kind==kind) { return Ok(Ty::Aggregate(id as u32)); }
        let fields=(0..n).map(|i| Field { name:i.to_string(),ty:element }).collect();
        let properties=self.properties(element);let concrete=self.concrete(element);
        let id=self.aggregates.len(); self.aggregates.push(Aggregate { kind, fields, origin:None, concrete, properties, building:false, recursive:false }); let ty=Ty::Aggregate(id as u32); self.order.push(ty); Ok(ty)
    }
    pub fn result(&mut self,arms:[Ty;2])->Result<Ty> {
        if let Some(id)=self.sums.iter().position(|s| s.origin.is_none() && !s.anonymous && s.arms==arms) { return Ok(Ty::Sum(id as u32)); }
        self.intern_sum(arms.to_vec(),vec!["Ok".into(),"Err".into()],None)
    }
    /// The anonymous sum with exactly these tags and payloads. Tag order does not matter and there is one per shape.
    pub fn union(&mut self, mut variants: Vec<(String,Ty)>) -> Result<Ty> {
        variants.sort_by(|a,b| a.0.cmp(&b.0));
        if variants.is_empty() || variants.len()>32 { return Err(error("an anonymous sum has between 1 and 32 tags")); }
        if variants.windows(2).any(|w| w[0].0==w[1].0) { return Err(error("an anonymous sum names each tag once")); }
        let same=|s:&Sum| s.anonymous && s.names.len()==variants.len() && s.names.iter().zip(&s.arms).zip(&variants).all(|((n,t),(m,u))| n==m && t==u);
        if let Some(id)=self.sums.iter().position(same) { return Ok(Ty::Sum(id as u32)); }
        let (names,arms)=variants.into_iter().unzip();
        let ty=self.intern_sum(arms,names,None)?;
        let Ty::Sum(id)=ty else { unreachable!() }; self.sums[id as usize].anonymous=true; Ok(ty)
    }
    /// How a value of the anonymous sum `from` becomes one of `to`: for each tag of `from`, its tag in `to`.
    /// Every tag of `from` must be in `to` and hold the same type. `None` when it is not a widening.
    pub fn widening(&self,from:Ty,to:Ty)->Option<Vec<u8>> {
        let (a,b)=(self.sum(from)?,self.sum(to)?);
        if !a.anonymous || !b.anonymous || from==to { return None; }
        a.names.iter().zip(&a.arms).map(|(name,arm)| b.names.iter().position(|n| n==name).filter(|at| b.arms[*at]==*arm).map(|at| at as u8)).collect()
    }
    fn intern_sum(&mut self,arms:Vec<Ty>,names:Vec<String>,origin:Option<(usize,Vec<Ty>)>)->Result<Ty> {
        let properties=self.combine(arms.iter().copied());
        let concrete=arms.iter().all(|t|self.concrete(*t))&&origin.iter().all(|(_,args)|args.iter().all(|t|self.concrete(*t)));
        let id=self.sums.len(); self.sums.push(Sum { arms, names, origin, concrete, properties, anonymous: false, building: false, recursive: false }); let ty=Ty::Sum(id as u32); self.order.push(ty); Ok(ty)
    }
    /// The declared sum `id` at these type arguments.
    fn declared_sum(&mut self,id:usize,args:&[Ty])->Result<Ty> {
        const ITSELF:&str="a sum type can contain itself only inside a Buffer: its values have a fixed size";
        if let Some(found)=self.sums.iter().position(|s| s.origin.as_ref().is_some_and(|(d,a)| *d==id && a==args)) {
            if self.sums[found].building { self.sums[found].recursive=true; }
            return Ok(Ty::Sum(found as u32));
        }
        if self.sums.iter().filter(|s| s.building).count()>=16 { return Err(error(ITSELF)); }
        // The type exists before its variants do, so that a Buffer inside it can name it.
        // Until then it is an owner of unknown shape; a sum that holds itself is one anyway.
        let variants=self.sum_definitions[id].variants.clone();
        let found=self.sums.len(); let ty=Ty::Sum(found as u32);
        let concrete=args.iter().all(|t|self.concrete(*t));
        self.sums.push(Sum { arms: vec![], names: variants.iter().map(|(n,_)|n.clone()).collect(), origin: Some((id,args.to_vec())), concrete,
            properties: Properties{copy:false,implicit_discard:false}, anonymous: false, building: true, recursive: false });
        let arms=variants.iter().map(|(_,t)| self.materialize(t,args)).collect::<Result<Vec<_>>>()?;
        let recursive=self.sums[found].recursive;
        // Naming itself is fine behind a Buffer, whose handle has a fixed size; held directly, the value would have none.
        if recursive && arms.iter().any(|t| self.holds_inline(*t,ty,0)) { return Err(error(ITSELF)); }
        if recursive && arms.iter().any(|t| self.lends(*t)) { return Err(error("a sum that is stored in a Buffer cannot hold a reference")); }
        let properties=if recursive { Properties{copy:false,implicit_discard:false} } else { self.combine(arms.iter().copied()) };
        let concrete=concrete&&arms.iter().all(|t|self.concrete(*t));
        let sum=&mut self.sums[found]; sum.arms=arms; sum.properties=properties; sum.concrete=concrete; sum.building=false;
        self.order.push(ty); Ok(ty)
    }
    /// Whether a value of type `t` holds a `target` directly, not behind a Buffer.
    fn holds_inline(&self,t:Ty,target:Ty,depth:usize)->bool { t==target || depth<32 && match t {
        Ty::Aggregate(i)=>self.aggregates[i as usize].fields.iter().any(|f| self.holds_inline(f.ty,target,depth+1))
            || matches!(self.aggregates[i as usize].kind,Kind::Array(element,_) if self.holds_inline(element,target,depth+1)),
        Ty::Sum(i)=>self.sums[i as usize].arms.iter().any(|t| self.holds_inline(*t,target,depth+1)),
        _=>false } }
    /// The declared name of a record or sum type: where its methods are looked up.
    /// The type arguments of a declared generic type.
    pub fn arguments(&self,t:Ty)->Vec<Ty> { match t {
        Ty::Aggregate(_)=>self.get(t).and_then(|a|a.origin.as_ref()).map(|(_,args)|args.clone()).unwrap_or_default(),
        Ty::Sum(_)=>self.sum(t).and_then(|s|s.origin.as_ref()).map(|(_,args)|args.clone()).unwrap_or_default(),_=>Vec::new() } }
    /// Renames a record for a report: `wasm-caps` names a closure's record by where it is written,
    /// as it names the closure's methods, so a method reached through the record is found.
    pub fn rename_record(&mut self, old: &str, new: &str) {
        if let Some(id) = self.names.remove(old) { self.definitions[id].name = new.to_owned(); self.names.insert(new.to_owned(), id); }
    }
    pub fn nominal(&self,t:Ty)->Option<&str> { match t {
        Ty::Unsigned(s)=>Some(s.layout().name),Ty::I64=>Some("I64"),Ty::F64=>Some("F64"),Ty::Bool=>Some("Bool"),
        // Both consoles are instances of the one built-in type, whose methods a library may add to.
        Ty::Console|Ty::Input=>Some("Console"),
        // So is the allocator: `impl Allocator` (library/wasm-tasks.wy).
        Ty::Allocator=>Some("Allocator"),
        // Each right is its own namespace of methods: `impl Dir<Read>`.
        Ty::Host(h)=>Some(h.name()),
        // Borrowed UTF-8 is a type a library may add methods and traits to: `impl str`.
        Ty::Slice(id) if self.is_str(id)=>Some("str"),
        // `impl Show for &str`: a trait whose `Self` is a value, given borrowed UTF-8.
        Ty::Borrow{mutable:false,pointee:super::logical_body::Pointee::Slice(id)} if self.is_str(id)=>Some("&str"),
        Ty::Aggregate(_)=>self.get(t)?.origin.as_ref().map(|(id,_)|self.definitions[*id].name.as_str()),
        // The built-in Result is a type a library may add methods to, like the console.
        Ty::Sum(_) if self.result_sum(t).is_some()=>Some("Result"),
        Ty::Sum(_)=>self.sum(t)?.origin.as_ref().map(|(id,_)|self.sum_definitions[*id].name.as_str()),_=>None } }
    /// Whether a value of this type carries a console or allocator, at any depth of records.
    pub fn holds_capability(&self,t:Ty)->bool { self.capability_in(t,0) }
    /// Records nest without cycles (a record holds itself only through a Buffer, which is not
    /// followed here), so the walk ends; past 128 levels it fails closed.
    fn capability_in(&self,t:Ty,depth:usize)->bool { match t {
        Ty::Console|Ty::Input|Ty::Allocator|Ty::Host(_)=>true,
        Ty::Aggregate(_) if depth>=128=>true,
        Ty::Aggregate(_)=>self.get(t).is_some_and(|r|r.fields.iter().any(|f|self.capability_in(f.ty,depth+1))),
        _=>false } }
    /// Like `holds_capability`, and through the arms of sums as well.
    /// Whether a value of this type holds a capability anywhere: in fields, arms, Buffer
    /// elements or through references. Each type is visited once, so recursive types end, and
    /// no depth cutoff is needed (there are at most a few hundred types).
    pub fn authority_in(&self,t:Ty,_depth:usize)->bool { self.authority_walk(t,&mut BTreeSet::new()) }
    fn authority_walk(&self,t:Ty,seen:&mut BTreeSet<(u8,u32)>)->bool {
        let key=match t { Ty::Aggregate(i)=>(0,i), Ty::Sum(i)=>(1,i), Ty::Buffer(i)=>(2,i), Ty::Boxed(i)=>(4,i), _=>(3,0) };
        if key.0<3 && !seen.insert(key) { return false; }
        match t {
            // A task may hold what its input held, but nothing generic can run it: stepping is
            // `Task.step`, which no trait offers, so a `Vec<Task>` moves tasks and never uses
            // what they hold. `wasm-caps` still lists a function holding one, and a `pure fn`
            // takes none (caps.rs).
            Ty::Host(super::host::Host::Task)=>false,
            Ty::Console|Ty::Input|Ty::Allocator|Ty::Host(_)=>true,
            // A library's own authority, granted by the run: not a type argument, not in heap
            // cells, held only where `wasm-caps` sees it (found by an adversarial review).
            Ty::Aggregate(_) if self.nominal(t).is_some_and(|n| self.grantable.contains(n))=>true,
            Ty::Aggregate(_)=>self.get(t).is_some_and(|r|r.fields.iter().any(|f|self.authority_walk(f.ty,seen))),
            Ty::Sum(_)=>self.sum(t).is_some_and(|s|s.arms.iter().any(|a|self.authority_walk(*a,seen))),
            // A type still being built is taken to hold one: the check fails closed.
            Ty::Boxed(i) | Ty::Buffer(i)=>self.buffers.get(i as usize).is_none_or(|b|self.authority_walk(b.element,seen)),
            // A slice holds its elements (found by an automated security review).
            Ty::Slice(i)=>self.authority_walk(self.slice_element(i),seen),
            Ty::Borrow{pointee,..}=>!matches!(pointee,super::logical_body::Pointee::Param(..))&&self.authority_walk(pointee.ty(),seen),
            _=>false,
        }
    }
    pub fn has_variant(&self,name:&str)->bool { self.variants.contains_key(name) }
    pub fn has_sum(&self,name:&str)->bool { self.sum_names.contains_key(name) }
    /// The sum type and tag a declared variant name builds. A generic sum needs the expected type.
    /// The name of the one sum with a variant of this name, when it takes no type arguments (so the
    /// variant alone says its type: `Dot == Dot` is `Shape.eq(&Dot, &Dot)`).
    /// The name a record or sum type was declared with, as a static call names it (`Shape.eq`),
    /// when the name finds that declaration.
    pub fn declared_name(&self,t:Ty)->Option<String> {
        match t {
            Ty::Sum(_) => { let (id,_)=self.sum(t)?.origin.as_ref()?; let name=&self.sum_definitions.get(*id)?.name; (self.sum_names.get(name)==Some(id)).then(|| name.clone()) }
            Ty::Aggregate(_) => { let (id,_)=self.get(t)?.origin.as_ref()?; let name=&self.definitions.get(*id)?.name; (self.names.get(name)==Some(id)).then(|| name.clone()) }
            _ => None,
        }
    }
    /// Whether some sum has a variant of this name (a generic one's included).
    pub fn is_variant(&self,name:&str)->bool { self.variants.contains_key(name) }
    pub fn plain_sum_of_variant(&self,name:&str)->Option<String> {
        let [(id,_)]=self.variants.get(name)?.as_slice() else { return None };
        let definition=&self.sum_definitions[*id];
        definition.parameters.is_empty().then(|| definition.name.clone())
    }
    pub fn variant(&self,name:&str,expected:Option<Ty>)->Result<(Ty,u8)> {
        let candidates=self.variants.get(name).ok_or_else(|| error("unknown variant"))?;
        for &(id,tag) in candidates {
            if let Some(t)=expected.filter(|t| self.sum(*t).is_some_and(|s| s.origin.as_ref().is_some_and(|(d,_)| *d==id))) { return Ok((t,tag as u8)); }
        }
        let &[(id,tag)]=candidates.as_slice() else {
            let types=candidates.iter().map(|(id,_)| self.sum_definitions[*id].name.as_str()).collect::<Vec<_>>().join(", ");
            return Err(error(format!("`{name}` is a variant of more than one type ({types}): build it where its type is expected, such as an annotated binding or a parameter")));
        };
        self.variant_at(id,tag,expected)
    }
    /// `Right.Io`: the variant of the sum named, whatever else has a variant of that name.
    pub fn variant_in(&self,sum:&str,name:&str,expected:Option<Ty>)->Result<(Ty,u8)> {
        let id=*self.sum_names.get(sum).ok_or_else(|| error("unknown sum type"))?;
        let &(_,tag)=self.variants.get(name).and_then(|c| c.iter().find(|(d,_)| *d==id)).ok_or_else(|| error("this type has no variant of that name"))?;
        self.variant_at(id,tag,expected)
    }
    pub fn variant_of(&self,sum:&str,name:&str)->bool { self.sum_names.get(sum).is_some_and(|id| self.variants.get(name).is_some_and(|c| c.iter().any(|(d,_)| d==id))) }
    fn variant_at(&self,id:usize,tag:usize,expected:Option<Ty>)->Result<(Ty,u8)> {
        let of=|s:&Sum| s.origin.as_ref().is_some_and(|(d,_)| *d==id);
        if let Some(t)=expected.filter(|t| self.sum(*t).is_some_and(of)) { return Ok((t,tag as u8)); }
        if self.sum_definitions[id].parameters.is_empty() {
            let found=self.sums.iter().position(of).expect("a sum without type parameters is built with the program");
            return Ok((Ty::Sum(found as u32),tag as u8));
        }
        Err(error("a variant of a generic type needs its type from the context: annotate the binding"))
    }
    pub fn get(&self,t:Ty)->Option<&Aggregate> { if let Ty::Aggregate(i)=t { self.aggregates.get(i as usize) } else { None } }
    pub fn sum(&self,t:Ty)->Option<&Sum> { if let Ty::Sum(i)=t { self.sums.get(i as usize) } else { None } }
    /// `Result<A, B>` only: the two-arm sum the proof models know.
    pub fn result_sum(&self,t:Ty)->Option<ResultSum> { self.sum(t).filter(|s| s.origin.is_none() && !s.anonymous).map(|s| ResultSum { arms: [s.arms[0], s.arms[1]] }) }
    pub fn buffers(&self)->&[Buffer] { &self.buffers }
    pub fn owner_field(&self,t:Ty)->Option<&Field> { self.get(t)?.fields.iter().find(|f| !self.properties(f.ty).copy) }
    /// The one owner field of a record that has exactly one.
    pub fn sole_owner_field(&self,t:Ty)->Option<&Field> { let mut owners=self.get(t)?.fields.iter().filter(|f| !self.properties(f.ty).copy);let first=owners.next()?;owners.next().is_none().then_some(first) }
    pub fn record_name(&self,t:Ty)->Option<&str> { match &self.get(t)?.kind { Kind::Record(name)=>Some(name),_=>None } }
    pub fn supported_linear(&self,t:Ty)->bool { match t { Ty::Boxed(_) | Ty::Buffer(_)=>true,
        // A file being written: it must be finished or abandoned, never dropped.
        Ty::Host(super::host::Host::File(super::host::Right::Write)|super::host::Host::Task|super::host::Host::P384Key)=>true,Ty::Param{owner,index}=>self.any.contains(&(owner.0,index)),Ty::Aggregate(i)=>self.aggregates[i as usize].building||self.owner_field(t).is_some(),Ty::Sum(i)=>self.sums[i as usize].building||self.sums[i as usize].arms.iter().any(|t| self.supported_linear(*t)),_=>false } }
    pub fn properties(&self,t:Ty)->Properties { match t {
        // Function parameters carry the explicit fixed CopyStorage family, never a layout.
        Ty::Param{owner,index} if self.any.contains(&(owner.0,index))=>Properties{copy:false,implicit_discard:false},
        Ty::Param{..}=>Properties{copy:true,implicit_discard:true},
        Ty::Host(super::host::Host::File(super::host::Right::Write)|super::host::Host::Task|super::host::Host::P384Key)=>Properties{copy:false,implicit_discard:false},
        Ty::Console|Ty::Input|Ty::Allocator|Ty::Host(_)=>Properties{copy:false,implicit_discard:true},Ty::Borrow{mutable:false,..}=>Properties{copy:true,implicit_discard:true},
        // A unique reference is not copied; dropping one loses nothing.
        Ty::Borrow{..}=>Properties{copy:false,implicit_discard:true},
        Ty::Boxed(_) | Ty::Buffer(_)=>Properties{copy:false,implicit_discard:false},
        Ty::Aggregate(i)=>self.aggregates[i as usize].properties,
        Ty::Sum(i)=>self.sums[i as usize].properties,_=>Properties{copy:true,implicit_discard:true} } }
    fn combine(&self,types:impl Iterator<Item=Ty>)->Properties { types.fold(Properties{copy:true,implicit_discard:true},|p,t| { let next=self.properties(t); Properties{copy:p.copy&&next.copy,implicit_discard:p.implicit_discard&&next.implicit_discard} }) }
    pub fn type_name(&self,t:Ty)->Result<String> {
        let mut out=String::new();self.name_inner(t,&mut out,0,&mut 0)?;Ok(out)
    }
    fn name_append(out:&mut String,s:&str)->Result<()> {
        if out.len().checked_add(s.len()).is_none_or(|n|n>super::MAX_SOURCE_BYTES){return Err(error("semantic type name complexity limit"));}
        out.push_str(s);Ok(())
    }
    fn name_inner(&self,t:Ty,out:&mut String,depth:usize,visits:&mut usize)->Result<()> {
        *visits+=1;if depth>16||*visits>4096{return Err(error("semantic type name complexity limit"));}
        match t {
            Ty::Param{..}=>return Err(error("unbound function type parameter has no canonical concrete name")),
            Ty::Unsigned(s)=>Self::name_append(out,s.layout().name)?,Ty::I64=>Self::name_append(out,"I64")?,Ty::F64=>Self::name_append(out,"F64")?,Ty::Bool=>Self::name_append(out,"Bool")?,
            Ty::Unit=>Self::name_append(out,"()")?,Ty::Console=>Self::name_append(out,"Console<Write>")?,Ty::Input=>Self::name_append(out,"Console<Read>")?,Ty::Host(h)=>Self::name_append(out,h.name())?,Ty::Allocator=>Self::name_append(out,"Allocator")?,
            Ty::Weak(i)=>{Self::name_append(out,"Weak<")?;self.name_inner(self.buffers[i as usize].element,out,depth+1,visits)?;Self::name_append(out,">")?;},
            Ty::Boxed(i)=>{Self::name_append(out,"Boxed<")?;self.name_inner(self.buffers[i as usize].element,out,depth+1,visits)?;Self::name_append(out,">")?;},
            Ty::Buffer(i)=>{Self::name_append(out,if self.buffers[i as usize].dense { "PrefixBuffer<" } else { "Buffer<" })?;self.name_inner(self.buffers[i as usize].element,out,depth+1,visits)?;Self::name_append(out,">")?;},
            Ty::Slice(i) if self.is_str(i) => Self::name_append(out,"str")?,
            Ty::Slice(i)=>{Self::name_append(out,"[")?;self.name_inner(self.slice_element(i),out,depth+1,visits)?;Self::name_append(out,"]")?;},
            Ty::Aggregate(i)=>{let a=&self.aggregates[i as usize];if let Some((_,args))=&a.origin {
                let Kind::Record(name)=&a.kind else{unreachable!()};
                Self::name_append(out,name)?;
                if !args.is_empty(){Self::name_append(out,"<")?;for(i,t)in args.iter().enumerate(){if i!=0{Self::name_append(out,", ")?;}self.name_inner(*t,out,depth+1,visits)?;}Self::name_append(out,">")?;}
            }else if let Some(origin)=self.brands.iter().find(|(b,_)| *b==t).map(|(_,o)| o.clone()){
                // A brand: its own name, unique, then what it was opened from.
                let Kind::Record(name)=&a.kind else{unreachable!()};
                Self::name_append(out,name)?;Self::name_append(out,"(")?;Self::name_append(out,&origin)?;Self::name_append(out,")")?;
            }else if matches!(a.kind,Kind::Record(_)){Self::name_append(out,".{")?;
                for(i,f)in a.fields.iter().enumerate(){if i!=0{Self::name_append(out,", ")?;}Self::name_append(out,&f.name)?;Self::name_append(out,": ")?;self.name_inner(f.ty,out,depth+1,visits)?;}Self::name_append(out,"}")?;
            }else if let Kind::Array(t,n)=a.kind{Self::name_append(out,"[")?;self.name_inner(t,out,depth+1,visits)?;Self::name_append(out,"; ")?;Self::name_append(out,&n.to_string())?;Self::name_append(out,"]")?;}},
            Ty::Sum(i) if self.sums[i as usize].anonymous=>{let sum=&self.sums[i as usize];Self::name_append(out,".[")?;
                for(i,(n,t))in sum.names.iter().zip(&sum.arms).enumerate(){if i!=0{Self::name_append(out," | ")?;}Self::name_append(out,n)?;
                    if *t!=Ty::Unit{Self::name_append(out,"(")?;self.name_inner(*t,out,depth+1,visits)?;Self::name_append(out,")")?;}}
                Self::name_append(out,"]")?;},
            Ty::Sum(i)=>{let sum=&self.sums[i as usize];let (name,args)=match &sum.origin{Some((id,args))=>(self.sum_definitions[*id].name.as_str(),args.clone()),None=>("Result",sum.arms.clone())};
                Self::name_append(out,name)?;
                if !args.is_empty(){Self::name_append(out,"<")?;for(i,t)in args.iter().enumerate(){if i!=0{Self::name_append(out,", ")?;}self.name_inner(*t,out,depth+1,visits)?;}Self::name_append(out,">")?;}},
            Ty::Borrow{mutable,pointee}=>{Self::name_append(out,if mutable{"&mut "}else{"&"})?;self.name_inner(pointee.ty(),out,depth+1,visits)?;},
        }Ok(())
    }
    pub fn concrete(&self,t:Ty)->bool { match t {
        Ty::Param{..}=>false,Ty::Weak(i) | Ty::Boxed(i) | Ty::Buffer(i)=>self.concrete(self.buffers[i as usize].element),
        Ty::Aggregate(i)=>self.aggregates[i as usize].concrete,
        Ty::Sum(i)=>self.sums[i as usize].concrete,
        Ty::Borrow{pointee,..}=>self.concrete(pointee.ty()),Ty::Slice(i)=>self.concrete(self.slice_element(i)),_=>true }
    }
    /// Read a nominal view's already-interned result in the receiver's generic
    /// environment. Resolving a proof clause does not create a new layout.
    pub fn view_type(&self, t: Ty, owner: FunctionId, args: &[Option<Ty>], depth: usize) -> Option<Ty> {
        if self.concrete(t) { return Some(t); }
        if depth > 16 { return None; }
        match t {
            Ty::Param { owner: who, index } if who == owner => *args.get(index as usize)?,
            Ty::Aggregate(i) => {
                let (id, params) = self.aggregates[i as usize].origin.as_ref()?;
                let params = params.iter().map(|p| self.view_type(*p, owner, args, depth + 1)).collect::<Option<Vec<_>>>()?;
                self.instances.iter().find(|(other, a, _)| other == id && *a == params).map(|(_, _, t)| *t)
            }
            _ => None,
        }
    }
    pub fn substitute(&mut self,t:Ty,owner:FunctionId,args:&[Ty])->Result<Ty> { self.sub_inner(t,owner,args,0,&mut 0) }
    fn sub_inner(&mut self,t:Ty,owner:FunctionId,args:&[Ty],depth:usize,visits:&mut usize)->Result<Ty> {
        if self.concrete(t){return Ok(t);} // cached identity: no parameter to substitute, no DAG expansion
        *visits+=1;if depth>16||*visits>4096{return Err(error("semantic substitution complexity limit"));}
        Ok(match t {
            Ty::Param{owner:o,index} if o==owner=>*args.get(index as usize).ok_or_else(||error("unbound function type parameter"))?,
            Ty::Weak(i)=>{let e=self.sub_inner(self.buffers[i as usize].element,owner,args,depth+1,visits)?;
                self.materialize(&TemplateType::Weak(Box::new(TemplateType::Base(e))),&[])?},
            Ty::Boxed(i)=>{let e=self.sub_inner(self.buffers[i as usize].element,owner,args,depth+1,visits)?;
                self.materialize(&TemplateType::Boxed(Box::new(TemplateType::Base(e))),&[])?},
            Ty::Buffer(i)=>{let e=Box::new(TemplateType::Base(self.sub_inner(self.buffers[i as usize].element,owner,args,depth+1,visits)?));
                self.materialize(&if self.buffers[i as usize].dense { TemplateType::Prefix(e) } else { TemplateType::Buffer(e) },&[])?},
            Ty::Slice(i) if self.is_str(i) => t,
            Ty::Slice(i)=>{let e=self.sub_inner(self.slice_element(i),owner,args,depth+1,visits)?; self.slice_of(e)?},
            Ty::Aggregate(i)=>{let a=&self.aggregates[i as usize];let origin=a.origin.clone();let kind=a.kind.clone();
                if let Some((id,params))=origin {let params=params.into_iter().map(|t|self.sub_inner(t,owner,args,depth+1,visits)).collect::<Result<Vec<_>>>()?;self.record(id,&params)?}
                else if let Kind::Array(e,n)=kind {let e=self.sub_inner(e,owner,args,depth+1,visits)?;self.array(e,n as u64)?}
                else{let fields=self.aggregates[i as usize].fields.clone();let fields=fields.into_iter().map(|f|Ok((f.name,self.sub_inner(f.ty,owner,args,depth+1,visits)?))).collect::<Result<Vec<_>>>()?;self.anonymous(fields)?}},
            Ty::Sum(i) if self.sums[i as usize].anonymous=>{let names=self.sums[i as usize].names.clone();let arms=self.sums[i as usize].arms.clone();
                let variants=names.into_iter().zip(arms).map(|(n,t)|Ok((n,self.sub_inner(t,owner,args,depth+1,visits)?))).collect::<Result<Vec<_>>>()?;self.union(variants)?},
            Ty::Sum(i)=>{let sum=&self.sums[i as usize];let origin=sum.origin.clone();let arms=sum.arms.clone();
                if let Some((id,params))=origin {let params=params.into_iter().map(|t|self.sub_inner(t,owner,args,depth+1,visits)).collect::<Result<Vec<_>>>()?;self.declared_sum(id,&params)?}
                else{let a=self.sub_inner(arms[0],owner,args,depth+1,visits)?;let b=self.sub_inner(arms[1],owner,args,depth+1,visits)?;self.result([a,b])?}},
            Ty::Borrow{mutable,pointee}=>{let t=self.sub_inner(pointee.ty(),owner,args,depth+1,visits)?;
                // Generic code may assign a whole `T` through `&mut T`; with `T` a view that would
                // replace its references, so a view is never `T` behind `&mut` (step 2).
                // Anywhere under it, too: `&mut Box<T>` lets generic code store a new `T` in the
                // box (adversarial review round 21).
                if mutable && !self.concrete(pointee.ty()) {
                    let mut used=vec![false;args.len()]; self.parameters_in(pointee.ty(),owner,&mut used);
                    if used.iter().zip(args).any(|(u,a)| *u && self.lends(*a)) { return Err(error("a value that holds a reference is not given to generic code behind `&mut`: the code could replace what it points at")); }
                }
                // A reference to `()` refers to nothing, and is nothing.
                if t==Ty::Unit{return Ok(Ty::Unit);}
                Ty::Borrow{mutable,pointee:super::logical_body::Pointee::from_ty(t).ok_or_else(||error("unsupported substituted pointee"))?}},
            _=>t })
    }
    pub fn partial(&mut self,t:Ty,owner:FunctionId,args:&[Option<Ty>])->Result<Option<Ty>> {
        let mut needed=vec![false;args.len()];self.parameters_in(t,owner,&mut needed);
        if needed.iter().enumerate().any(|(i,n)|*n&&args[i].is_none()){return Ok(None);}
        // Unused entries retain THEIR real symbolic identity, never a physical stand-in.
        let args=args.iter().enumerate().map(|(i,t)|t.unwrap_or(Ty::Param{owner,index:i as u32})).collect::<Vec<_>>();
        Ok(Some(self.substitute(t,owner,&args)?))
    }
    /// Whether a type written in `owner`'s signature names any of its `count` type parameters:
    /// one that names none has nothing for an argument to settle.
    pub fn names_parameter(&self,t:Ty,owner:FunctionId,count:usize)->bool {
        let mut needed=vec![false;count];self.parameters_in(t,owner,&mut needed);needed.contains(&true)
    }
    fn parameters_in(&self,t:Ty,owner:FunctionId,out:&mut[bool]) {
        let mut pending=vec![t];let mut seen=vec![];
        while let Some(t)=pending.pop(){if seen.contains(&t){continue;}seen.push(t);match t {
            Ty::Param{owner:o,index} if o==owner=>out[index as usize]=true,
            Ty::Weak(i) | Ty::Boxed(i) | Ty::Buffer(i)=>pending.push(self.buffers[i as usize].element),
            Ty::Slice(i)=>pending.push(self.slice_element(i)),
            Ty::Aggregate(i)=>{let a=&self.aggregates[i as usize];if let Some((_,args))=&a.origin{pending.extend(args)}else if let Kind::Array(t,_)=a.kind{pending.push(t)}else{pending.extend(a.fields.iter().map(|f|f.ty))}},
            Ty::Sum(i)=>{let s=&self.sums[i as usize];pending.extend(s.arms.iter().copied());if let Some((_,args))=&s.origin{pending.extend(args.iter().copied());}},Ty::Borrow{pointee,..}=>pending.push(pointee.ty()),_=>{} }
        }
    }
    /// Two types a type argument cannot both be. When one is a reference to the other, say how
    /// to read it (`for p in [...]` over records gives `&P`; a `Vec<P>` takes a `P`).
    fn conflict(&self,expected:Ty,actual:Ty)->Diagnostic {
        let borrowed=|r:Ty,v:Ty| matches!(r,Ty::Borrow{pointee,..} if pointee.ty()==v);
        if borrowed(actual,expected) { return Diagnostic::new("W0402",0..0,"generic type argument conflict: a value is expected and this is a reference to one; read it with `*`"); }
        if borrowed(expected,actual) { return Diagnostic::new("W0402",0..0,"generic type argument conflict: a reference is expected and this is a value; lend it with `&`"); }
        // `map.get("pen")`: the parameter is `&Q`, so any value given there is lent with `&`.
        if matches!(expected,Ty::Borrow{..}) && !matches!(actual,Ty::Borrow{..}) { return Diagnostic::new("W0402",0..0,"generic type argument conflict: a reference is expected and this is a value; lend it with `&`, as `map.get(&\"pen\")`"); }
        // Name both types (it said only the first sentence).
        if let (Ok(first),Ok(here))=(self.type_name(expected),self.type_name(actual)) {
            return Diagnostic::new("W0402",0..0,format!("generic type argument conflict: one type parameter is given a `{first}` and a `{here}`; one call gives each type parameter one type"));
        }
        Diagnostic::new("W0402",0..0,"generic type argument conflict")
    }
    pub fn unify(&self,pattern:Ty,actual:Ty,owner:FunctionId,args:&mut[Option<Ty>])->Result<()> {
        let mut pending=vec![(pattern,actual)];let mut seen=vec![];
        while let Some((p,a))=pending.pop(){
            if seen.contains(&(p,a)){continue;}seen.push((p,a));
            if seen.len()>4096{return Err(error("generic inference complexity limit"));}
            if let Ty::Param{owner:o,index}=p {if o==owner {let slot=&mut args[index as usize];
                if let Some(t)=*slot{if t!=a{
                    // Two openings' numberings: say where each came from (docs/design-brands.md).
                    if let (Some(first),Some(here))=(self.brand_origin(t),self.brand_origin(a)) {
                        let text=if here==first { format!("brand conflict: this comes from one opening of `{here}` and what it is used with here from another. Each `brand` opening numbers its own values, even of the same thing, so a value from one fits nothing from another") }
                            else { format!("brand conflict: this comes from `{here}`; what it is used with here was opened from `{first}`. Each `brand` opening numbers its own values, so a value from one fits nothing from another") };
                        return Err(Diagnostic::new("W0402",0..0,text));
                    }
                    if let Some(origin)=self.brand_origin(a).or_else(||self.brand_origin(t)) {
                        let text=format!("brand conflict: this comes from the opening of `{origin}` and is used with a value that does not: a value from a `brand` opening is used only inside that block, with values from the same opening (a helper takes them as `G: Brand`)");
                        return Err(Diagnostic::new("W0402",0..0,text));
                    }
                    return Err(self.conflict(t,a));}}else{*slot=Some(a);}continue;}}
            if p==a&&(self.concrete(p)||matches!(p,Ty::Param{..})){continue;}
            match(p,a){
                // A Buffer and a PrefixBuffer are two types, whatever they hold.
                (Ty::Buffer(a),Ty::Buffer(b)) if self.buffers[a as usize].dense!=self.buffers[b as usize].dense=>return Err(Diagnostic::new("W0402",0..0,"generic nominal shape conflict")),
                (Ty::Weak(a),Ty::Weak(b)) | (Ty::Boxed(a),Ty::Boxed(b)) | (Ty::Buffer(a),Ty::Buffer(b))=>pending.push((self.buffers[a as usize].element,self.buffers[b as usize].element)),
                (Ty::Slice(a),Ty::Slice(b)) if !self.is_str(a) && !self.is_str(b) => pending.push((self.slice_element(a),self.slice_element(b))),
                (Ty::Aggregate(a),Ty::Aggregate(b))=>{let a=&self.aggregates[a as usize];let b=&self.aggregates[b as usize];match(&a.origin,&b.origin){
                    (Some((ad,aa)),Some((bd,ba))) if ad==bd=>pending.extend(aa.iter().copied().zip(ba.iter().copied())),
                    (None,None)=>if let(Kind::Array(a,n),Kind::Array(b,m))=(&a.kind,&b.kind){if n==m{pending.push((*a,*b));}else{return Err(Diagnostic::new("W0402",0..0,"generic array shape conflict"));}}
                        else if matches!((&a.kind,&b.kind),(Kind::Record(_),Kind::Record(_)))&&a.fields.len()==b.fields.len()&&a.fields.iter().zip(&b.fields).all(|(x,y)|x.name==y.name){pending.extend(a.fields.iter().zip(&b.fields).map(|(x,y)|(x.ty,y.ty)));}
                        else{return Err(error("generic nominal shape conflict"));},
                    _=>return Err(Diagnostic::new("W0402",0..0,"generic nominal shape conflict")),}},
                (Ty::Sum(a),Ty::Sum(b))=>{let a=&self.sums[a as usize];let b=&self.sums[b as usize];match(&a.origin,&b.origin){
                    (Some((ad,aa)),Some((bd,ba))) if ad==bd=>pending.extend(aa.iter().copied().zip(ba.iter().copied())),
                    (None,None) if a.anonymous==b.anonymous && a.names==b.names=>pending.extend(a.arms.iter().copied().zip(b.arms.iter().copied())),
                    _=>return Err(Diagnostic::new("W0402",0..0,"generic nominal shape conflict")),}},
                (Ty::Borrow{mutable:a,pointee:p},Ty::Borrow{mutable:b,pointee:q}) if !a||b=>pending.push((p.ty(),q.ty())),
                _=>{
                    if let Some(origin)=self.brand_origin(a).or_else(||self.brand_origin(p)) {
                        let text=format!("brand conflict: this comes from the opening of `{origin}` and is used where a value that does not is expected: a value from a `brand` opening stays inside that block, with values from the same opening");
                        return Err(Diagnostic::new("W0402",0..0,text));
                    }
                    return Err(self.conflict(p,a));
                }
            }
        }Ok(())
    }
    pub fn enable_heap(&mut self) { self.heap=true; }
    pub fn uses_allocator(&self)->bool { self.allocator }
    pub fn uses_heap(&self)->bool { self.heap||self.allocator }
    pub fn uses_console(&self)->bool { self.console }
    pub fn uses_input(&self)->bool { self.input }
    pub fn templates_receipt(&self)->Option<String> { let entries=self.definitions.iter().filter(|d| !d.parameters.is_empty()).map(|d| {
        let params=d.parameters.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(",");
        format!("{{\"name\":\"{}\",\"parameters\":[{params}],\"validation\":\"{}\"}}",d.name,if d.owner {"structural-owning-template"} else {"structural-copyable-template"}) }).collect::<Vec<_>>();
        if entries.is_empty(){None}else{Some(format!("[{}]",entries.join(",")))} }
}
