//! `type Point derive(Eq, Hash, Before):` written out as the impls a program would write, then
//! parsed as source, so the checker sees ordinary methods and adds no rule for them.
use crate::ast::Type;

/// Number types compared with the operators; every other field through its own trait's method,
/// so a field whose type lacks the trait is refused as a call to a missing method would be.
fn number(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, arguments) if arguments.is_empty() && matches!(name.as_str(), "U8" | "U16" | "U32" | "U64" | "I64" | "F64"))
}

pub(super) fn source(name: &str, generics: &[String], fields: &[(String, Type)], traits: &[String]) -> String {
    let mut out = String::new();
    let head = applied(name, generics);
    for derived in traits {
        let bounds = bounded(generics, derived);
        match derived.as_str() {
            // Equal when every field is: `==` on a value that is not a number is `eq`.
            "Eq" => {
                let test = if fields.is_empty() { "true".to_string() } else {
                    fields.iter().map(|(f, _)| format!("(*self).{f} == (*other).{f}")).collect::<Vec<_>>().join(" && ")
                };
                out += &format!("impl{bounds} Eq for {head}:\n    fn eq(self: &{head}, other: &{head}) -> Bool:\n        return {test}\n\n");
            }
            // Each field's hash folded in by `hash_fold` (library/wasm-hash.wy). Generated tokens all
            // carry the type's span, so the source here writes no two-token operator such as `<<`.
            "Hash" => {
                out += &format!("impl{bounds} Hash for {head}:\n    fn hash(self: &{head}) -> U64:\n        var folded: U64 = 0\n");
                for (f, _) in fields {
                    out += &format!("        folded = folded.fold_hash((*self).{f}.hash())\n");
                }
                out += "        return folded\n\n";
            }
            // In field order: the first field that differs decides.
            "Before" => {
                out += &format!("impl{bounds} Before for {head}:\n    fn before(self: &{head}, other: &{head}) -> Bool:\n");
                for (f, ty) in fields {
                    let (less, more) = if number(ty) {
                        (format!("(*self).{f} < (*other).{f}"), format!("(*other).{f} < (*self).{f}"))
                    } else {
                        (format!("(*self).{f}.before(&(*other).{f})"), format!("(*other).{f}.before(&(*self).{f})"))
                    };
                    out += &format!("        if {less}:\n            return true\n        if {more}:\n            return false\n");
                }
                out += "        return false\n\n";
            }
            // Opened, and each field discarded through its own impl: owners closed, numbers dropped.
            "Discard" => {
                out += &format!("impl{bounds} Discard for {head}:\n    fn discard(self: {head}):\n");
                if fields.is_empty() { out += "        return\n\n"; continue; }
                let names = fields.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>().join(", ");
                out += &format!("        let {name}({names}) = move self\n");
                for (f, _) in fields { out += &format!("        {f}.discard()\n"); }
                out += "\n";
            }
            _ => unreachable!("checked by the parser"),
        }
    }
    out
}

/// The same traits for a sum type: a `match` on the variant, then each value it holds through its
/// own trait, as a record's fields are. Variants compare in the order written, so `Before` is that
/// order, then the values in order.
pub(super) fn sum_source(name: &str, generics: &[String], variants: &[(String, Option<Type>)], traits: &[String]) -> String {
    let head = applied(name, generics);
    // What each variant holds, in order: nothing, one value, or several (an anonymous record).
    let held: Vec<(String, Vec<Type>)> = variants.iter().map(|(v, ty)| (v.clone(), match ty {
        None => Vec::new(),
        Some(Type::Record(fields)) => fields.iter().map(|(_, t)| t.clone()).collect(),
        Some(t) => vec![t.clone()],
    })).collect();
    // `V(mine0, mine1):`, `V(mine0):` or `V:`.
    let arm = |v: &str, count: usize, binding: &str| if count == 0 { format!("{v}:") } else {
        format!("{v}({}):", (0..count).map(|k| format!("{binding}{k}")).collect::<Vec<_>>().join(", "))
    };
    let mut out = String::new();
    for derived in traits {
        let bounds = bounded(generics, derived);
        match derived.as_str() {
            "Eq" => {
                out += &format!("impl{bounds} Eq for {head}:\n    fn eq(self: &{head}, other: &{head}) -> Bool:\n        match self:\n");
                for (v, values) in &held {
                    out += &format!("            {}\n                match other:\n                    {}\n", arm(v, values.len(), "mine"), arm(v, values.len(), "theirs"));
                    let test = if values.is_empty() { "true".to_string() } else { (0..values.len()).map(|k| format!("*mine{k} == *theirs{k}")).collect::<Vec<_>>().join(" && ") };
                    out += &format!("                        return {test}\n                    _:\n                        return false\n");
                }
                out += "\n";
            }
            "Hash" => {
                out += &format!("impl{bounds} Hash for {head}:\n    fn hash(self: &{head}) -> U64:\n        match self:\n");
                for (index, (v, values)) in held.iter().enumerate() {
                    out += &format!("            {}\n                var folded: U64 = {index}\n                folded = folded.fold_hash(0)\n", arm(v, values.len(), "mine"));
                    for k in 0..values.len() { out += &format!("                folded = folded.fold_hash((*mine{k}).hash())\n"); }
                    out += "                return folded\n";
                }
                out += "\n";
            }
            "Before" => {
                out += &format!("impl{bounds} Before for {head}:\n    fn before(self: &{head}, other: &{head}) -> Bool:\n        match self:\n");
                for (i, (v, values)) in held.iter().enumerate() {
                    out += &format!("            {}\n                match other:\n", arm(v, values.len(), "mine"));
                    for (j, (w, others)) in held.iter().enumerate() {
                        out += &format!("                    {}\n", arm(w, others.len(), "theirs"));
                        if i != j { out += &format!("                        return {}\n", i < j); continue; }
                        for (k, ty) in values.iter().enumerate() {
                            let (less, more) = if number(ty) { (format!("*mine{k} < *theirs{k}"), format!("*theirs{k} < *mine{k}")) }
                                else { (format!("(*mine{k}).before(theirs{k})"), format!("(*theirs{k}).before(mine{k})")) };
                            out += &format!("                        if {less}:\n                            return true\n                        if {more}:\n                            return false\n");
                        }
                        out += "                        return false\n";
                    }
                }
                out += "\n";
            }
            "Discard" => {
                out += &format!("impl{bounds} Discard for {head}:\n    fn discard(self: {head}):\n        match move self:\n");
                for (v, values) in &held {
                    out += &format!("            {}\n", arm(v, values.len(), "mine"));
                    if values.is_empty() { out += "                return\n"; }
                    for k in 0..values.len() { out += &format!("                mine{k}.discard()\n"); }
                }
                out += "\n";
            }
            _ => unreachable!("checked by the parser"),
        }
    }
    out
}

/// `Pair<T, U>`, or the name alone.
fn applied(name: &str, generics: &[String]) -> String {
    if generics.is_empty() { name.to_owned() } else { format!("{name}<{}>", generics.join(", ")) }
}
/// `<T: Any + Eq, U: Any + Eq>`: each type parameter has the trait derived, as each field must,
/// and may be an owner (`Pair<String>`): the derived bodies reach a `T` only through a reference,
/// or move it out once (`Discard`), so nothing copies one.
fn bounded(generics: &[String], derived: &str) -> String {
    if generics.is_empty() { String::new() } else { format!("<{}>", generics.iter().map(|g| format!("{g}: Any + {derived}")).collect::<Vec<_>>().join(", ")) }
}
