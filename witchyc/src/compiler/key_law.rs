//! The key law, checked from the shape of a type's `Eq`, `HashKey` and `Equivalent` bodies
//! (docs/plan-derive-laws.md, slice 3). A map hashing keys under its policy needs: keys equal by
//! `eq` feed the same input, `eq` is an equivalence, and a borrowed query equivalent to a key
//! feeds the key's input. Each follows from the template below for every type at once, so
//! nothing here runs per instantiation and no proof is scheduled:
//!
//! - `eq` tests fields E, each `(*self).f == (*other).f` (a conjunction returned, or early
//!   `if !(...): return false` steps then `return true`); `feed` feeds fields F, each
//!   `(*self).f.feed(state)`, or two U32 fields at once as one word,
//!   `state.word(U32.to_u64((*self).a) | (U32.to_u64((*self).b) << 32), 8)` (the bytes `a.feed`
//!   then `b.feed` give, in one call), each field once; F is a subset of E. Equal on E is equal on F; each leaf
//!   feeds equal input for equal values, so the concatenations are equal. Field-wise equality of
//!   equivalences is an equivalence.
//! - A query type (`HashKey` and `Equivalent<K>`, no `Eq`): `equivalent` is a conjunction of
//!   pairs, each `(*self).q == (*key).k` or `(*self).q.equivalent(&(*key).k)`, and its feed,
//!   mapped through the pairs, is exactly K's feed in order, call for call (a packed word
//!   against a packed word of the paired fields: the same calls on equal values, so this needs
//!   nothing of how a policy splits its input), each pair of one type or a pair the
//!   library makes feed the same bytes (Span, Text against String).
//! - A field's type is a leaf: a library trust root (below; their impls are the library's own
//!   and are not read here), a record of the program whose impls pass this check, or a type
//!   parameter (bound by `HashKey` and `Eq` where the impls are used, so its own impls are
//!   checked where they are written).
//!
//! The check reads the bodies the program has on every build, so no evidence can go stale.
use crate::{ast as a, diagnostic::Diagnostic};
use std::collections::{BTreeMap, BTreeSet};

/// Types whose `Eq` and `HashKey` are the library's, trusted: numbers fed as their bytes, and
/// byte strings fed as their length and bytes. A root is trusted by where it is declared, not
/// its name (review: a program's own `type Span` passed as the library's): a number has no
/// record declaration (a record of that name is checked as any record), `Text` cannot be
/// declared by a program (parser.rs), and `String` and `Span` are returned with the file of
/// their declaration for the build to refuse unless it is a standard one (wasm/files.rs).
const NUMBERS: &[&str] = &["U8", "U16", "U32", "U64", "Bool"];
const LIBRARY: &[&str] = &["String", "Span"];

/// (query field type, key field type) pairs that feed the same input for equivalent values.
const SAME_BYTES: &[(&str, &str)] = &[("Span", "String"), ("Text", "String"), ("str", "String")];

fn refuse(span: &std::ops::Range<usize>, message: String) -> Diagnostic {
    Diagnostic::new("W0401", span.clone(), message)
}

fn named(ty: &a::Type) -> Option<&str> {
    match ty { a::Type::Named(name, _) => Some(name.as_str()), _ => None }
}

/// `ty` with aliases followed (bounded: an alias chain longer than 64 is left as it is).
/// A generic alias's arguments put in place of its parameters (review round 5: `type W<T> =
/// U64` as a query key was read as `W`).
fn substitute(ty: &a::Type, names: &BTreeMap<&str, &a::Type>) -> a::Type {
    match ty {
        a::Type::Named(name, arguments) if arguments.is_empty() && names.contains_key(name.as_str()) => names[name.as_str()].clone(),
        a::Type::Named(name, arguments) => a::Type::Named(name.clone(), arguments.iter().map(|t| substitute(t, names)).collect()),
        other => other.clone(),
    }
}

/// `ty` with aliases followed, as seen from the file `module`: an alias private to another file
/// is not one this file can name (review round 8: another file's private `type U = U64` made a
/// record's type parameter `U` read as U64).
/// Whether `ty` names an alias anywhere in it (any file's). Aliases are refused where the key law
/// reads types (a key's field types, a query's key): four review rounds (4, 5, 8, 9) found a way
/// to make the law resolve one differently from the checker, so the law no longer resolves them.
fn names_alias(aliases: &BTreeMap<&str, (&[String], &a::Type)>, ty: &a::Type) -> bool {
    match ty {
        a::Type::Named(name, arguments) => aliases.contains_key(name.as_str()) || arguments.iter().any(|t| names_alias(aliases, t)),
        a::Type::Borrow { inner, .. } => names_alias(aliases, inner),
        a::Type::Array { element, .. } | a::Type::Slice(element) => names_alias(aliases, element),
        a::Type::Record(fields) => fields.iter().any(|(_, t)| names_alias(aliases, t)),
        a::Type::Union(tags) => tags.iter().any(|(_, t)| t.as_ref().is_some_and(|t| names_alias(aliases, t))),
        a::Type::Unit => false,
    }
}

/// `parameters` are the type parameters in scope (the record's): never expanded as aliases, at
/// any step (review round 9: `b: Id<T>` with `type Id<X> = X` became `T`, then an unrelated
/// `type T = U64` made the parameter read as U64).
fn unalias(aliases: &BTreeMap<&str, (&[String], &a::Type)>, private: &BTreeMap<String, u32>, module: u32, parameters: &[String], ty: &a::Type) -> a::Type {
    let mut ty = ty.clone();
    for _ in 0..64 {
        let a::Type::Named(name, arguments) = &ty else { break };
        if parameters.iter().any(|p| p == name) { break; }
        if private.get(name).is_some_and(|home| *home != module) { break; }
        let Some((parameters, aliased)) = aliases.get(name.as_str()) else { break };
        if parameters.len() != arguments.len() { break; }
        let names: BTreeMap<&str, &a::Type> = parameters.iter().map(String::as_str).zip(arguments.iter()).collect();
        ty = substitute(aliased, &names);
    }
    ty
}

/// A name with its `@file` (private_types.rs) taken off.
fn base(name: &str) -> &str { name.split('@').next().unwrap_or(name) }

/// `(*NAME).field` -> field.
fn field_of<'e>(expr: &'e a::Expr, receiver: &str) -> Option<&'e str> {
    let a::Expression::Field(base, field) = &expr.kind else { return None };
    let a::Expression::Unary("*", inner) = &base.kind else { return None };
    matches!(&inner.kind, a::Expression::Name(n) if n == receiver).then_some(field.as_str())
}

/// `(*self).f == (*other).f` -> f.
fn equal_field(expr: &a::Expr, other: &str) -> Option<String> {
    let a::Expression::Binary("==", left, right) = &expr.kind else { return None };
    let (l, r) = (field_of(left, "self")?, field_of(right, other)?);
    (l == r).then(|| l.to_string())
}

/// A conjunction of `test`s, in order; `true` alone is none.
fn conjunction<T>(expr: &a::Expr, test: &dyn Fn(&a::Expr) -> Option<T>, out: &mut Vec<T>) -> bool {
    match &expr.kind {
        a::Expression::Bool(true) => true,
        a::Expression::Binary("&&", left, right) => conjunction(left, test, out) && conjunction(right, test, out),
        _ => match test(expr) { Some(t) => { out.push(t); true } None => false },
    }
}

/// The fields `eq` tests, or None when the body is not the template.
fn eq_fields(method: &a::Function) -> Option<Vec<String>> {
    let other = second(method)?;
    let test = |e: &a::Expr| equal_field(e, &other);
    let mut fields = Vec::new();
    let (last, steps) = method.body.split_last()?;
    for step in steps {
        let a::Statement::If { condition, yes, no } = step else { return None };
        if !no.is_empty() || !matches!(yes.as_slice(), [a::Statement::Return(Some(a::Expr { kind: a::Expression::Bool(false), .. }))]) { return None; }
        let tested = match &condition.kind {
            a::Expression::Unary("!", inner) => test(inner)?,
            a::Expression::Binary("!=", left, right) => { let (l, r) = (field_of(left, "self")?, field_of(right, &other)?); if l != r { return None; } l.to_string() }
            _ => return None,
        };
        fields.push(tested);
    }
    let a::Statement::Return(Some(value)) = last else { return None };
    conjunction(value, &test, &mut fields).then_some(fields)
}

/// One call of a `feed` of the checked shape: a field's own `feed`, or two fields (U32, checked
/// by the caller) packed low first into one 8-byte word.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Fed { One(String), Packed(String, String) }

impl Fed {
    fn fields(&self) -> Vec<&String> { match self { Fed::One(f) => vec![f], Fed::Packed(a, b) => vec![a, b] } }
}

/// `U32.to_u64((*self).f)` -> f. The conversion is the compiler's own (refused where `U32` is
/// a variable's name), not a method a program can supply.
fn widened<'e>(expr: &'e a::Expr) -> Option<&'e str> {
    let a::Expression::Call(callee, arguments) = &expr.kind else { return None };
    let a::Expression::Field(scalar, method) = &callee.kind else { return None };
    if method != "to_u64" || !matches!(&scalar.kind, a::Expression::Name(n) if n == "U32") { return None; }
    let [field] = arguments.as_slice() else { return None };
    field_of(field, "self")
}

/// The calls `feed` makes, in order, or None when the body is not the template.
fn feed_fields(method: &a::Function) -> Option<Vec<Fed>> {
    let state = second(method)?;
    let mut fed = Vec::new();
    for step in &method.body {
        let a::Statement::Expr(call) = step else { return None };
        let a::Expression::Call(callee, arguments) = &call.kind else { return None };
        let a::Expression::Field(receiver, method_name) = &callee.kind else { return None };
        if method_name == "word" {
            // `state.word(U32.to_u64((*self).a) | (U32.to_u64((*self).b) << 32), 8)`.
            if !matches!(&receiver.kind, a::Expression::Name(n) if *n == state) { return None; }
            let [word, a::Expr { kind: a::Expression::Integer(8), .. }] = arguments.as_slice() else { return None };
            let a::Expression::Binary("|", low, high) = &word.kind else { return None };
            let a::Expression::Binary("<<", high, by) = &high.kind else { return None };
            if !matches!(by.kind, a::Expression::Integer(32)) { return None; }
            fed.push(Fed::Packed(widened(low)?.to_string(), widened(high)?.to_string()));
            continue;
        }
        if method_name != "feed" || !matches!(arguments.as_slice(), [a::Expr { kind: a::Expression::Name(n), .. }] if *n == state) { return None; }
        fed.push(Fed::One(field_of(receiver, "self")?.to_string()));
    }
    Some(fed)
}

/// The (query field, key field) pairs `equivalent` tests, or None when not the template.
fn equivalent_pairs(method: &a::Function) -> Option<Vec<(String, String)>> {
    let key = second(method)?;
    let test = |e: &a::Expr| -> Option<(String, String)> {
        match &e.kind {
            a::Expression::Binary("==", left, right) => Some((field_of(left, "self")?.to_string(), field_of(right, &key)?.to_string())),
            a::Expression::Call(callee, arguments) => {
                let a::Expression::Field(query, name) = &callee.kind else { return None };
                let [argument] = arguments.as_slice() else { return None };
                let a::Expression::Unary("&", borrowed) = &argument.kind else { return None };
                (name == "equivalent").then_some(())?;
                Some((field_of(query, "self")?.to_string(), field_of(borrowed, &key)?.to_string()))
            }
            _ => None,
        }
    };
    // A conjunction returned, or early `if !(pair): return false` steps then `return true` (the
    // form a recipe's `static for` writes), as for `eq`.
    let (last, steps) = method.body.split_last()?;
    let mut pairs = Vec::new();
    for step in steps {
        let a::Statement::If { condition, yes, no } = step else { return None };
        if !no.is_empty() || !matches!(yes.as_slice(), [a::Statement::Return(Some(a::Expr { kind: a::Expression::Bool(false), .. }))]) { return None; }
        let a::Expression::Unary("!", inner) = &condition.kind else { return None };
        pairs.push(test(inner)?);
    }
    let a::Statement::Return(Some(value)) = last else { return None };
    conjunction(value, &test, &mut pairs).then_some(pairs)
}

struct Record<'p> { generics: &'p [String], fields: BTreeMap<&'p str, &'p a::Type>, module: u32 }

/// `eq(self, other)`, `feed(self, state)`, `equivalent(self, key)`: the second parameter's name,
/// when the first is `self` and the second is not (review: `eq(other, self)` was read with the
/// operands swapped, and `(*self).a == (*self).a` passed).
fn second(method: &a::Function) -> Option<String> {
    match method.parameters.as_slice() {
        [(first, _), (other, _)] if first == "self" && other != "self" => Some(other.clone()),
        _ => None,
    }
}

/// Checks every program `HashKey` impl and every `Equivalent` on a keyed type; returns the
/// library roots relied on, with the file declaring each.
pub(super) fn check(program: &a::Program) -> Result<Vec<(String, u32)>, Diagnostic> {
    let records: BTreeMap<&str, Record> = program.declarations.iter().filter_map(|d| match d {
        a::Declaration::Record { name, generics, fields, module, .. } => Some((name.as_str(), Record { generics, fields: fields.iter().map(|(f, t)| (f.as_str(), t)).collect(), module: *module })),
        _ => None,
    }).collect();
    // Aliases resolved first (review round 4: `type W = U64` as an `Equivalent` key was read as
    // `W`, neither a root nor a record, while the checker uses U64).
    let aliases: BTreeMap<&str, (&[String], &a::Type)> = program.declarations.iter().filter_map(|d| match d {
        a::Declaration::Alias { name, generics, ty } => Some((name.as_str(), (generics.as_slice(), ty))),
        _ => None,
    }).collect();
    let resolve = |ty: &a::Type, module: u32, parameters: &[String]| -> a::Type { unalias(&aliases, &program.private, module, parameters, ty) };
    // The traits the law reads are the library's: a program trait of one of their names would
    // rename the library's (`HashKey@file`) and hide its impls (review round 4).
    let mut trait_names: BTreeMap<&str, usize> = BTreeMap::new();
    for declaration in &program.declarations {
        if let a::Declaration::Trait { name, span, .. } = declaration && matches!(base(name), "Eq" | "HashKey" | "Equivalent") {
            *trait_names.entry(base(name)).or_default() += 1;
            if trait_names[base(name)] > 1 {
                return Err(refuse(span, format!("`{}` is a trait of the library's hash module, which the key law reads: give this trait another name", base(name))));
            }
        }
    }
    let roots: std::cell::RefCell<Vec<(String, u32)>> = Default::default();
    // Whether `name` is a trusted root, noting the file of a library one.
    let is_root = |name: &str| -> bool {
        if NUMBERS.contains(&name) { return !records.contains_key(name); }
        if name == "Text" || name == "str" { return true; }
        if LIBRARY.contains(&name) && let Some(record) = records.get(name) {
            roots.borrow_mut().push((name.to_string(), record.module));
            return true;
        }
        false
    };
    // Each impl by (trait, target): its method of that trait and the block's span.
    let mut impls: BTreeMap<(&str, &str), (&a::Function, &std::ops::Range<usize>, &[a::Type])> = BTreeMap::new();
    // Every `Equivalent` impl by target (one type may be a query for several keys).
    let mut queries: BTreeMap<&str, Vec<(&a::Function, &std::ops::Range<usize>, &[a::Type])>> = BTreeMap::new();
    // Blanket `Equivalent` impls (`impl<K: Any + Eq> Equivalent<K> for K`, the library's): their
    // targets are parameters, and they call the key's own checked `eq`.
    let mut blanket_queries: BTreeSet<&str> = BTreeSet::new();
    // Each `HashKey` impl's bound for each of the target's type arguments, by position.
    let mut key_bounds: BTreeMap<&str, Vec<Option<&[String]>>> = BTreeMap::new();
    for declaration in &program.declarations {
        let a::Declaration::Impl { implements: Some(trait_name), target: target_type, methods, span, trait_arguments, generics, bounds, .. } = declaration else { continue };
        let Some(target) = named(target_type) else { continue };
        // A blanket impl (`impl<T: Marker> Eq for T`) is filed under its parameter, not the types
        // it covers, so their bodies would go unread (review round 3: an irreflexive blanket `Eq`
        // on a key): refused for the traits the law reads.
        let trait_name = base(trait_name);
        if matches!(trait_name, "Eq" | "HashKey") && generics.iter().any(|g| g == target) {
            return Err(refuse(span, format!("`impl {trait_name} for {target}` covers every type with its bounds, and the key law reads each key's own `{trait_name}`: implement it for each type")));
        }
        if base(trait_name) == "HashKey" && let a::Type::Named(_, arguments) = target_type {
            key_bounds.insert(target, arguments.iter().map(|argument| named(argument)
                .and_then(|p| generics.iter().position(|g| g == p)).and_then(|i| bounds.get(i)).map(Vec::as_slice)).collect());
        }
        // `KeyFor<K>` makes the key a map stores (`HashMap.get_or_insert`): for a library key type
        // (or any, through a type parameter) it must be the library's own, recorded for the build
        // to check like the key types' own impls (review of get_or_insert: a program's
        // `impl KeyFor<String> for Text` made "zzz" for every query, and the String keys it
        // stored sat where no lookup finds them).
        if trait_name == "KeyFor" && let Some((method, _)) = methods.first() {
            let key = trait_arguments.first().and_then(named);
            let covers_root = key.is_none_or(|k| NUMBERS.contains(&k) || LIBRARY.contains(&k) || k == "Text" || k == "str" || generics.iter().any(|g| g == k));
            if covers_root { roots.borrow_mut().push((format!("impl KeyFor for {target}"), method.module)); }
            continue;
        }
        let wanted = match trait_name { "Eq" => "eq", "HashKey" => "feed", "Equivalent" => "equivalent", _ => continue };
        // An impl for a type the law trusts (a number, Bool, Text, String, Span) must be the
        // library's own: its file is recorded for the build to check, as the types' are (review
        // round 7: a program that did not load wasm-string.wy wrote `Eq` and `HashKey` for Text,
        // every Text equal, and they were trusted).
        if (NUMBERS.contains(&target) || LIBRARY.contains(&target) || target == "Text" || target == "str")
            && let Some((method, _)) = methods.first() {
            roots.borrow_mut().push((format!("impl {trait_name} for {target}"), method.module));
        }
        if let Some((method, _)) = methods.iter().find(|(m, _)| m.name == wanted) {
            if wanted == "equivalent" {
                // A blanket `Equivalent` (the library's `impl<K: Any + Eq> Equivalent<K> for K`) is
                // kept apart, not filed under its parameter's name (review round 6: a program
                // record named `K` was checked against the library's blanket body).
                if generics.iter().any(|g| g == target) { blanket_queries.insert(target); continue; }
                queries.entry(target).or_default().push((method, span, trait_arguments));
            }
            else { impls.insert((trait_name, target), (method, span, trait_arguments)); }
        }
    }
    let keyed: BTreeSet<&str> = impls.keys().filter(|(t, _)| *t == "HashKey").map(|(_, target)| *target).collect();
    // `==` and `.feed`/`.equivalent` calls resolve to a method of that name, trait or not: an
    // inherent one would run in place of what the law reads (review round 11). With keys in the
    // program, no type has an inherent method of those names (the library has none).
    if !keyed.is_empty() {
        // Nor does any trait but the library's three declare a method of those names: `==` on a
        // type parameter resolves to any bound trait's `eq` (review round 12: a program trait
        // `Mine` with `eq`, implemented for a query type, ran in place of `Eq.eq`). So the names
        // reach only the impls the law reads.
        for declaration in &program.declarations {
            if let a::Declaration::Trait { name, methods, .. } = declaration && !matches!(base(name), "Eq" | "HashKey" | "Equivalent")
                && let Some((method, at)) = methods.iter().find(|(m, _)| matches!(m.name.as_str(), "eq" | "feed" | "equivalent")) {
                return Err(refuse(at, format!("the trait `{}` declares `{}`, the name of a key trait's method that `==` and the map call: with keys in the program, name it otherwise", base(name), method.name)));
            }
        }
        for declaration in &program.declarations {
            let a::Declaration::Impl { implements: None, target, methods, .. } = declaration else { continue };
            if let Some((method, at)) = methods.iter().find(|(m, _)| matches!(m.name.as_str(), "eq" | "feed" | "equivalent")) {
                let owner = named(target).unwrap_or("this type");
                return Err(refuse(at, format!("`{owner}` has its own method `{}`, the name of a key trait's method that `==` and the map call: with keys in the program, name it otherwise", method.name)));
            }
        }
    }
    // A query for a checked key must itself be checked below: a record with `HashKey` of the
    // checked shape (review round 2: `impl Equivalent<Key> for U64`, a root, went unread).
    for (&target, list) in &queries {
        for &(method, span, arguments) in list {
            if arguments.first().is_some_and(|k| names_alias(&aliases, k)) {
                return Err(refuse(span, format!("`{target}`'s `Equivalent` names its key with an alias; name the key type itself, so the key law reads the type the program uses")));
            }
            let resolved = arguments.first().map(|k| resolve(k, method.module, &[]));
            let Some(key) = resolved.as_ref().and_then(named) else { continue };
            // A query for a root key (a number, Text, String, Span): only the library's own pairs
            // that feed the same bytes, Span and Text for String, whose files the build checks
            // (review round 3: `impl Equivalent<U64> for U8` went unread; U8 feeds 1 byte, U64 8).
            let root_name = |n: &str| NUMBERS.contains(&n) || LIBRARY.contains(&n) || n == "Text" || n == "str";
            if root_name(key) && !(records.contains_key(key) && !LIBRARY.contains(&key) && !NUMBERS.contains(&key)) {
                if SAME_BYTES.contains(&(target, key)) && is_root(target) && is_root(key) { continue; }
                if target == key { continue; }
                return Err(refuse(span, format!("`{target}` is written as a query for `{key}`, a library key type: only the library's own queries (Span, Text and str for String) are known to feed their key's input")));
            }
            // Fail closed: a key that is neither a root (above) nor a record of the program is not
            // one the law can speak for (review round 5: an unexpanded alias was skipped).
            if !records.contains_key(key) {
                return Err(refuse(span, format!("`{target}` is written as a query for `{key}`, which is not a record of the program or a library key type, so the key law cannot check it")));
            }
            let checked_key = keyed.contains(key) && records.contains_key(key) && !LIBRARY.contains(&key);
            let checked_query = keyed.contains(target) && records.contains_key(target) && !LIBRARY.contains(&target) && !NUMBERS.contains(&target) && target != "Text" && target != "str";
            if checked_key && !checked_query {
                return Err(refuse(span, format!("`{target}` is a query for `{key}`, a key with a checked law, so it must be a record of the program that implements `HashKey` of the checked shape")));
            }
        }
    }
    // Whether a key packs fields into `state.word`: then that call must reach the library's own
    // `HashState.word` (below).
    let mut packed = false;
    for &target in &keyed {
        if is_root(target) { continue; }
        let (feed, span, _) = impls[&("HashKey", target)];
        let Some(record) = records.get(target) else {
            return Err(refuse(span, format!("`{target}` implements `HashKey`, and the key law is checked for records so far; `{target}` is not one")));
        };
        let lawful_leaf = |ty: &a::Type, field: &str| -> Result<(), Diagnostic> {
            let parameter = named(ty).is_some_and(|name| record.generics.iter().any(|g| g == name));
            if !parameter && names_alias(&aliases, ty) {
                return Err(refuse(span, format!("`{target}`'s field `{field}` is written with an alias; a key's fields name their types themselves, so the key law reads the type the program uses")));
            }
            // The type and every type argument in it, each a lawful leaf (review round 10: a field
            // `w: Wrap<U64, Bad>` tested by `eq` but not fed passed as the keyed record `Wrap`,
            // while `Bad`'s unchecked `Eq` came with it, since only `Wrap`'s `Eq` bound applied).
            let mut pending = vec![ty.clone()];
            while let Some(ty) = pending.pop() {
                // The record's own type parameters first, before any alias (review round 8).
                let raw = named(&ty).filter(|name| record.generics.iter().any(|g| g == name)).map(|_| ty.clone());
                let ty = raw.unwrap_or_else(|| resolve(&ty, record.module, record.generics));
                match named(&ty) {
                    Some(name) if !record.generics.iter().any(|g| g == name) && is_root(name) => {}
                    // A type parameter: only when the `HashKey` impl binds its argument by `HashKey`, so
                    // whatever it is instantiated with has its own impls checked here (review round 2).
                    Some(name) if let Some(position) = record.generics.iter().position(|g| g == name) => {
                        let bound = key_bounds.get(target).and_then(|b| b.get(position).copied().flatten());
                        if !bound.is_some_and(|b| b.iter().any(|t| t == "HashKey")) {
                            return Err(refuse(span, format!("`{target}`'s field `{field}` has the type parameter `{name}`, which its `HashKey` impl does not bind by `HashKey`: add the bound, so the type it is given carries a checked key law")));
                        }
                    }
                    Some("F64") => return Err(refuse(span, format!("`{target}`'s field `{field}` is an F64, which is not a lawful key: NaN is not equal to itself"))),
                    // A keyed record with its own checked `Eq` (review round 11: a query type, keyed
                    // with no `Eq` impl, could declare an inherent `eq` that `==` then called).
                    Some(name) if keyed.contains(name) && records.contains_key(name) && impls.contains_key(&("Eq", name)) => {
                        if let a::Type::Named(_, arguments) = &ty { pending.extend(arguments.iter().cloned()); }
                    }
                    Some(name) => return Err(refuse(span, format!("`{target}`'s field `{field}` has the type `{name}`, which carries no checked key law: give it `Eq` and `HashKey` of the checked shape (a `keys.Key`/`keys.Fed` derive)"))),
                    None => return Err(refuse(span, format!("`{target}`'s field `{field}` is not a named type, which carries no checked key law"))),
                }
            }
            Ok(())
        };
        let Some(calls) = feed_fields(feed) else {
            return Err(refuse(span, format!("`{target}`'s `feed` is not of the checked shape: one `(*self).field.feed(state)` per field, or `state.word(U32.to_u64((*self).a) | (U32.to_u64((*self).b) << 32), 8)` for two U32 fields, nothing else")));
        };
        let fed: Vec<String> = calls.iter().flat_map(Fed::fields).cloned().collect();
        let mut seen = BTreeSet::new();
        for field in &fed {
            if !seen.insert(field) { return Err(refuse(span, format!("`{target}`'s `feed` feeds the field `{field}` twice"))); }
            let Some(ty) = record.fields.get(field.as_str()) else { return Err(refuse(span, format!("`{target}` has no field `{field}`"))); };
            lawful_leaf(ty, field)?;
        }
        // A packed word holds two U32 fields, declared so (not a type parameter, alias or a
        // program's own `U32`): `U32.to_u64` of anything else is refused by the checker, and
        // the word is the bytes the two fields' own `feed` gives.
        for call in &calls {
            let Fed::Packed(..) = call else { continue };
            packed = true;
            for field in call.fields() {
                let ty = record.fields[field.as_str()];
                let declared = named(ty).filter(|n| !record.generics.iter().any(|g| g == n)).map(|_| resolve(ty, record.module, record.generics));
                if !(declared.as_ref().and_then(named) == Some("U32") && is_root("U32")) {
                    return Err(refuse(span, format!("`{target}` packs the field `{field}` into a word, and only U32 fields are packed")));
                }
            }
        }
        if let Some((eq, eq_span, _)) = impls.get(&("Eq", target)) {
            let Some(tested) = eq_fields(eq) else {
                return Err(refuse(eq_span, format!("`{target}` implements `HashKey`, so its `eq` must be of the checked shape: `(*self).f == (*other).f` for each field it tests, joined by `&&` or as early `return false` steps")));
            };
            for field in &tested {
                let Some(ty) = record.fields.get(field.as_str()) else { return Err(refuse(eq_span, format!("`{target}` has no field `{field}`"))); };
                lawful_leaf(ty, field)?;
            }
            if let Some(extra) = fed.iter().find(|f| !tested.contains(f)) {
                return Err(refuse(span, format!("`{target}` feeds the field `{extra}` to the hash, and its `eq` does not test it: two equal keys could hash apart")));
            }
        } else if !queries.contains_key(target) {
            return Err(refuse(span, format!("`{target}` implements `HashKey` with neither `Eq` nor `Equivalent<K>`: nothing says which values feed equal input")));
        }
        // Every `Equivalent` it has, with or without `Eq` (review: a type with `Eq` had its
        // `equivalent` unchecked, and a query one off from its key missed it in the map).
        for &(equivalent, eq_span, arguments) in queries.get(target).map(Vec::as_slice).unwrap_or(&[]) {
            let resolved = arguments.first().map(|k| resolve(k, equivalent.module, record.generics));
            let Some(key) = resolved.as_ref().and_then(named) else { return Err(refuse(eq_span, format!("`{target}`'s `Equivalent` names no key type"))); };
            let Some(key_record) = records.get(key).filter(|_| keyed.contains(key)) else {
                return Err(refuse(eq_span, format!("`{target}` is a query for `{key}`, which is not a record with a checked key law")));
            };
            let Some(pairs) = equivalent_pairs(equivalent) else {
                return Err(refuse(eq_span, format!("`{target}`'s `equivalent` is not of the checked shape: a conjunction of `(*self).q == (*key).k` or `(*self).q.equivalent(&(*key).k)`")));
            };
            let key_fed = feed_fields(impls[&("HashKey", key)].0).unwrap_or_default();
            // Call for call: a field's feed against the paired field's, a packed word against the
            // word of the paired fields in the same order.
            let through = |q: &String| pairs.iter().find(|(p, _)| p == q).map(|(_, k)| k.clone());
            let mapped: Vec<Option<Fed>> = calls.iter().map(|call| match call {
                Fed::One(q) => Some(Fed::One(through(q)?)),
                Fed::Packed(a, b) => Some(Fed::Packed(through(a)?, through(b)?)),
            }).collect();
            if mapped.len() != key_fed.len() || mapped.iter().zip(&key_fed).any(|(m, k)| m.as_ref() != Some(k)) {
                return Err(refuse(span, format!("`{target}` feeds its fields so that they do not match `{key}`'s feed in order through `equivalent`: an equivalent query could hash apart from its key")));
            }
            for (q, k) in &pairs {
                let (q_type, k_type) = (record.fields.get(q.as_str()).map(|t| resolve(t, record.module, record.generics)), key_record.fields.get(k.as_str()).map(|t| resolve(t, key_record.module, key_record.generics)));
                // The key's type parameters as the query names them (`Query(for: W<U64>)`: the key
                // field `T` is U64 here; review round 16 found this lawful query refused).
                let key_arguments: &[a::Type] = match resolved.as_ref() { Some(a::Type::Named(_, arguments)) => arguments, _ => &[] };
                let instance: BTreeMap<&str, &a::Type> = key_record.generics.iter().map(String::as_str).zip(key_arguments.iter()).collect();
                let k_type = k_type.map(|t| substitute(&t, &instance));
                let (Some(qt), Some(kt)) = (q_type.as_ref().and_then(named), k_type.as_ref().and_then(named)) else {
                    return Err(refuse(eq_span, format!("`{target}`'s `equivalent` pairs `{q}` with `{k}`, which are not fields of named types")));
                };
                if qt != kt && !SAME_BYTES.contains(&(qt, kt)) {
                    return Err(refuse(eq_span, format!("`{target}`'s `equivalent` pairs `{q}` ({qt}) with `{k}` ({kt}), types that do not feed the same input for equivalent values")));
                }
                if qt == kt { lawful_leaf(record.fields[q.as_str()], q)?; }
            }
        }
    }
    // A packed word is fed by `state.word`, read here as the library's `HashState.word`: no trait
    // of the program declares a method of that name (a blanket impl would reach `HashState`),
    // and no type named `HashState` has an inherent `word` but the library's, whose file is
    // checked with the trait's below.
    if packed {
        for declaration in &program.declarations {
            match declaration {
                a::Declaration::Trait { name, methods, .. } => if let Some((_, at)) = methods.iter().find(|(m, _)| m.name == "word") {
                    return Err(refuse(at, format!("the trait `{}` declares `word`, the name of the hash state's method a packed key calls: with packed keys in the program, name it otherwise", base(name))));
                },
                a::Declaration::Impl { implements: None, target, methods, .. } if named(target).is_some_and(|n| base(n) == "HashState") => {
                    if let Some((method, at)) = methods.iter().find(|(m, _)| m.name == "word") {
                        if !program.declarations.iter().any(|d| matches!(d, a::Declaration::Trait { name, methods: m, .. } if base(name) == "HashKey" && m.first().is_some_and(|(k, _)| k.module == method.module))) {
                            return Err(refuse(at, "a `HashState` with a `word` method of its own: a packed key's word must reach the library's".to_string()));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // The trait itself, and with it the library's number and Bool impls the law trusts: its file is
    // checked like String's (review round 6: a program with only U64 keys brought an edited copy
    // of wasm-hash.wy, where `U64`'s `eq` compared the low byte, and nothing checked it).
    if !keyed.is_empty() {
        let declared = program.declarations.iter().find_map(|d| match d {
            a::Declaration::Trait { name, methods, .. } if base(name) == "HashKey" => methods.first().map(|(m, _)| m.module),
            _ => None,
        });
        match declared {
            Some(module) => roots.borrow_mut().push(("HashKey".to_string(), module)),
            None => return Err(refuse(&(0..0), "a type implements `HashKey`, and no `HashKey` trait is declared".to_string())),
        }
    }
    Ok(roots.into_inner())
}
