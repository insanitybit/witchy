use super::*;

impl Checker<'_> {
    /// The name of this function's own type parameter bounded `Brand` or `Fresh` among a type's
    /// arguments (`H` in `Node<H>`), when there is one.
    pub(super) fn brand_parameter_in(&self, t: Ty) -> Option<String> {
        let mut pending = vec![t]; let mut seen = 0;
        while let Some(t) = pending.pop() {
            seen += 1; if seen > 64 { return None; }
            match t {
                Ty::Param { owner, index } if owner == self.definition && self.bounds.get(index as usize).is_some_and(|b| b.iter().any(|b| b == "Brand" || b == "Fresh")) => return self.generics.get(index as usize).cloned(),
                Ty::Borrow { pointee, .. } => pending.push(pointee.ty()),
                Ty::Aggregate(_) | Ty::Sum(_) => pending.extend(self.layouts.parameters_of(t)),
                _ => {}
            }
        }
        None
    }
    /// The brand of the opening at `at`: made once per opening, the same on every reading of it.
    /// Its origin, for messages, is the first name in the opened value (`roads` in
    /// `roads.nodes()`).
    pub(super) fn opening_brand(&mut self, at: &a::Expr, inner: &a::Expr) -> Ty {
        fn first_name(e: &a::Expr) -> Option<String> {
            match &e.kind {
                a::Expression::Name(n) => Some(n.clone()),
                a::Expression::Field(base, _) | a::Expression::Unary(_, base) => first_name(base),
                a::Expression::Call(callee, args) => first_name(callee).filter(|n| !n.contains('.') || args.is_empty()).or_else(|| args.iter().find_map(first_name)),
                _ => None,
            }
        }
        let origin = first_name(inner).unwrap_or_else(|| "this opening".to_string());
        self.layouts.brand_at((self.definition.0, at.span.start), origin)
    }
    /// Whether a reference that is still in use borrows this root.
    pub(super) fn kept(&self, root: u32) -> bool {
        self.holds.values().any(|held| held.iter().any(|loan| self.overlaps(*loan, root))) || self.carried.iter().any(|loan| self.overlaps(*loan, root))
    }
    /// Whether a type implements a trait: by a bound on one of this function's own type
    /// parameters, or by an `impl Trait for Type` whose own bounds its type arguments meet.
    /// The type arguments a bound's text gives its trait (`K` in `Equivalent<K>`), read among the
    /// type parameters `names` of `owner`, then with `substitutions` put for them (none: as read).
    pub(super) fn bound_arguments(&mut self, bound: &str, owner: FunctionId, names: &[String], substitutions: Option<&[Ty]>) -> Result<Vec<Ty>> {
        if !bound.contains('<') { return Ok(Vec::new()); }
        let crate::ast::Type::Named(_, arguments) = crate::parser::parse_type(bound)? else { return Err(error("a bound names a trait")); };
        let mut out = Vec::with_capacity(arguments.len());
        for argument in &arguments {
            let ty = self.layouts.ty_in(argument, owner, names)?;
            out.push(match substitutions { Some(given) => self.layouts.substitute(ty, owner, given)?, None => ty });
        }
        Ok(out)
    }
    /// Whether `ty` implements the trait `name` with these type arguments: as this function's own
    /// parameter, by a bound that gives exactly these arguments; otherwise by an impl that does.
    pub(super) fn satisfies(&mut self, ty: Ty, name: &str, arguments: &[Ty]) -> Result<()> {
        let refuse = |this: &Self| {
            // A type parameter has no concrete name: its own name, as written.
            let written = match ty { Ty::Param { owner, index } if owner == this.definition => this.generics.get(index as usize).cloned().unwrap_or_default(),
                // A type with a type parameter inside has no concrete name either: its own name.
                _ => this.layouts.type_name(ty).ok().or_else(|| this.layouts.nominal(ty).map(|n| format!("{n}<...>"))).unwrap_or_else(|| "this type".to_string()) };
            // A trait two files declare is `Name@file` inside: shown as written, with whose it is.
            let (name, whose) = match name.split_once('@') {
                Some((bare, file)) if file.parse::<u32>().is_ok_and(|file| file == this.module) => (bare, " (the one this file declares)"),
                Some((bare, _)) => (bare, " (another file's, not the one this file declares of the same name)"),
                None => (name, ""),
            };
            // A type parameter of this function by its written name (`Equivalent<T>`, not `Equivalent<>`).
            let shown = |t: Ty| match t { Ty::Param { owner, index } if owner == this.definition => this.generics.get(index as usize).cloned().unwrap_or_default(),
                _ => this.layouts.type_name(t).unwrap_or_default() };
            let wanted = if arguments.is_empty() { name.to_string() }
                else { format!("{name}<{}>", arguments.iter().map(|t| shown(*t)).collect::<Vec<_>>().join(", ")) };
            // The message lives as long as the compiler run that reports it.
            // A record of the program's own takes the traits `derive` writes by asking for them.
            let derivable = arguments.is_empty() && matches!(name, "Eq" | "Hash" | "Before" | "Discard")
                && this.layouts.nominal(ty).is_some_and(|n| this.layouts.has_record(n));
            // An anonymous record (`type Edge = .{to: U64, weight: U64}` included) has no impls:
            // declared as a record of its own it can derive the trait.
            let anonymous = arguments.is_empty() && matches!(name, "Eq" | "Hash" | "Before" | "Discard") && this.layouts.is_anonymous(ty);
            let hint = if derivable { format!("; a record takes it by deriving it: `type {} derive({name}):`", this.layouts.nominal(ty).unwrap_or_default()) }
                else if anonymous { format!("; an anonymous record implements no trait: declare it as a record, `type Name derive({name}):` with its fields one to a line") }
                else { String::new() };
            // The library module that gives a built-in type this trait, when the program has not imported it.
            let module = match (name, written.as_str()) {
                ("Show", "I64") | ("Hash" | "Eq" | "Discard", "I64") => Some("int"),
                ("Show", "U8" | "U16" | "U32" | "U64" | "Bool" | "Text") => Some("console"),
                ("Hash" | "Eq", "U8" | "U16" | "U32" | "U64" | "Bool") => Some("hash"),
                ("Hash" | "Eq" | "Discard", "String") | ("Hash" | "Eq", "Text") => Some("string"),
                ("Before", "U8" | "U16" | "U32" | "U64" | "I64" | "F64" | "Bool" | "String") => Some("sort"),
                ("Discard", "U8" | "U16" | "U32" | "U64" | "Bool" | "F64" | "Text") => Some("discard"),
                _ => None,
            };
            // A built-in type is not derived for: the library's module, when one gives the trait.
            let hint = match module { Some(module) => format!("; the library gives it in `import {module}`"), None => hint };
            // Its own impl of this trait has other arguments: a type takes one impl of a trait.
            let own = this.layouts.nominal(ty).and_then(|n| this.trait_arguments.get(&(name.to_string(), n.to_string())))
                .filter(|own| !arguments.is_empty() && own.as_slice() != arguments)
                .map(|own| format!("; it implements `{name}<{}>`, and a type takes one impl of a trait, so not this one as well", own.iter().map(|t| this.layouts.type_name(*t).unwrap_or_default()).collect::<Vec<_>>().join(", ")));
            let hint = own.unwrap_or(hint);
            error(format!("`{written}` does not implement `{wanted}`{whose}{hint}"))
        };
        // A brand is one an opening made, or this function's own parameter bounded `Brand` or
        // `Fresh`. Nothing else is one, and no `impl` makes it one. A `Fresh` parameter is given
        // only by an opening (infer_call), never by satisfying a bound.
        if name == "Brand" || name == "Fresh" {
            // (A brand meets `Fresh` too: infer_call alone decides which one a `Fresh` parameter gets.)
            if self.layouts.brand_origin(ty).is_some() { return Ok(()); }
            if let Ty::Param { owner, index } = ty && owner == self.definition
                && self.bounds[index as usize].iter().any(|b| b == "Brand" || (name == "Brand" && b == "Fresh")) { return Ok(()); }
            let shown = match ty { Ty::Param { owner, index } if owner == self.definition => self.generics.get(index as usize).cloned().unwrap_or_default(), _ => self.layouts.type_name(ty).unwrap_or_default() };
            let text = if matches!(ty, Ty::Param { .. }) { format!("`{shown}` is not a brand: a type parameter is one when it is declared `{shown}: Brand`") } else { format!("`{shown}` is not a brand: a brand is made by an opening, `brand value as name:`") };
            return Err(error(text));
        }
        if let Ty::Param { owner, index } = ty {
            if owner != self.definition { return Err(refuse(self)); }
            let (definition, generics) = (self.definition, self.generics.clone());
            for bound in self.bounds[index as usize].clone() {
                if crate::compiler::methods::bound_name(&bound) != name { continue; }
                if self.bound_arguments(&bound, definition, &generics, None)? == arguments { return Ok(()); }
            }
            return Err(refuse(self));
        }
        let Some(nominal) = self.layouts.nominal(ty).map(str::to_string) else { return Err(refuse(self)); };
        let key = (name.to_string(), nominal);
        let Some(needed) = self.implemented.get(&key).cloned() else {
            // An impl for every type that meets its bounds.
            for (trait_name, written, parameter, bounds) in self.blankets.clone() {
                if trait_name != name { continue; }
                let mut given = Vec::new();
                for t in &written { given.push(if matches!(t, a::Type::Named(n, inner) if *n == parameter && inner.is_empty()) { ty } else { self.layouts.ty(t)? }); }
                if given != arguments { continue; }
                self.impl_in_reach(name, "#", None)?;
                for bound in &bounds { self.satisfies(ty, bound, &[])?; }
                return Ok(());
            }
            return Err(refuse(self));
        };
        // A type's own impl with other arguments is refused, and the blanket impl is not tried in
        // its place: a method is found by type and name alone (`Text.equivalent`), so a call would
        // reach the type's own impl with arguments of the other types (2026-11-19: `Text` as a map
        // key read a Text key as a String; docs/decisions.md).
        // An impl with type parameters of its own has no arguments the checker reads yet (bodies.rs).
        // Never read as no arguments: a bound written without them is not met either.
        if !self.trait_arguments.contains_key(&key) {
            return Err(error(format!("a bound that gives `{name}` arguments is not met yet by an impl with type parameters of its own, as `{}`'s is: take the type itself as the parameter", key.1)));
        }
        if self.trait_arguments.get(&key).map_or(&[][..], Vec::as_slice) != arguments { return Err(refuse(self)); }
        self.impl_in_reach(name, &key.1, Some(&key.1))?;
        for (argument, bounds) in self.layouts.arguments(ty).into_iter().zip(needed) {
            for bound in bounds {
                // An impl's own bounds name traits without arguments: one with arguments has no parameters of its own.
                if bound.contains('<') { return Err(error("a bound with type arguments on an impl's parameter is not supported yet")); }
                self.satisfies(argument, &bound, &[])?;
            }
        }
        Ok(())
    }
    /// An impl is used where its file is seen, or where it sits with the type or the trait it
    /// is for (`finds`): another file's impl for a type and a trait that are both someone
    /// else's is that file's own business until it is imported.
    fn impl_in_reach(&self, trait_name: &str, target: &str, ty: Option<&str>) -> Result<()> {
        if self.reach.language.contains(trait_name) && ty.is_some_and(|ty| ty.starts_with("Closure#")) { return Ok(()); }
        let Some(&(home, at)) = self.reach.impls.get(&(trait_name.to_string(), target.to_string())) else { return Err(error("this impl has no file: it cannot be used here")) };
        if self.finds(home, trait_name) || ty.is_some_and(|ty| self.finds(home, ty)) { return Ok(()); }
        let shown = |name: &str| name.split('@').next().unwrap_or(name).to_string();
        let what = match ty { Some(ty) => format!("`{}` implements `{}`", shown(ty), shown(trait_name)), None => format!("the impl of `{}` for every type", shown(trait_name)) };
        Err(error(format!("{what} in a file this one does not import (⟪at {at}⟫): an impl is used where its file is imported, or where it is written beside the type or the trait; import that file here")))
    }
}
