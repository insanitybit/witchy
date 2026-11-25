use super::diagnostics::distance;
use super::*;

impl Checker<'_> {
    /// A variant written where a value is built: `Io`, or `Right.Io` naming its type. The
    /// type, if named, and the variant.
    pub(super) fn constructor<'e>(&self, expr: &'e a::Expr) -> Option<(Option<&'e str>, &'e str)> {
        match &expr.kind {
            a::Expression::Name(name) if self.layouts.has_variant(name) && !self.scope.contains_key(name) => Some((None, name)),
            a::Expression::Field(receiver, name) => match &receiver.kind {
                a::Expression::Name(sum) if !self.scope.contains_key(sum) && self.layouts.variant_of(sum, name) => Some((Some(sum), name)),
                _ => None,
            },
            _ => None,
        }
    }
    /// The sum and tag a constructor builds.
    pub(super) fn built(&self, (sum, name): (Option<&str>, &str), expected: Option<Ty>) -> Result<(Ty, u8)> {
        let built = match sum { Some(sum) => self.layouts.variant_in(sum, name, expected), None => self.layouts.variant(name, expected) }?;
        // A sum declared without `pub` is built only in its own file, as a private record is
        // (review: `Hush` of another file's private `Quietude` could be built).
        if let Some(owner) = self.layouts.nominal(built.0) && self.private.get(owner).is_some_and(|file| !self.may_reach(*file, Some(owner))) {
            return Err(error("this type or trait is private to the module that declares it; that module marks what it offers with `pub`"));
        }
        // A variant written by its name names its type: the type must be in this file's reach.
        // (`.Name`, read from the type a value already has, names nothing.)
        if let Some(owner) = self.layouts.nominal(built.0) && !name.starts_with('.') && let Err(refusal) = self.named_in_reach(owner, &(0..0)) { return Err(error(refusal.message)); }
        Ok(built)
    }
    /// The function a call by this name means here: this file's own private one, or the one
    /// every file shares.
    pub(super) fn function_named(&self, name: &str) -> String { self.function_named_from(name, self.module) }
    /// The function `name` means when written in `module`'s file.
    pub(super) fn function_named_from(&self, name: &str, module: u32) -> String {
        if name.contains('@') {
            // `name@file` written by the compiler for a function `file` declares with `pub`
            // (an operator's `sort_after`): the one key it has is its bare name.
            if !self.signatures.contains_key(name) && let Some((bare, file)) = name.split_once('@') && let Ok(file) = file.parse::<u32>()
                && self.signatures.get(bare).is_some_and(|sig| sig.module == file && sig.public) { return bare.to_string(); }
            return name.to_string();
        }
        let own = format!("{name}@{module}");
        if self.signatures.contains_key(&own) { return own; }
        // A method, or a function the compiler wrote: found by its type, asked at the call.
        if name.contains(['.', '#']) || self.reach.language.contains(name) { return name.to_string(); }
        // The only function of this name in the program: this file's own, or one a file this
        // one imports declares (private, it is refused at the call as private). One in a file
        // this one does not import is not what the name means here: no such function.
        if let Some(sig) = self.signatures.get(name) {
            return if self.sees_from(module, sig.home) { name.to_string() } else { format!("{name}@") };
        }
        // Declared in several other files: unqualified, it means the public one of a file this
        // one imports, if only one is. Two need the file's name (`unknown_function`).
        let mut public = self.others(name).filter(|key| self.signatures[*key].public && self.sees_from(module, self.signatures[*key].home));
        match (public.next(), public.next()) { (Some(only), None) => only.clone(), _ => name.to_string() }
    }
    /// Whether code written in `module` sees the file `home`: its own file, or one it imports
    /// itself. Code a derive recipe wrote is its recipe's file's and is put in the deriving
    /// file, so it sees what either does.
    pub(super) fn sees_from(&self, module: u32, home: u32) -> bool {
        self.reach.sees(module, home) || (module == self.module && self.derived.as_ref().is_some_and(|d| self.reach.sees(d.module, home)))
    }
    /// Whether this file may call the method or use the impl declared in `home` for a value of
    /// the type (or through the trait) `owner`: by seeing the file, or because that file is
    /// where the type or trait is declared and is of this package or the standard library.
    pub(super) fn finds(&self, home: u32, owner: &str) -> bool {
        if self.sees_from(self.module, home) { return true; }
        let owner = owner.strip_prefix('#').unwrap_or(owner);
        // A closure is a value: calling it is using what one was handed, not naming a declaration.
        if owner.starts_with("Closure#") { return true; }
        let nears = |file: u32| self.reach.near(file, home);
        if !(nears(self.module) || self.derived.as_ref().is_some_and(|d| nears(d.module))) { return false; }
        match self.reach.homes.get(owner) {
            // Declared in the type's (or the trait's) own file.
            Some((declared, _)) if *declared == home => true,
            // A type no file declares (`U64`, `Result`, `Console`, the language's `Text`): its
            // methods and impls are where the near files write them.
            None => true,
            _ if self.reach.language.contains(owner) => true,
            // A method of an impl of a trait for the type, written in the trait's own file.
            Some(_) => self.reach.impls.iter().any(|((trait_name, target), (at, _))| *at == home && target == owner
                && self.reach.homes.get(trait_name).is_some_and(|(declared, _)| *declared == home)),
        }
    }
    /// A call of `name`, resolved to `sig`, that this file cannot make because it does not
    /// import the declaring file.
    pub(super) fn out_of_reach(&self, name: &str, sig: &Signature, span: &std::ops::Range<usize>) -> Option<Diagnostic> {
        if self.reach.language.contains(name) { return None; }
        // `#Trait.method`: a blanket impl's method (`impl<T> Trait for T`), found through the
        // trait whatever the value's type: in reach where the impl's file is, or where it is
        // written in the trait's own file of this package or the standard library (review of
        // the reach rule: another package's inner blanket method was called by any value).
        if let Some(rest) = name.strip_prefix('#') {
            let (owner, method) = rest.split_once('.')?;
            if self.reach.language.contains(owner) || self.finds(sig.home, owner) { return None; }
            let owner = owner.split('@').next().unwrap_or(owner);
            return Some(Diagnostic::new("W0401", span.clone(), format!("`{owner}.{method}` is declared in a file this one does not import (⟪at {}⟫): a file calls the methods its own imports offer; import that file here", sig.at)));
        }
        let message = match name.rsplit_once('.') {
            None => return None,
            Some((owner, _)) if self.finds(sig.home, owner) => return None,
            Some((owner, method)) => {
                let (owner, method) = (owner.split('@').next().unwrap_or(owner).trim_start_matches('#'), method);
                format!("`{owner}.{method}` is declared in a file this one does not import (⟪at {}⟫): a file calls the methods its own imports offer; import that file here{}", sig.at, standard_import(self.reach, sig.home, crate::compiler::files::module_declaring_method(owner, method)))
            }
        };
        Some(Diagnostic::new("W0401", span.clone(), message))
    }
    /// The functions of this name in files that each have one: `name@file`.
    pub(super) fn others<'b>(&'b self, name: &'b str) -> impl Iterator<Item = &'b String> + 'b {
        self.others.get(name).into_iter().flatten()
    }
    /// Up to three methods of `owner` that this module may call and that are near `written`: a
    /// small edit distance, or one family of common names (closing, size, adding, reading).
    pub(super) fn near_methods(&self, owner: &str, written: &str) -> Vec<String> {
        const FAMILIES: [&[&str]; 5] = [&["close", "release", "discard", "free", "drop", "try_release"], &["len", "count", "size", "length", "byte_len"],
            &["push", "append", "add", "insert", "put", "push_mut", "push_text", "push_string"], &["get", "at", "index", "find", "lookup"],
            // Writing a number: `write_int` for `write_u64` (from a program that guessed the name).
            &["write_int", "write_number", "write_num", "write_i64", "write_u64", "print_int", "print_number", "write_f64"]];
        // The family of the word written, or of a family word it is a misspelling of (`lenght`).
        let family = FAMILIES.iter().find(|f| f.iter().any(|w| *w == written || distance(w, written) <= 2));
        // An empty owner: the free functions, whose keys have no `.` (one from another file is
        // `name@file`).
        let prefix = if owner.is_empty() { String::new() } else { format!("{owner}.") };
        let mut found: Vec<(usize, String)> = self.signatures.iter().filter_map(|(key, sig)| {
            let name = key.strip_prefix(&prefix)?;
            let name = if owner.is_empty() { name.split('@').next().unwrap_or(name) } else { name };
            if (!owner.is_empty() && name.contains('@')) || name.contains('.') || name.starts_with('#') || name == written { return None; }
            if !(sig.public || sig.module == self.module) { return None; }
            let d = distance(name, written);
            // A family word, or a longer name that begins with what was written (`equals_text`).
            let related = family.is_some_and(|f| f.contains(&name)) || (written.len() >= 3 && name.starts_with(written))
                // `from_u64` for `from`: the written name extends one the type has.
                || (name.len() >= 3 && written.starts_with(name) && written[name.len()..].starts_with('_'));
            (related || d <= 2.max(written.len() / 4)).then(|| (if related { 0 } else { d }, name.to_string()))
        }).collect();
        found.sort();
        found.dedup_by(|a, b| a.1 == b.1);
        found.into_iter().take(3).map(|(_, n)| n).collect()
    }
    /// Why no function of this name can be called here.
    pub(super) fn unknown_function(&self, name: &str, span: &std::ops::Range<usize>) -> Diagnostic {
        // Declared, and offered, by a file this one does not import.
        let elsewhere = self.signatures.get(name).filter(|sig| !name.contains(['.', '#', '@']) && !self.sees_from(self.module, sig.home))
            .or_else(|| { let mut found = self.others(name).map(|key| &self.signatures[key]).filter(|sig| sig.public && !self.sees_from(self.module, sig.home)); let first = found.next(); if found.next().is_none() && !self.others(name).any(|key| self.signatures[key].public && self.sees_from(self.module, self.signatures[key].home)) { first } else { None } });
        if let Some(sig) = elsewhere {
            let bare = name.split('@').next().unwrap_or(name);
            if sig.public && self.reach.standard(sig.home) && let Some(module) = crate::compiler::files::module_declaring_function(bare) {
                return Diagnostic::new("W0401", span.clone(), format!("unsupported or unknown function: `{bare}` is in the standard library's `{module}`: add `import {module}`"));
            }
            let text = if sig.public { format!("`{name}` is declared in a file this one does not import (⟪at {}⟫): a file sees its own declarations and what the files it imports itself offer with `pub`; import that file here", sig.at) }
                else { format!("`{name}` is private to the module that declares it; that module marks what it offers with `pub`") };
            return Diagnostic::new("W0401", span.clone(), text);
        }
        if self.others(name).filter(|key| self.signatures[*key].public).count() > 1 {
            return Diagnostic::new("W0401", span.clone(), format!("`{name}` is offered by more than one imported module: name the one meant, as `module.{name}`"));
        }
        if self.others(name).next().is_some() {
            return Diagnostic::new("W0401", span.clone(), format!("`{name}` is private to the module that declares it; that module marks what it offers with `pub`"));
        }
        // A standard library function the program has not imported (`sort(&mut v)` without `import sort`).
        let bare = name.split('@').next().unwrap_or(name);
        if let Some(module) = crate::compiler::files::module_declaring_function(bare) {
            return Diagnostic::new("W0401", span.clone(), format!("unsupported or unknown function: `{bare}` is in the standard library's `{module}`: add `import {module}`"));
        }
        let near = self.near_methods("", name);
        if !near.is_empty() {
            let listed = near.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", ");
            return Diagnostic::new("W0401", span.clone(), format!("unsupported or unknown function: the nearest functions in reach are {listed}"));
        }
        let shown = name.split('@').next().unwrap_or(name);
        Diagnostic::new("W0401", span.clone(), format!("unsupported or unknown function: no function, type or variant named `{shown}` is declared or imported here"))
    }
    /// A type or trait spelled in this file and declared without `pub` in another is refused.
    /// Whether this function may reach what is private to, or sealed in, the file `home` (the
    /// type `name`, if one). A function reaches its own file's. A method a derive recipe wrote
    /// (definition-site resolution) is the recipe file's, so it reaches that file's, except the
    /// type the derive's `For` stands for (the deriving user chose it: through it a user minted
    /// the recipe module's sealed type); and of the deriving file only the type it is derived for
    /// and the `For` type, as the deriving file's own code could (review rounds 1 and 2 of
    /// definition-site resolution: authority by token position missed field writes, `${...}`
    /// and a stale position; it is decided by the type).
    pub(super) fn may_reach(&self, home: u32, name: Option<&str>) -> bool {
        // A recipe applied in its own file: all of it is that file's code.
        let Some(derived) = self.derived.as_ref().filter(|d| d.module != self.module) else { return home == self.module };
        if home == derived.module { return name == Some(derived.target.as_str()) || (name.is_some() && name == self.for_outer.as_deref()); }
        home == self.module && !name.is_some_and(|n| self.for_reach.contains(n))
    }
    pub(super) fn sealed_reach(&self, ty: Ty) -> bool {
        self.layouts.sealed_in(ty).is_none_or(|module| self.may_reach(module, self.layouts.nominal(ty)))
    }
    pub(super) fn visible(&self, name: &str, span: &std::ops::Range<usize>) -> Result<()> {
        match self.private.get(name) {
            Some(home) if !self.may_reach(*home, Some(name)) => Err(Diagnostic::new("W0401", span.clone(), "this type or trait is private to the module that declares it; that module marks what it offers with `pub`")),
            _ => self.named_in_reach(name, span),
        }
    }
    /// A type, trait or variant's type named bare: declared here, or offered by a file this one imports.
    fn named_in_reach(&self, name: &str, span: &std::ops::Range<usize>) -> Result<()> {
        match self.reach.homes.get(name) {
            Some(&(home, at)) if !self.sees_from(self.module, home) && !self.reach.language.contains(name) && !name.contains('#') => { let bare = name.split('@').next().unwrap_or(name); Err(Diagnostic::new("W0401", span.clone(), format!(
                "`{bare}` is declared in a file this one does not import (⟪at {at}⟫): a file names its own types and those the files it imports itself offer with `pub`; import that file here{}", standard_import(self.reach, home, crate::compiler::files::module_declaring_type(bare))))) }
            _ => Ok(()),
        }
    }
}
