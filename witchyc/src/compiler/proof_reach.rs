//! Which proof obligations an executable build sends: every one, except those of standard-library
//! functions the emitted module cannot run (docs/decisions.md, "Proving only the library a
//! program reaches", 2026-11-25).
//!
//! The module holds exactly the instances `instances::plan` registered from `main` (and each
//! `nopanic fn`) through every call of every expanded body, with each trait method resolved to
//! the implementation the type selects. Nothing else can run: the host and the runtime enter
//! only through what is emitted. A function's obligations show that its body keeps its contract
//! and that each call it makes meets the callee's `requires`; callers rely on a callee's contract
//! only at a call, which makes the callee reached too. So the obligations of a function with no
//! emitted instance bear on nothing that runs. A contract that reads a method (`xs.len()`, a
//! view, an `index`) reads its body, never that method's proof (scalar_vc `projection`).
//!
//! Kept whatever is reached (fail closed): the program's own files and packages (an author's
//! refused contract is refused whether or not `main` calls it yet), lemmas, any goal whose
//! function name is not exactly one known function's, a name shared with any kept function,
//! and every goal of a library build (`check_package`), an inspection, or a build whose files
//! are not known to be the standard library's. A kept function that is not emitted (the
//! program's own, never called) is still proved against its callees' contracts, so everything
//! it can call is kept too, by its call edges; a call to a trait's method keeps every function
//! of that method's name, since any of them could answer it.
use super::{logical_body::Function, proof_body::Prepared};
use std::collections::{BTreeMap, BTreeSet};

struct Entry { name: String, definition: u32, standard: bool, calls: Vec<u32>, required: bool }
/// Each function's name, definition, call edges and whether its file is the standard library's:
/// taken before instance planning consumes the checked program.
pub(super) struct Catalog { functions: Vec<Entry> }
impl Catalog {
    /// A file is the standard library's only when the parser put it in the standard package (it
    /// came in through `import NAME`) and its text in `source` is, line for line, a file the
    /// compiler ships (files.rs `standard_text`): a program's file, a test adapter's or a package's
    /// is never taken for one, whatever its imports say.
    pub(super) fn of(source: &str, syntax: &crate::ast::Program, functions: &[Function]) -> Self {
        let files = syntax.starts.len();
        let standard: Vec<bool> = (0..files).map(|file| {
            let (start, end) = (syntax.starts[file], syntax.starts.get(file + 1).copied().unwrap_or(source.len()));
            syntax.packages.len() == files && syntax.packages[file].1
                && source.get(start..end).is_some_and(super::files::standard_text)
        }).collect();
        Self { functions: functions.iter().map(|f| Entry {
            name: f.name.clone(), definition: f.definition.0, standard: standard.get(f.module as usize).copied().unwrap_or(false),
            calls: f.calls.iter().map(|edge| edge.definition.0).collect(), required: f.required,
        }).collect() }
    }
    /// The library functions not kept, given the definitions with an emitted instance.
    pub(super) fn unreached(&self, reached: &BTreeSet<u32>) -> Unreached {
        // A build with nothing emitted is not one this rule was made for.
        if reached.is_empty() { return Unreached::default(); }
        let at: BTreeMap<u32, usize> = self.functions.iter().enumerate().map(|(index, entry)| (entry.definition, index)).collect();
        let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (index, entry) in self.functions.iter().enumerate() { by_name.entry(entry.name.as_str()).or_default().push(index); }
        // Kept: what is emitted and every function not the library's; then whatever a kept one
        // calls, and every function sharing a name with a kept one, until nothing changes.
        let mut kept = vec![false; self.functions.len()];
        let mut pending: Vec<usize> = self.functions.iter().enumerate()
            .filter(|(_, entry)| !entry.standard || reached.contains(&entry.definition)).map(|(index, _)| index).collect();
        while let Some(index) = pending.pop() {
            if std::mem::replace(&mut kept[index], true) { continue; }
            let entry = &self.functions[index];
            pending.extend(by_name[entry.name.as_str()].iter().copied());
            for callee in &entry.calls {
                // An edge to a function not in the catalog: nothing can be said, so keep all.
                let Some(&callee) = at.get(callee) else { return Unreached::default(); };
                pending.push(callee);
                let target = &self.functions[callee];
                if target.required {
                    // `Trait.method`: any function named `….method` may answer it.
                    let Some((_, method)) = target.name.split_once('.') else { return Unreached::default(); };
                    let suffix = format!(".{method}");
                    pending.extend(self.functions.iter().enumerate().filter(|(_, f)| f.name.ends_with(&suffix)).map(|(index, _)| index));
                }
            }
        }
        let mut unreached = Unreached::default();
        for (name, entries) in by_name {
            if entries.iter().all(|index| !kept[*index]) {
                unreached.names.insert(name.to_string());
                unreached.definitions.extend(entries.iter().map(|index| self.functions[*index].definition));
            }
        }
        unreached
    }
}

/// Standard-library functions no emitted function can call: their obligations are not sent, and
/// the report names each one that had any.
#[derive(Default, Clone)]
pub(super) struct Unreached { names: BTreeSet<String>, definitions: BTreeSet<u32> }
/// The definition under `scalar_vc`'s `*_PROVED` flags, which take bits 29 and up.
const DEFINITION: u32 = (1 << 29) - 1;
impl Unreached {
    /// The obligations to send (those of every function not in this set, and every lemma), and
    /// the report's account of what was left out: each function whose obligations were not sent,
    /// after the obligations (empty when none was).
    pub(super) fn select(&self, mut prepared: Prepared) -> (Prepared, String) {
        if self.names.is_empty() { return (prepared, String::new()); }
        let mut left = BTreeSet::new();
        prepared.goals.retain(|goal| match &goal.source {
            Some(identity) if self.names.contains(&identity.function) => { left.insert(identity.function.clone()); false }
            _ => true,
        });
        // Nothing unproved is claimed in range, though no emitted function reads these.
        prepared.proved_arithmetic.retain(|flagged| !self.definitions.contains(&(flagged & DEFINITION)));
        if left.is_empty() { return (prepared, String::new()); }
        let names = left.iter().map(|name| format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))).collect::<Vec<_>>().join(",");
        (prepared, format!(",\"unproved_unreached\":{{\"rule\":\"standard-library functions with no emitted instance: never run by this module, their obligations not checked\",\"functions\":[{names}]}}"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    fn names(goals: &[super::super::proof::Goal]) -> BTreeSet<String> {
        goals.iter().filter_map(|goal| goal.source.as_ref().map(|identity| identity.function.clone())).collect()
    }
    /// `program` as `main.wy`, with the standard modules it imports, selected as an executable
    /// build selects: the functions whose obligations are sent, and those left out.
    fn selected(name: &str, program: &str) -> (BTreeSet<String>, BTreeSet<String>) {
        let root = std::env::temp_dir().join(format!("witchy-reach-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("main.wy"), program).unwrap();
        let bundle = super::super::files::read_bundle(&root.join("main.wy"));
        let _ = std::fs::remove_dir_all(&root);
        let (source, parts) = bundle.unwrap();
        let syntax = super::super::parse_modules(&source, &super::super::files::starts(&parts)).unwrap();
        let logical = super::super::check::program(&syntax).unwrap();
        let prepared = super::super::source_vc::prepare_with_sites(&logical).unwrap();
        let all = names(&prepared.goals);
        let catalog = super::Catalog::of(&source, &syntax, &logical.functions);
        let checked = super::super::concrete::program(logical).unwrap();
        let (kept, left_out) = catalog.unreached(&checked.definitions.iter().copied().collect()).select(prepared);
        let kept = names(&kept.goals);
        let left: BTreeSet<String> = all.difference(&kept).cloned().collect();
        // The report names exactly what was left out.
        assert_eq!(left.is_empty(), left_out.is_empty());
        for function in &left { assert!(left_out.contains(&format!("\"{function}\"")), "{function} unreported"); }
        (kept, left)
    }

    const GROW: &str = "    let grow = fn(a: &Allocator) -> U64:\n        let Ok(made): Vec<U64> = Vec.new(a, 4) else Err(e):\n            return 0\n        made.close()\n        return 1\n";
    #[test]
    fn a_library_function_reached_only_through_a_closure_is_proved() {
        let program = |call: &str| format!("import vec\n\nfn apply(f: &fn(&Allocator) -> U64, allocator: &Allocator) -> U64:\n    return f(allocator)\n\nfn main(console: Console<Write>, allocator: Allocator):\n{GROW}{call}");
        let (kept, left) = selected("closure", &program("    console.print_int(apply(&grow, &allocator))\n"));
        for function in ["Vec.new", "Vec.close"] { assert!(kept.contains(function) && !left.contains(function), "{function}"); }
        // Made and never called: the closure is the program's, so it is proved, against its
        // callees' contracts, which are proved too.
        let (kept, left) = selected("closure-uncalled", &program("    console.print_int(0)\n"));
        assert!(kept.contains("Vec.new") && !left.contains("Vec.new"));
        assert!(left.contains("Vec.push_mut"));
        // Neither, without it: what the library's Vec.new alone calls is the library's to leave.
        let (kept, left) = selected("closure-none", "import vec\n\nfn main(console: Console<Write>):\n    console.print_int(0)\n");
        assert!(left.contains("Vec.new") && !kept.contains("Vec.new"));
    }
    #[test]
    fn what_an_unreached_function_of_the_programs_calls_is_proved() {
        // `unused` is proved though nothing calls it; its proof assumes `Vec.new`'s and
        // `Vec.close`'s contracts, so theirs are proved as well.
        let (kept, left) = selected("own-calls", "import vec\n\nfn unused(allocator: &Allocator):\n    let Ok(made): Vec<U64> = Vec.new(allocator, 4) else Err(e):\n        return\n    made.close()\n\nfn main(console: Console<Write>):\n    console.print_int(1)\n");
        for function in ["Vec.new", "Vec.close", "prefix_release_empty"] { assert!(kept.contains(function) && !left.contains(function), "{function}"); }
    }
    #[test]
    fn only_the_shipped_text_is_the_standard_librarys() {
        // As `include` writes `wasm-string.wy`: its header imports rewritten, the rest as shipped.
        let shipped = super::super::files::standard_text;
        let string = include_str!("../../library/wasm-string.wy");
        let bundled = string.lines().map(|line| match line.strip_prefix("import ") {
            Some(_) => "import vec 3 1".to_string(), None => line.to_string() }).collect::<Vec<_>>().join("\n");
        assert!(shipped(&bundled));
        // One line changed, an import of a program's file (no kind), or a line added: not the library's.
        assert!(!shipped(&bundled.replacen("return", "return  ", 1)));
        assert!(!shipped(&bundled.replacen("import vec 3 1", "import vec 3", 1)));
        assert!(!shipped(&format!("{bundled}\nfn extra():\n    return\n")));
        assert!(!shipped("fn main():\n    return\n"));
    }
    #[test]
    fn a_library_method_reached_only_through_a_trait_bound_is_proved() {
        let program = |call: &str| format!("import string\nimport hash\n\nfn same<T: Any + Eq>(a: &T, b: &T) -> Bool:\n    return a.eq(b)\n\nfn main(console: Console<Write>, allocator: Allocator):\n    let Ok(made): String = String.new(&allocator) else Err(e):\n        return\n    let word = move made\n{call}    word.close()\n");
        let (kept, left) = selected("bound", &program("    if same(&word, &word):\n        console.print_int(1)\n"));
        // `String.eq` has no obligations of its own; what it calls does.
        assert!(kept.contains("String.word_at") && !left.contains("String.word_at"));
        // `same`, though uncalled, is the program's and is proved: its call through the bound
        // keeps every `eq`. Without it, `String.eq` and what it calls are left out.
        let (kept, left) = selected("bound-uncalled", &program(""));
        assert!(kept.contains("String.word_at"));
        let (kept, left_none) = selected("bound-none", &program("").replace("fn same<T: Any + Eq>(a: &T, b: &T) -> Bool:\n    return a.eq(b)\n\n", ""));
        assert!(!left.contains("String.word_at"));
        let left = left_none;
        assert!(left.contains("String.word_at") && !kept.contains("String.word_at"));
    }
    #[test]
    fn the_programs_own_functions_are_proved_whether_or_not_reached() {
        let (kept, left) = selected("own", "import vec\n\nfn bad(x: U64) -> U64:\n    ensures result == x + 1\n    return x\n\nfn main(console: Console<Write>):\n    console.print_int(1)\n");
        assert!(kept.contains("bad"));
        assert!(!left.is_empty() && !left.contains("bad") && !left.contains("main"));
    }
    #[test]
    fn a_name_left_out_is_only_ever_the_librarys_and_unreached() {
        let entry = |name: &str, definition: u32, standard: bool, calls: &[u32], required: bool| super::Entry {
            name: name.into(), definition, standard, calls: calls.to_vec(), required };
        let catalog = super::Catalog { functions: vec![
            entry("shared", 0, true, &[], false), entry("shared", 1, false, &[], false), // a program's function of a library function's name
            entry("twice", 2, true, &[], false), entry("twice", 3, true, &[], false),    // one of two reached
            entry("free", 4, true, &[], false), entry("own", 5, false, &[7, 8], false), entry("main", 6, false, &[3], false),
            entry("callee", 7, true, &[9], false),                                   // called by the program's unreached `own`
            entry("Show.show", 8, true, &[], true),                                  // a trait's method `own` calls
            entry("deep", 9, true, &[], false), entry("Text.show", 10, true, &[], false), entry("Text.other", 11, true, &[], false),
        ] };
        let unreached = catalog.unreached(&[3, 6].into_iter().collect());
        assert_eq!(unreached.names.iter().map(String::as_str).collect::<Vec<_>>(), ["Text.other", "free"]);
        assert_eq!(unreached.definitions.iter().copied().collect::<Vec<_>>(), [4, 11]);
        // An edge to no known function: nothing is left out.
        let unknown = super::Catalog { functions: vec![entry("main", 0, false, &[99], false), entry("free", 1, true, &[], false)] };
        assert!(unknown.unreached(&[0].into_iter().collect()).names.is_empty());
        // Nothing emitted: not a build this rule is for.
        assert!(catalog.unreached(&BTreeSet::new()).names.is_empty());
        // The flagged facts of a function left out go with it.
        let mut prepared = super::Prepared::default();
        prepared.proved_arithmetic.extend([4, 4 | (1 << 31), 4 | (1 << 29), 3 | (1 << 30), 5]);
        let (prepared, _) = unreached.select(prepared);
        assert_eq!(prepared.proved_arithmetic.into_iter().collect::<Vec<_>>(), [5, 3 | (1 << 30)]);
    }
}
