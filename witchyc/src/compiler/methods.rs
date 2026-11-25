//! A method is a function named `Type.method`. An `impl` block only says which type.
//!
//! A trait method is a function named `Trait.method` over one type parameter, `Self`,
//! with no body: a call to it is replaced by the implementing type's own method once
//! `Self` is known. `impl Trait for Type` is an ordinary impl block whose methods must
//! match the trait's signatures.
use crate::{ast as a, diagnostic::Diagnostic};
use std::collections::BTreeMap;

fn refuse(span: &std::ops::Range<usize>, message: &'static str) -> Diagnostic { Diagnostic::new("W0401", span.clone(), message) }

/// The function a method becomes.
pub(super) fn name(owner: &str, method: &str) -> String { format!("{owner}.{method}") }

/// The trait a bound names: `Equivalent` for `Equivalent<K>`.
pub(super) fn bound_name(bound: &str) -> &str { bound.split_once('<').map_or(bound, |(name, _)| name) }

/// `Self` replaced by the implementing type, and a trait's own parameters by its arguments.
fn for_type(ty: &a::Type, names: &BTreeMap<String, a::Type>) -> a::Type {
    match ty {
        a::Type::Named(name, arguments) if arguments.is_empty() && names.contains_key(name) => names[name].clone(),
        a::Type::Named(name, arguments) => a::Type::Named(name.clone(), arguments.iter().map(|t| for_type(t, names)).collect()),
        a::Type::Borrow { mutable, inner } => a::Type::Borrow { mutable: *mutable, inner: Box::new(for_type(inner, names)) },
        a::Type::Array { element, length } => a::Type::Array { element: Box::new(for_type(element, names)), length: *length },
        a::Type::Slice(inner) => a::Type::Slice(Box::new(for_type(inner, names))),
        a::Type::Record(fields) => a::Type::Record(fields.iter().map(|(n, t)| (n.clone(), for_type(t, names))).collect()),
        a::Type::Union(variants) => a::Type::Union(variants.iter().map(|(n, t)| (n.clone(), t.as_ref().map(|t| for_type(t, names)))).collect()),
        a::Type::Unit => a::Type::Unit,
    }
}

/// A trait method's contract is every implementation's: `method` proves the trait's clauses (and
/// any `ensures` of its own), which generic callers rely on.
fn inherit(method: &mut a::Function, wanted: &[a::Function], at: &std::ops::Range<usize>) -> Result<(), Diagnostic> {
    let Some(signature) = wanted.iter().find(|m| m.name == method.name) else { return Ok(()) };
    // A pure trait method is read by contracts as a value of its receiver: each implementation
    // must be pure too, so two calls on an unchanged receiver agree.
    if signature.pure && !method.pure { return Err(refuse(at, "the trait's method is `pure`: so is every implementation of it (`pure fn`)")); }
    if signature.requires.is_empty() && signature.ensures.is_empty() { return Ok(()); }
    if signature.parameters.iter().zip(&method.parameters).any(|((wanted, _), (given, _))| wanted != given) {
        return Err(refuse(at, "an implementation of a trait method with a contract names its parameters as the trait does"));
    }
    method.requires = signature.requires.clone();
    let own = std::mem::take(&mut method.ensures);
    method.ensures = signature.ensures.iter().cloned().chain(own).collect();
    if !method.body.iter().any(|s| matches!(s, a::Statement::Proof(_))) {
        method.body.insert(0, a::Statement::Proof(vec![a::ProofStep::Auto]));
    }
    Ok(())
}

/// Replaces every `impl` block and trait by functions, in place.
pub(super) fn lift(mut program: a::Program) -> Result<a::Program, Diagnostic> {
    program.key_roots = super::key_law::check(&program)?;
    if !program.declarations.iter().any(|d| matches!(d, a::Declaration::Impl { .. } | a::Declaration::Trait { .. })) { return Ok(program); }
    let mut traits: BTreeMap<String, (Vec<String>, Vec<a::Function>)> = BTreeMap::new();
    for declaration in &program.declarations {
        let a::Declaration::Trait { name, generics, methods, span } = declaration else { continue; };
        if generics.iter().any(|g| g == "Self") { return Err(refuse(span, "`Self` is the implementing type, not a parameter of the trait")); }
        if matches!(name.as_str(), "Any" | "Self" | "Brand" | "Fresh") { return Err(refuse(span, "this trait name is reserved")); }
        let mut listed = Vec::new();
        for (method, at) in methods {
            if !method.generics.is_empty() { return Err(refuse(at, "a trait method has no type parameters of its own")); }
            if listed.iter().any(|m: &a::Function| m.name == method.name) { return Err(refuse(at, "a trait lists each method once")); }
            listed.push(method.clone());
        }
        if traits.insert(name.clone(), (generics.clone(), listed)).is_some() { return Err(refuse(span, "a trait is declared once")); }
    }
    // Each record's type parameters and fields, for view receivers (`self: Self { &mut count }`).
    let records: BTreeMap<String, (Vec<String>, Vec<(String, a::Type)>, bool)> = program.declarations.iter().filter_map(|d| match d {
        a::Declaration::Record { name, generics, fields, invariants, conditions, .. } => Some((name.clone(), (generics.clone(), fields.clone(), !invariants.is_empty() || !conditions.is_empty()))),
        _ => None,
    }).collect();
    let private_traits: std::collections::BTreeSet<String> = program.declarations.iter().filter_map(|d| match d {
        a::Declaration::Trait { name, .. } if program.private.contains_key(name) => Some(name.clone()),
        _ => None,
    }).collect();
    // The file an impl is written in: its methods', or for one with none, by where it starts.
    let starts = program.starts.clone();
    let file = |methods: &[(a::Function, std::ops::Range<usize>)], span: &std::ops::Range<usize>| methods.first().map_or_else(
        || starts.partition_point(|first| *first <= span.start).saturating_sub(1) as u32, |(method, _)| method.module);
    let mut out = Vec::new();
    // A type's methods share one namespace, its own and every trait's.
    let mut lifted = std::collections::BTreeSet::new();
    for declaration in program.declarations.drain(..) {
        match declaration {
            a::Declaration::Trait { name: trait_name, generics, methods, .. } => {
                // `Self` must implement the trait, with the trait's own parameters as its arguments.
                let bound = if generics.is_empty() { trait_name.clone() }
                    else { a::Type::Named(trait_name.clone(), generics.iter().map(|g| a::Type::Named(g.clone(), Vec::new())).collect()).render() };
                for (mut method, _) in methods {
                    method.name = name(&trait_name, &method.name);
                    method.generics = std::iter::once("Self".to_string()).chain(generics.iter().cloned()).collect();
                    // `Self` and the trait's parameters may be owners.
                    method.any = vec![true; method.generics.len()];
                    method.bounds = std::iter::once(vec![bound.clone()]).chain(generics.iter().map(|_| Vec::new())).collect();
                    out.push(a::Declaration::Function(method));
                }
            }
            a::Declaration::Impl { generics, any, bounds, implements, trait_arguments, target, methods, span } => {
                // `impl<K: Eq> Equivalent<K> for K`: a trait for every type that meets the bounds.
                // Its methods are one generic function per method, under a name no source spells.
                let blanket = implements.is_some() && generics.len() == 1 && matches!(&target, a::Type::Named(name, inner) if *name == generics[0] && inner.is_empty());
                if blanket {
                    let trait_name = implements.clone().expect("a blanket impl names its trait");
                    let (parameters, wanted) = traits.get(&trait_name).ok_or_else(|| refuse(&span, "`impl Trait for Type` names a declared trait"))?;
                    if parameters.len() != trait_arguments.len() { return Err(refuse(&span, "an impl gives the trait one type argument for each of its parameters")); }
                    let owner = format!("#{}", generics[0]);
                    if program.implements.iter().any(|i| i.trait_name == trait_name && i.target.starts_with('#')) { return Err(refuse(&span, "a trait has one impl for every type")); }
                    let names = std::iter::once(("Self".to_string(), target.clone())).chain(parameters.iter().cloned().zip(trait_arguments.iter().cloned())).collect::<BTreeMap<_, _>>();
                    for (method, at) in &methods {
                        let Some(signature) = wanted.iter().find(|m| m.name == method.name) else { return Err(refuse(at, "this method is not one of the trait's")); };
                        if !method.generics.is_empty() { return Err(refuse(at, "a trait method has no type parameters of its own")); }
                        if !method.requires.is_empty() { return Err(refuse(at, "an implementation of a trait method cannot add `requires`")); }
                        let same = signature.parameters.len() == method.parameters.len()
                            && signature.parameters.iter().zip(&method.parameters).all(|((_, wanted), (_, given))| for_type(wanted, &names) == *given)
                            && for_type(&signature.result, &names) == method.result;
                        if !same { return Err(refuse(at, "this method's parameters and result must match the trait's, with `Self` as the implementing type")); }
                    }
                    if wanted.iter().any(|m| !methods.iter().any(|(given, _)| given.name == m.name)) { return Err(refuse(&span, "an impl of a trait provides every method of the trait")); }
                    program.implements.push(a::Implements { trait_name: trait_name.clone(), arguments: trait_arguments.clone(), target: owner, bounds: bounds.clone(), module: file(&methods, &span), at: span.start, parameters: Vec::new() });
                    for (mut method, at) in methods {
                        inherit(&mut method, wanted, &at)?;
                        // A blanket impl of a private trait is as private (review round 7: `n.z()` of a
                        // private blanket `Z` from another file minted a sealed type).
                        if private_traits.contains(trait_name.as_str()) { method.public = false; }
                        method.name = name(&format!("#{trait_name}"), &method.name);
                        method.generics = generics.clone();
                        method.any = any.clone();
                        method.bounds = bounds.clone();
                        out.push(a::Declaration::Function(method));
                    }
                    continue;
                }
                // `impl<T> Vec<T>`: the type applied to exactly the block's own parameters.
                // `impl Show for &str`: a trait whose `Self` is taken by value, for borrowed UTF-8.
                // Its methods are `&str.name`; `impl Hash for str` (`self: &Self`) gives `str.name`.
                let no_arguments: Vec<a::Type> = Vec::new();
                let str_borrow = implements.is_some() && matches!(&target, a::Type::Borrow { mutable: false, inner } if matches!(&**inner, a::Type::Named(n, inner) if n == "str" && inner.is_empty()));
                let (owner, arguments) = match &target {
                    a::Type::Named(owner, arguments) => (owner.clone(), arguments),
                    _ if str_borrow => ("&str".to_string(), &no_arguments),
                    _ => return Err(refuse(&span, "an impl block names a declared type")),
                };
                let owner = &owner;
                // A directory, file or secret with one right is its own namespace: `Dir<Read>.open`.
                let rendered = target.render();
                let owner = if matches!(owner.as_str(), "Dir" | "File" | "Secret") { &rendered } else { owner };
                let own = arguments.len() == generics.len() && arguments.iter().zip(&generics).all(|(argument, parameter)|
                    matches!(argument, a::Type::Named(name, inner) if name == parameter && inner.is_empty()));
                // `impl Console<Write>`: methods of one instance, written with no parameters of
                // the block's own. They are ordinary functions on that instance's type, in the
                // type's one namespace of methods; a trait is implemented for every instance.
                let one = generics.is_empty() && implements.is_none() && !arguments.is_empty();
                if !own && !one { return Err(refuse(&span, "an impl block is for every instance of its type, `impl<T> Name<T>`, or for one, `impl Name<U64>`; a trait is implemented for every instance")); }
                if implements.is_none() && !trait_arguments.is_empty() { return Err(refuse(&span, "an impl block names a declared type")); }
                if let Some(trait_name) = &implements {
                    let (parameters, wanted) = traits.get(trait_name).ok_or_else(|| refuse(&span, "`impl Trait for Type` names a declared trait"))?;
                    if parameters.len() != trait_arguments.len() { return Err(refuse(&span, "an impl gives the trait one type argument for each of its parameters")); }
                    // The arguments are read where the program's types are, with no parameters of the block's own.
                    // `impl<T> Walk<T> for Vec<T>`: the arguments may name the block's own parameters; the
                    // methods are compared with the trait's as written, the parameters as names.
                    let names = std::iter::once(("Self".to_string(), target.clone())).chain(parameters.iter().cloned().zip(trait_arguments.iter().cloned())).collect::<BTreeMap<_, _>>();
                    if program.implements.iter().any(|i| i.trait_name == *trait_name && i.target == *owner) { return Err(refuse(&span, "a type implements a trait once")); }
                    for (method, at) in &methods {
                        let Some(signature) = wanted.iter().find(|m| m.name == method.name) else { return Err(refuse(at, "this method is not one of the trait's")); };
                        if !method.generics.is_empty() { return Err(refuse(at, "a trait method has no type parameters of its own")); }
                        // Callers see only the trait's signature, so they could not be asked to establish more.
                        if !method.requires.is_empty() { return Err(refuse(at, "an implementation of a trait method cannot add `requires`")); }
                        let same = signature.parameters.len() == method.parameters.len()
                            && signature.parameters.iter().zip(&method.parameters).all(|((_, wanted), (_, given))| for_type(wanted, &names) == *given)
                            && for_type(&signature.result, &names) == method.result;
                        if !same { return Err(refuse(at, "this method's parameters and result must match the trait's, with `Self` as the implementing type")); }
                    }
                    if wanted.iter().any(|m| !methods.iter().any(|(given, _)| given.name == m.name)) { return Err(refuse(&span, "an impl of a trait provides every method of the trait")); }
                    program.implements.push(a::Implements { trait_name: trait_name.clone(), arguments: trait_arguments.clone(), target: owner.clone(), bounds: bounds.clone(), module: file(&methods, &span), at: span.start, parameters: generics.clone() });
                }
                let record_name = match &target { a::Type::Named(n, _) => n.clone(), _ => String::new() };
                for (mut method, at) in methods {
                    if let Some(trait_name) = &implements && let Some((_, wanted)) = traits.get(trait_name) { inherit(&mut method, wanted, &at)?; }
                    if method.generics.iter().any(|g| generics.contains(g)) { return Err(refuse(&at, "a method type parameter repeats one of its impl block")); }
                    // A view receiver's fields take their types from the record, as this impl applies it.
                    if method.parameters.iter().any(|(n, _)| n.starts_with("self#")) {
                        if implements.is_some() { return Err(refuse(&at, "a trait method's receiver is the whole value: a view receiver is for a type's own methods")); }
                        let Some((parameters, fields, kept)) = records.get(&record_name) else { return Err(refuse(&at, "a view receiver is for the methods of a record type")) };
                        // A field written alone could break what the type keeps true of all its
                        // fields; such a type is changed only where a proof covers the whole
                        // (found by adversarial review round 13).
                        if *kept && method.parameters.iter().any(|(n, t)| n.starts_with("self#") && matches!(t, a::Type::Borrow { mutable: true, .. })) {
                            return Err(refuse(&at, "a type with an invariant is changed only in a function whose proof covers it: a view receiver may lend its fields shared (`&field`), not `&mut`"));
                        }
                        let a::Type::Named(_, applied) = &target else { unreachable!("checked above") };
                        let names: BTreeMap<String, a::Type> = parameters.iter().cloned().zip(applied.iter().cloned()).collect();
                        // A field that is itself a shared reference is passed as it is, and the body
                        // reads it without a dereference (adversarial review round 23).
                        let mut passed = Vec::new();
                        for (parameter, ty) in method.parameters.iter_mut() {
                            let a::Type::Borrow { inner, mutable } = ty else { continue };
                            let a::Type::Named(marker, field) = inner.as_ref() else { continue };
                            if marker != "#FieldOf" { continue; }
                            let [a::Type::Named(field, _)] = field.as_slice() else { continue };
                            let Some((_, field_ty)) = fields.iter().find(|(n, _)| n == field) else {
                                return Err(Diagnostic::new("W0401", at.clone(), format!("`{record_name}` has no field `{field}` for this view receiver to borrow")));
                            };
                            let field_ty = for_type(field_ty, &names);
                            if matches!(field_ty, a::Type::Borrow { .. }) {
                                if *mutable { return Err(Diagnostic::new("W0401", at.clone(), format!("`{field}` holds a reference: a view receiver lends it shared (`&{field}`); a reference field is never changed through `&mut`"))); }
                                *ty = field_ty;
                                passed.push(parameter.clone());
                            } else { **inner = field_ty; }
                        }
                        for statement in &mut method.body {
                            statement.visit_exprs_mut(&mut |e| {
                                if let a::Expression::Unary("*", inner) = &e.kind && let a::Expression::Name(n) = &inner.kind && passed.contains(n) {
                                    e.kind = a::Expression::Name(n.clone());
                                    return true;
                                }
                                false
                            });
                        }
                    }
                    // A private trait's methods are private to its file, as the trait is: the parser
                    // marks every trait impl's methods public, before it knows the trait (review round
                    // 6 of recipes: another file called `pt.Token.make()` of a private `Mk` and minted
                    // the sealed `Token`).
                    if let Some(trait_name) = &implements && private_traits.contains(trait_name.as_str()) { method.public = false; }
                    method.name = name(owner, &method.name);
                    if !lifted.insert(method.name.clone()) {
                        return Err(refuse(&at, "this type already has a method of this name: a type's own methods and those of the traits it implements share one namespace"));
                    }
                    method.generics = generics.iter().cloned().chain(method.generics).collect();
                    method.any = any.iter().copied().chain(method.any).collect();
                    method.bounds = bounds.iter().cloned().chain(method.bounds).collect();
                    out.push(a::Declaration::Function(method));
                }
            }
            other => out.push(other),
        }
    }
    program.declarations = out;
    Ok(program)
}

/// Whether a bound is a call-once trait (`WitchyOnce0<()>`, `WitchyOnce1<U64, ()>`): work run
/// once, which takes over what it holds (closure.rs writes these traits).
pub(super) fn once_bound(bound: &str) -> bool {
    bound.strip_prefix("WitchyOnce").and_then(|rest| rest.split('<').next()).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}
