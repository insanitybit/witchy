use super::invariants::opaque_view;
use super::*;

pub(super) fn declarations(program: &a::Program, at: &mut Option<std::ops::Range<usize>>) -> Result<CheckedDeclarations> {
    for declaration in &program.declarations {
        let name = match declaration {
            a::Declaration::Function(f) => Some(&f.name),
            a::Declaration::Record { name, .. } | a::Declaration::Alias { name, .. } | a::Declaration::Sum { name, .. } => Some(name),
            a::Declaration::Lemma(lemma) => Some(&lemma.name),
            _ => None,
        };
        if name.is_some_and(|name| crate::compiler::sum::reserved(name)) {
            return Err(error("Result, Ok, Err, discard, Buffer, PrefixBuffer, Boxed, Weak and Allocator are reserved selected declaration names"));
        }
    }
    let mut layouts = Types::new(program)?;
    // A brand that holds no authority would be a capability in name only.
    for name in &program.capabilities {
        let ty = layouts.named_record(name).ok_or_else(|| error("capability type missing"))?;
        if !layouts.authority_in(ty, 0) {
            return Err(Diagnostic::new("W0401", 0..0, format!("`{name}` is declared a capability, but what it is made from holds no authority")));
        }
    }
    // A grantable capability is policy data the run hands in: it may hold no authority
    // (granting it can never be a disguised `Net`), and in this first version its fields are
    // numbers and Bools, which the run can give by value.
    // A generator keeps its arguments between steps and can be handed on: it holds no
    // authority, not even a reference to some (docs/wasm-generators.md).
    for (name, at) in &program.generators {
        // A generic generator (`gen fn all<G: Brand>`) has no concrete record to look at: its
        // fields are read as written with each type parameter `U64`, which lays out wherever a
        // type parameter may stand. A type argument never holds authority (semantic_types,
        // `argument`), so what could is in the fields themselves. A field that still has no
        // layout is refused, not passed (review: `Wrap<()>` failed to lay out and was let by).
        let declared = program.declarations.iter().find_map(|d| match d { a::Declaration::Record { name: n, generics, fields, .. } if n == name && !generics.is_empty() => Some((generics.clone(), fields.clone())), _ => None });
        if let Some((generics, fields)) = declared {
            fn unit(t: &a::Type, generics: &[String]) -> a::Type {
                match t {
                    a::Type::Named(n, args) if args.is_empty() && generics.contains(n) => a::Type::Named("U64".into(), Vec::new()),
                    a::Type::Named(n, args) => a::Type::Named(n.clone(), args.iter().map(|t| unit(t, generics)).collect()),
                    a::Type::Borrow { mutable, inner } => a::Type::Borrow { mutable: *mutable, inner: Box::new(unit(inner, generics)) },
                    a::Type::Slice(inner) => a::Type::Slice(Box::new(unit(inner, generics))),
                    a::Type::Array { element, length } => a::Type::Array { element: Box::new(unit(element, generics)), length: *length },
                    a::Type::Record(fields) => a::Type::Record(fields.iter().map(|(n, t)| (n.clone(), unit(t, generics))).collect()),
                    a::Type::Union(tags) => a::Type::Union(tags.iter().map(|(n, t)| (n.clone(), t.as_ref().map(|t| unit(t, generics)))).collect()),
                    a::Type::Unit => a::Type::Unit,
                }
            }
            let shown = name.split('@').next().unwrap_or(name);
            // A `&fn(...)` parameter is a hidden type parameter that may carry authority
            // (review: a closure that captured the console was held).
            if generics.iter().any(|g| g.starts_with("#F")) {
                return Err(Diagnostic::new("W0401", at.clone(), format!("the generator type `{shown}` would hold a function value, which may hold authority: a `gen fn` takes no `&fn(...)` yet")));
            }
            for (_, t) in &fields {
                let concrete = layouts.ty(&unit(t, &generics));
                if concrete.as_ref().map_or(true, |ty| layouts.authority_in(*ty, 0)) {
                    let shown = name.split('@').next().unwrap_or(name);
                    return Err(Diagnostic::new("W0401", at.clone(), format!("the generator type `{shown}` would hold authority: a `gen fn` takes no capability, nor a reference to one")));
                }
            }
            continue;
        }
        let ty = layouts.named_record(name).ok_or_else(|| error("generator type missing"))?;
        if layouts.authority_in(ty, 0) {
            let shown = name.split('@').next().unwrap_or(name);
            return Err(Diagnostic::new("W0401", at.clone(), format!("the generator type `{shown}` would hold authority: a `gen fn` takes no capability, nor a reference to one")));
        }
    }
    for name in &program.grantable {
        let shown = name.split('@').next().unwrap_or(name);
        let refuse = |why: &str| Diagnostic::new("W0401", 0..0, format!("grantable capability `{shown}` {why}"));
        let ty = layouts.named_record(name).ok_or_else(|| error("capability type missing"))?;
        if layouts.authority_in(ty, 0) { return Err(refuse("holds host authority: a grantable capability carries policy data only, and one that wraps authority is built inside the program from an explicit capability")); }
        let fields = program.declarations.iter().find_map(|d| match d { a::Declaration::Record { name: n, fields, .. } if n == name => Some(fields), _ => None }).ok_or_else(|| error("capability type missing"))?;
        if fields.is_empty() || !fields.iter().all(|(_, t)| matches!(t, a::Type::Named(n, args) if args.is_empty() && matches!(n.as_str(), "U64" | "Bool"))) {
            return Err(refuse("has fields the run cannot grant: in this first version each is a U64 or a Bool"));
        }
        layouts.grantable.insert(name.clone());
    }
    let mut signatures = BTreeMap::new();
    let mut main = None;
    let mut secret_slots = Vec::new();
    // Scan every selected declaration before lowering any function. No proof,
    // contract, predicate or type declaration is silently eliminated.
    for declaration in &program.declarations {
        if matches!(declaration, a::Declaration::Record { .. } | a::Declaration::Alias { .. } | a::Declaration::Sum { .. } | a::Declaration::Lemma(_)) {
            continue;
        }
        let index = signatures.len();
        let a::Declaration::Function(f) = declaration else {
            return Err(error(
                "scalar Wasm rejects predicates and impls; their obligations are not implemented",
            ));
        };
        // A refusal of this signature with no place of its own is placed at the function.
        *at = (f.span != (0..0)).then(|| f.span.clone());
        let contracted=!f.requires.is_empty()||!f.ensures.is_empty();
        if contracted&&f.name=="main"{
            return Err(error("function contracts require nongeneric private pure U64 functions"));
        }
        if layouts.has_variant(&f.name) || layouts.has_sum(&f.name) {
            return Err(error("function conflicts with a sum type or variant"));
        }
        if layouts.has_record(&f.name) {
            return Err(error("function conflicts with record constructor"));
        }
        if layouts.has_alias(&f.name) {
            return Err(error("function conflicts with a type alias"));
        }
        if f.name=="main" && !f.generics.is_empty(){return Err(error("main cannot be generic"));}
        let mut type_names=BTreeSet::new();
        // A variant of another file's sum does not take a type parameter's name (review of a
        // program: `type Op: E` broke the library's `Result<T, E>`); one of this file's does.
        let own_variant=|name:&str| program.declarations.iter().any(|d| matches!(d, a::Declaration::Sum{variants,module,..} if *module==f.module && variants.iter().any(|(v,_)| v==name)));
        for name in &f.generics {
            let available=layouts.parameter_name_free_of_types(name)&&(layouts.parameter_name_available(name)||!own_variant(name));
            // A derive's method takes its type parameters from the deriving file (they are the
            // derive's words), so a private type of the recipe's file does not take the name.
            let home=program.derived.iter().find(|d| d.recipe.contains(&f.span.start) && d.recipe_module==f.module && f.name.split('.').next()==Some(d.target.as_str())).map_or(f.module,|d|d.module);
            if (!available&&!program.private.get(name).is_some_and(|file|*file!=home))||name==&f.name||!type_names.insert(name)||f.parameters.iter().any(|(n,_)|n==name)
                // A function of the parameter's own file: another file's `fn T()` does not take the name.
                ||program.declarations.iter().any(|d|matches!(d,a::Declaration::Function(other) if &other.name==name && other.module==f.module)) {
                return Err(error("duplicate or conflicting function type parameter"));
            }
        }
        let definition=FunctionId(index as u32);
        for (i,any) in f.any.iter().enumerate(){if *any{layouts.declare_any(definition,i as u32);}}
        let result = layouts.ty_in(&f.result,definition,&f.generics)?;
        let params = if f.name == "main" {
            if result != Ty::Unit {
                return Err(error("main must return unit"));
            }
            if f.parameters.len() > 12 { return Err(error("main accepts at most twelve distinct capabilities")); }
            let mut requested = Vec::new();
            for (name, source_ty) in &f.parameters {
                let ty = layouts.ty(source_ty)?;
                let root = matches!(ty, Ty::Host(host) if host.root());
                // A fixed secret is a slot of its own, named by its parameter: two `Secret<Seal>`
                // parameters are two keys, each bound by name (docs/wasm-capabilities.md).
                let slot = matches!(ty, Ty::Host(host) if host.slot_kind().is_some());
                let named_dir = program.test_adapter == Some(f.module) && matches!(ty, Ty::Host(crate::compiler::host::Host::Dir(_)));
                if !(matches!(ty, Ty::Console | Ty::Input | Ty::Allocator) || root) || (!slot && !named_dir && requested.contains(&ty)) {
                    return Err(error("main accepts only distinct Console<Write>, Console<Read>, Allocator, Dir, Dir<Read>, Dir<Write>, File<Read>, File<Write>, Clock, Rand, Env, Args, Net, Listener, Socket, Sqlite, Exec and SecretStore parameters, and any number of Secret<Seal>, Secret<Reveal>, Signer, TlsSigner and Mldsa87Signer ones"));
                }
                // `main(connection: Socket)` is the program for one connection: the engine accepts and
                // starts a new instance for each, and each instance holds only what it can be given
                // again whole. What is read once or held open across connections is not that
                // (docs/decisions.md, 2026-11-22, "An instance for each connection").
                let per_connection = |ty: &Ty| *ty == Ty::Host(crate::compiler::host::Host::Socket);
                // Granted again whole since round 3 of the native launch (docs/decisions.md,
                // 2026-11-23): a `File<Read>` and the input, read once before the first
                // connection, each instance reading its own copy from the start; `Net` and
                // `Exec`, policies whose objects the launch opened once. Still refused: what one
                // connection would change for the next (a written file), the engine's own
                // listener, and secrets. An engine that cannot grant one again refuses the launch.
                let once = |ty: &Ty| matches!(ty, Ty::Host(host) if matches!(host, crate::compiler::host::Host::Listener | crate::compiler::host::Host::SecretStore | crate::compiler::host::Host::File(crate::compiler::host::Right::Write | crate::compiler::host::Right::All)) || (*host != crate::compiler::host::Host::TlsSigner && host.slot_kind().is_some()));
                if (per_connection(&ty) && requested.iter().any(once)) || (once(&ty) && requested.iter().any(per_connection)) {
                    return Err(error("a main that takes a Socket is run once for each connection, in a new instance: it takes no Listener, File<Write>, File, SecretStore, Secret or Signer beside it (a written file is reached through a Dir)"));
                }
                if let Ty::Host(host) = ty && let Some(kind) = host.slot_kind() {
                    // The name reaches the module's slot section and the host's flags: a plain one.
                    if name.is_empty() || name.len() > 64 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                        return Err(error("a secret parameter of main has a plain name of at most 64 letters, digits and `_`"));
                    }
                    secret_slots.push((name.clone(), kind));
                }
                requested.push(ty);
            }
            if requested.contains(&Ty::Host(crate::compiler::host::Host::LocalService))
                && !requested.contains(&Ty::Host(crate::compiler::host::Host::Socket)) {
                return Err(error("a LocalService requires a Socket entry parameter"));
            }
            main = Some(FunctionId(index as u32));
            vec![]
        } else {
            f.parameters
                .iter()
                .map(|(_, t)| {
                    let t = layouts.ty_in(t, definition, &f.generics)?;
                    // A task is stepped through `&mut`: it holds no authority to swap or replace.
                    if let Ty::Borrow { mutable: true, pointee } = t
                        && matches!(pointee.ty(), Ty::Console | Ty::Input | Ty::Allocator | Ty::Host(_))
                        && pointee.ty() != Ty::Host(crate::compiler::host::Host::Task)
                    {
                        return Err(error("mutable Console/Allocator/Dir/File references are unsupported"));
                    }
                    if t == Ty::Unit {
                        Err(error("unit parameters are unsupported"))
                    } else {
                        Ok(t)
                    }
                })
                .collect::<Result<Vec<_>>>()?
        };
        for (index, default) in f.defaults.iter().enumerate() {
            if let Some(d) = default && matches!(d.kind, a::Expression::SourceSite(_)) && params.get(index) != Some(&Ty::U64) {
                return Err(Diagnostic::new("W0401", d.span.clone(), "a source-site parameter has type U64"));
            }
        }
        // `pure fn`: given no capability, so a call has no effect but its result (or a trap).
        if f.pure {
            if f.name == "main" { return Err(error("`main` is given the program's capabilities; it cannot be `pure`")); }
            // A lent callback (`&fn(...)`) may hold authority its maker gave it.
            if f.generics.iter().zip(&f.any).any(|(g, any)| g.starts_with("#F") && *any) {
                return Err(error("a `pure fn` takes no lent callback (`&fn(...)`): it may hold authority, and calling it would have an effect"));
            }
            // Nor work to run once (`W: WitchyOnce0<R>`), which may hold authority by value.
            if f.bounds.iter().flatten().any(|bound| crate::compiler::methods::once_bound(bound)) {
                return Err(error("a `pure fn` takes no work to run once (`W: WitchyOnce0<R>`): it may hold authority, and running it would have an effect"));
            }
            for ((name, _), ty) in f.parameters.iter().zip(&params) {
                let mut held = std::collections::BTreeSet::new();
                crate::compiler::caps::held(&layouts, *ty, &mut held, &mut Default::default(), 0);
                if let Some(capability) = held.first() {
                    return Err(Diagnostic::new("W0401", 0..0, format!("`{}` is a `pure fn`, but its parameter `{name}` holds {capability}: a pure function is given no capability", f.name)));
                }
            }
        }
        // A `&` result that could point into what a `&mut` parameter borrows would be a shared
        // reference holding a unique loan. A `&mut` parameter the result cannot point into, such
        // as a cursor, is free again when the call returns.
        let open = |t: Ty| open_parameter(t, definition, &f.generics, &f.bounds);
        let borrows = variant_sources(f, &params, result, &layouts)?;
        // A clause that names the `&mut` parameter says so outright: a caller keeps it lent, uniquely,
        // while the variant's `&` is in use.
        let named_outright = |index: usize| borrows.iter().any(|from| from.as_ref().is_some_and(|from| from.contains(&index)));
        if layouts.lends(result) && !layouts.lends_unique(result) && params.iter().enumerate().any(|(index, t)| matches!(t, Ty::Borrow { mutable: true, .. }) && layouts.may_lend_to(*t, result, &open) && named_by_borrows(&borrows, index) && !named_outright(index)) {
            return Err(error("a function that returns a `&` reference borrows only with `&`, not `&mut`, what the reference could point into"));
        }
        let storage=crate::compiler::storage_contract::eligible(definition,f.generics.len(),&params,result,&layouts);
        let record=contracted&&crate::compiler::record_contract::eligible(definition,f.generics.len(),&params,result,&layouts);
        let memory=contracted&&{let plain=layouts.plain(result)?;crate::compiler::memory_contract::eligible(definition,f.generics.len(),&params,plain,&layouts)};
        // No proof model reads a record that holds a reference. A proof over numbers may be given
        // one whole, to hand on to calls (`opaque_view`, `lending::only_handed_on`); it is never the result.
        if contracted && (layouts.views(result) || params.iter().enumerate().any(|(index, t)| layouts.views(*t)
            && !(!storage && !record && !memory && params.len() == f.parameters.len() && opaque_view(&layouts, *t)
                && !crate::compiler::lending::clauses_name(f, &f.parameters[index].0) && crate::compiler::lending::only_handed_on(f, &f.parameters[index].0)))) {
            return Err(error("a function with a contract cannot use a record that holds a reference yet"));
        }
        // Only the memory proof knows what a returned `&mut` reference lets its holder change.
        if contracted && !memory && layouts.lends_unique(result) {
            return Err(error("a function with a contract returns a `&mut` reference only when its proof is about a Buffer"));
        }
        // Any other contract, generic or not, is a claim about numbers (`scalar_vc`).
        let _=(storage,record,memory);
        // A method call is resolved by the receiver's type, after labels and defaults are put in
        // place, so a method's default could never be used.
        if f.name.contains('.') && f.defaults.iter().any(Option::is_some) {
            return Err(error("a method's parameters have no defaults yet: only a function called by its name fills them"));
        }
        if signatures
            .insert(
                f.name.clone(),
                Signature {
                    definition,
                    generics: f.generics.clone(),
                    any: f.any.clone(),
                    bounds: f.bounds.clone(),
                    contracted,
                    requires: !f.requires.is_empty(),
                    params,
                    result,
                    module: f.module,
                    public: f.public,
                    home: program.derived.iter().find(|d| d.recipe.contains(&f.span.start) && d.recipe_module == f.module && f.name.split('.').next() == Some(d.target.as_str())).map_or(f.module, |d| d.module),
                    at: f.span.start,
                    names: f.parameters.iter().map(|(n, _)| n.clone()).collect(),
                    defaults: f.defaults.clone(),
                    view: f.parameters.iter().map_while(|(n, t)| n.strip_prefix("self#").map(|field| (field.to_string(), matches!(t, a::Type::Borrow { mutable: true, .. })))).collect(),
                    borrows,
                    pure: f.pure,
                    shape: super::Shape::of(f),
                },
            )
            .is_some()
        {
            return Err(error("duplicate function"));
        }
    }
    *at = None;
    layouts.validate_reference_storage()?;
    Ok(CheckedDeclarations { layouts, signatures, main, secret_slots })
}
