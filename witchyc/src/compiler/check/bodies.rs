use super::invariants::{element_clauses, guarded_names, invariant_clauses, invariant_escape};
use super::diagnostics::missing_main;
use super::*;

pub(super) fn checked_program(program: &a::Program, at: &mut Option<std::ops::Range<usize>>) -> Result<Program> {
    let mut expanded = prepare(program)?;
    loop {
        let mut required = BTreeSet::new();
        let result = check_bodies(&expanded, at, &mut required);
        if required.is_empty() { return result; }
        // A body can discover a proof requirement through its resolved operand types.
        // Rebuild signatures too, so every caller consumes the same checked contract.
        for declaration in &mut expanded.to_mut().declarations {
            if let a::Declaration::Function(f)=declaration && required.contains(&f.name) {
                f.ensures.push(a::Postcondition::truth(f.span.clone()));
            }
        }
    }
}
fn check_bodies(program:&a::Program,at:&mut Option<std::ops::Range<usize>>,required:&mut BTreeSet<String>)->Result<Program> {
    let CheckedDeclarations { mut layouts, signatures, main, secret_slots } = declarations(program, at)?;
    let main = match main { Some(main) => main, None => return Err(error(missing_main(program))) };
    let blankets: Blankets = program.implements.iter().filter_map(|i| i.target.strip_prefix('#').map(|parameter|
        (i.trait_name.clone(), i.arguments.clone(), parameter.to_string(), i.bounds.first().cloned().unwrap_or_default()))).collect();
    let reach = Reach::of(program);
    // A type or trait named where only one can be: declared in the naming file, or offered by a
    // file it imports itself. Code a derive recipe wrote carries its recipe's spans: it names
    // what the recipe's file or a deriving file sees (`Checker::sees_from`).
    for (name, span) in &program.type_uses {
        let Some(&(home, at)) = reach.homes.get(name) else { continue };
        let here = program.starts.partition_point(|first| *first <= span.start).saturating_sub(1) as u32;
        if reach.language.contains(name) || name.contains('#') || reach.sees(here, home) { continue; }
        if program.derived.iter().any(|d| d.recipe_module == here && d.recipe.contains(&span.start) && reach.sees(d.module, home)) { continue; }
        let bare = name.split('@').next().unwrap_or(name);
        return Err(Diagnostic::new("W0401", span.clone(), format!(
            "`{bare}` is declared in a file this one does not import (⟪at {at}⟫): a file names its own types and those the files it imports itself offer with `pub`; import that file here{}", standard_import(&reach, home, crate::compiler::files::module_declaring_type(bare)))));
    }
    let direct = || program.implements.iter().filter(|i| !i.target.starts_with('#'));
    // One impl per type and trait: a direct impl the blanket impl also covers is refused.
    for i in direct() {
        for (trait_name, arguments, parameter, bounds) in &blankets {
            if *trait_name != i.trait_name { continue; }
            // An impl with parameters of its own names them in its arguments, so whether the
            // blanket impl covers some instance of it is not compared: refused whenever one exists.
            if !i.parameters.is_empty() && !i.arguments.is_empty() {
                return Err(error("an impl with type parameters of its own is not supported for a trait that has an impl for every type"));
            }
            let named = a::Type::Named(i.target.clone(), Vec::new());
            let covered = arguments.iter().map(|t| if matches!(t, a::Type::Named(n, inner) if n == parameter && inner.is_empty()) { named.clone() } else { t.clone() }).collect::<Vec<_>>() == i.arguments
                && bounds.iter().all(|bound| program.implements.iter().any(|j| j.trait_name == *bound && j.target == i.target));
            if covered { return Err(error("this type already has the trait from an impl for every type that meets its bounds")); }
        }
    }
    // A trait call is sent to `Type.method` when that exists, so a type with a method of a blanket
    // trait's name must have it from implementing that trait directly; otherwise refuse.
    for (trait_name, _, _, _) in &blankets {
        let names = program.declarations.iter().filter_map(|d| match d { a::Declaration::Function(f) if f.required => f.name.strip_prefix(&format!("{trait_name}.")).map(str::to_string), _ => None }).collect::<Vec<_>>();
        for d in &program.declarations {
            let a::Declaration::Function(f) = d else { continue };
            if f.required { continue; }
            let Some((owner, method)) = f.name.split_once('.') else { continue };
            if owner.starts_with('#') || !names.iter().any(|n| n == method) { continue; }
            if !direct().any(|i| i.trait_name == *trait_name && i.target == owner) {
                return Err(error("a type has a method named like one of a trait that has an impl for every type; rename it or implement the trait"));
            }
        }
    }
    let implemented: Implemented = direct().map(|i| ((i.trait_name.clone(), i.target.clone()), i.bounds.clone())).collect();
    let mut trait_arguments = TraitArguments::new();
    for i in direct() {
        // An impl with type parameters of its own names them in its arguments (`impl<T> Walk<T>
        // for Vec<T>`): they are no types on their own, and a bound with arguments is not met by
        // such an impl yet (`satisfies`).
        if !i.parameters.is_empty() && !i.arguments.is_empty() {
            // Each argument is still a type in reach, read with the impl's own parameters as names.
            for argument in &i.arguments {
                layouts.ty_in(argument, crate::compiler::logical_body::FunctionId(u32::MAX), &i.parameters)?;
            }
            // A callable's shape is checked on its arguments; with parameters of its own an
            // impl's arguments are not read, so a call trait takes no such impl.
            if i.trait_name.starts_with("WitchyFn") || crate::compiler::methods::once_bound(&i.trait_name) {
                return Err(error("a callable implemented with type parameters of its own is not supported: write the impl for a type without them"));
            }
            continue;
        }
        let arguments = i.arguments.iter().map(|t| layouts.ty(t)).collect::<Result<Vec<_>>>()?;
        // A callable written by hand takes values or shared references and gives a result that
        // holds no reference: any other is never usable (calls through the trait refuse it), so
        // it is refused where it is written.
        let call_trait = i.trait_name.strip_prefix("WitchyFn").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if call_trait && let Some((result, taken)) = arguments.split_last()
            && (taken.iter().any(|t| matches!(t, Ty::Borrow { mutable: true, .. })) || layouts.lends(*result) || matches!(result, Ty::Borrow { .. })) {
            return Err(error("a callable takes values or shared references, and its result holds no reference"));
        }
        trait_arguments.insert((i.trait_name.clone(), i.target.clone()), arguments);
    }
    let traits = program.declarations.iter().filter_map(|d| match d {
        a::Declaration::Function(f) if f.required => f.name.split_once('.').map(|(t, _)| t),
        _ => None,
    }).chain(program.implements.iter().map(|i| i.trait_name.as_str())).collect::<BTreeSet<_>>();
    for declaration in &program.declarations {
        let a::Declaration::Function(f) = declaration else { continue; };
        // Placed at the function, naming the bound (it had no place: a library's was found nowhere).
        if let Some(bound) = f.bounds.iter().flatten().find(|bound| !traits.contains(crate::compiler::methods::bound_name(bound)) && !matches!(bound.as_str(), "Brand" | "Fresh")) {
            let shown = crate::compiler::methods::bound_name(bound).split('@').next().unwrap_or(bound);
            return Err(Diagnostic::new("W0401", f.span.clone(), format!("a type parameter bound names a declared trait or `Any`: `{shown}` in `{}` is not a trait this program has", f.name.split('@').next().unwrap_or(&f.name))));
        }
        // A `Fresh` parameter is given only by an opening: no parameter may name it, so nothing
        // passed in can carry it (docs/design-brands.md).
        // One `Fresh` type parameter per function, the impl's counted: an opening offers one brand,
        // so two would name the same numbering (review: `issue<A: Fresh, B: Fresh>` let their rows mix).
        if f.bounds.iter().filter(|bounds| bounds.iter().any(|b| b == "Fresh")).count() > 1 {
            return Err(Diagnostic::new("W0401", f.span.clone(), "a function has one `Fresh` type parameter: an opening makes one new numbering, so a second would be the same one. Open twice for two"));
        }
        for (index, bounds) in f.bounds.iter().enumerate() {
            if !bounds.iter().any(|b| b == "Fresh") { continue; }
            let name = &f.generics[index];
            if f.parameters.iter().any(|(_, t)| type_mentions(t, name)) {
                return Err(Diagnostic::new("W0401", f.span.clone(), "a `Fresh` type parameter is new at each opening, so no parameter's type may name it; only the result does"));
            }
        }
    }
    let mut assertion_sites = BTreeMap::new();
    let mut functions = Vec::new();
    let mut effects: Vec<crate::compiler::effects::Facts> = Vec::new();
    let mut first: Option<Diagnostic> = None;
    let mut others: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for key in signatures.keys() {
        if let Some((name, file)) = key.rsplit_once('@') && file.parse::<u32>().is_ok() { others.entry(name.to_string()).or_default().push(key.clone()); }
    }
    let required_result = program.test_reporter.as_ref().map(|key| {
        signatures.get(key).and_then(|s| s.params.first()).copied().ok_or_else(|| error("missing checked test reporter"))
    }).transpose()?;
    LATER.with(|later| later.borrow_mut().clear());
    for declaration in &program.declarations {
        let a::Declaration::Function(f) = declaration else {
            continue;
        };
        *at = (f.span != (0..0)).then(|| f.span.clone());
        let span = at.clone();
        // One function at a time: a refusal is kept and the rest are still checked, so one
        // build reports a refusal in each function that has one (`LATER`).
        let needs_contract=std::cell::Cell::new(false);
        let mut one = || -> Result<()> {
            let sig = &signatures[&f.name];
            let assertion_result = program.test_members.contains(&f.module).then_some(required_result).flatten();
            let mut checker = Checker {
                test_edges: &program.test_edges, assertion_result,
                signatures: &signatures,
                others: &others,
                inline_generators: &program.inline_generators,
                inlining: Vec::new(),
                definition: sig.definition,
                module: f.module,
                derived: program.derived.iter().find(|d| d.recipe.contains(&f.span.start) && d.recipe_module == f.module
                    && f.name.split('.').next() == Some(d.target.as_str())).cloned(),
                private: &program.private,
                imports: &program.imports,
                reach: &reach,
                operator: false,
                generics: sig.generics.clone(),
                calls: Vec::new(),
                sites: Vec::new(),
                contract_sites: 0,
                call_spans: Vec::new(),
                operands: None,
                hint_work: 0,
                hint_memo: BTreeMap::new(),
                hinting: 0,
                hint_chain: std::ptr::null(),
                hint_reach: 0,
                needs_contract: &needs_contract,
                entry: f.name == "main",
                nopanic_body:f.nopanic.is_some(),
                contracted: !f.requires.is_empty()||!f.ensures.is_empty(),
                suspends: f.suspends,
                proof_requests: 0,
                memory_proof: false,
                returning: false,
                scalar_proof: false,
                view_parameters: BTreeSet::new(),
                brief_lend: false,
                memory_element: None,
                record_proof: None,
                layouts: &mut layouts,
                scope: BTreeMap::new(),
                bound_at: BTreeMap::new(),
                moved_at: BTreeMap::new(),
                local_names: BTreeMap::new(),
                locals: Vec::new(),
                flow: Flow::default(),
                loops: Vec::new(),
                yields: Vec::new(),
                chosen: Vec::new(),
                held: None,
                checks: 0,
                element_limits: Vec::new(),
                steps: BTreeSet::new(),
                overwrites: BTreeSet::new(),
                guarded: crate::compiler::guarded::counters(&f.body),
                open_numbers: false,
                result: sig.result,
                written_result: f.result.clone(),
                nodes: 0,
                at: 0..0,
                nesting: 0,
                addressed: BTreeSet::new(),
                loans: BTreeMap::new(),
                paths: Vec::new(),
                parameters: sig.params.len() as u32,
                carried: Vec::new(),
                split: None,
                pending_parts: None,
                returned_at: 0..0,
                returned_payload_at: None,
                local_parts: BTreeMap::new(),
                own_borrows: sig.borrows.clone(),
                parameter_names: f.parameters.iter().map(|(name, _)| name.clone()).collect(),
                holds: BTreeMap::new(),
                try_exit: None,
                adopt: Vec::new(),
                opened_views: Vec::new(),
                bound: None,
                bounds: sig.bounds.clone(),
                implemented: &implemented,
                blankets: &blankets,
                trait_arguments: &trait_arguments,
                opening: None,
                fresh_for: None,
                opened: false,
                for_reach: BTreeSet::new(),
                for_outer: None,
            };
            if let Some(derived) = checker.derived.clone().filter(|d| d.module != f.module) {
                if let Some(given) = &derived.for_type {
                    let ty = checker.layouts.ty_in(given, sig.definition, &sig.generics)
                        .map_err(|_| Diagnostic::new("W0100", f.span.clone(), "a derive's `for:` names a type the deriving file declares or imports"))?;
                    checker.for_reach = guarded_names(checker.layouts, ty, f.module);
                    let mut outer = ty;
                    while let Ty::Borrow { pointee, .. } = outer { outer = pointee.ty(); }
                    checker.for_outer = checker.layouts.nominal(outer).map(str::to_string);
                }
                // The target's fields are the user's choice too (`static for f in fields:` writes
                // through each): what they reach is not the recipe file's either (review round 5:
                // a field of the recipe module's sealed `Token` was written). The target itself
                // stays reachable.
                let target = (0..=sig.generics.len()).find_map(|k| {
                    let named = a::Type::Named(derived.target.clone(), sig.generics[..k].iter().map(|g| a::Type::Named(g.clone(), Vec::new())).collect());
                    checker.layouts.ty_in(&named, sig.definition, &sig.generics).ok()
                });
                if let Some(target) = target {
                    let fields: Vec<Ty> = checker.layouts.get(target).map(|a| a.fields.iter().map(|f| f.ty).collect()).unwrap_or_default();
                    let arms: Vec<Ty> = checker.layouts.sum(target).map(|s| s.arms.clone()).unwrap_or_default();
                    for held in fields.into_iter().chain(arms) { checker.for_reach.extend(guarded_names(checker.layouts, held, f.module)); }
                    if let Some(name) = checker.layouts.nominal(target) { checker.for_reach.remove(name); }
                }
            }
            effects.push(crate::compiler::effects::Facts { suspends: f.suspends, public: f.public, nosuspend: f.nosuspend.clone(), quiet: f.quiet.clone(), sites: Vec::new() });
            if f.required {
                // A trait method: a signature only. Each call is replaced by the implementing type's
                // method. Its contract, over its numbers, is what generic callers show and rely on;
                // every implementation proves it (methods.rs gives it the clauses).
                let contract = if checker.contracted {
                    let (mut parameters, mut bools, mut scalars, mut integers) = (BTreeMap::new(), BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
                    for ((name, _), t) in f.parameters.iter().zip(&sig.params) {
                        let local = checker.new_local(*t, false, None)?;
                        parameters.insert(name.clone(), local.id);
                        if *t == Ty::Bool { bools.insert(local.id); }
                        if matches!(t, Ty::Unsigned(_) | Ty::Bool | Ty::I64) { scalars.insert(local.id); }
                        if *t == Ty::I64 { integers.insert(local.id); }
                    }
                    let result = matches!(sig.result, Ty::Unsigned(_) | Ty::Bool | Ty::I64).then_some(sig.result == Ty::Bool);
                    Some(crate::compiler::contract::resolve(f, &parameters, &bools, &scalars, &integers, sig.result == Ty::I64, result, None,
                        &|local, method| checker.observer(local, method), &|local, names, starred| checker.clause_field(local, names, starred))
                        .map_err(|e| if e.message.starts_with("function contracts require ensures") { error("a trait method's contract has an `ensures`, as every contract does") } else { error("a trait method's contract speaks of its numbers, its result, and pure methods of `self` only") })?)
                } else { None };
                functions.push(Function {
                    definition: sig.definition,
                    type_parameters: vec![TypeParameter{ty:Ty::Param{owner:sig.definition,index:0},name:"Self".into(),family:ParameterFamily::AnyStorage,work:sig.bounds.first().is_some_and(|b|b.iter().any(|b|crate::compiler::methods::once_bound(b))),bounded:true}],
                    calls: Vec::new(), contract, name: f.name.clone(), checked_nodes: 0, params: sig.params.clone(), result: sig.result,
                    locals: std::mem::take(&mut checker.locals), local_names: BTreeMap::new(), body: Vec::new(), addressed: BTreeSet::new(), checks: 0, element_limits: Vec::new(), required: true, nopanic: None, module: f.module, pure: f.pure, suspends: f.suspends, steps: BTreeSet::new(), overwrites: BTreeSet::new(), call_spans: Vec::new(), span: (f.span.start, f.span.end),
                });
                return Ok(());
            }
            for ((name, _), t) in f.parameters.iter().zip(&sig.params) {
                let local = checker.new_local(*t, false, None)?;
                if checker.layouts.views(*t) { checker.view_parameters.insert(local.id); }
                if checker.layouts.lends(*t) && !matches!(t, Ty::Borrow { .. }) {
                    // A by-value sum can carry references supplied by the caller.
                    // Its parameter identity stands for those external origins, not
                    // for the address of the parameter's own local carrier slots.
                    checker.holds.insert(local.id, vec![local.id]);
                }
                if { checker.local_names.insert(local.id, name.clone()); checker.scope.insert(name.clone(), local).is_some() } {
                    return Err(error("duplicate parameter"));
                }
            }
            let contract=if checker.contracted{
                let parameters:BTreeMap<String,u32>=checker.scope.iter().map(|(n,l)|(n.clone(),l.id)).collect();
                // A proof reads a returned shared reference as the value it points at.
                let plain=checker.layouts.plain(sig.result)?;
                let typed:BTreeMap<String,(u32,Ty)>=checker.scope.iter().map(|(n,l)|(n.clone(),(l.id,l.ty))).collect();
                let modelled=if crate::compiler::memory_contract::eligible(sig.definition,sig.generics.len(),&sig.params,plain,checker.layouts) {
                    Some(crate::compiler::memory_contract::resolve(f,&typed,checker.layouts,plain,sig.definition))
                }else if crate::compiler::record_contract::eligible(sig.definition,sig.generics.len(),&sig.params,sig.result,checker.layouts) {
                    Some(crate::compiler::record_contract::resolve(f,&typed,checker.layouts,sig.result,sig.definition))
                }else if crate::compiler::storage_contract::eligible(sig.definition,sig.generics.len(),&sig.params,sig.result,checker.layouts) {
                    Some(crate::compiler::storage_contract::resolve(f,&typed))
                }else{None};
                // A type's invariant, for a proof over numbers (docs/proposal-scalar-invariants.md).
                let (assumed,shown)=invariant_clauses(f,&sig.params,sig.result,checker.layouts);
                // Copies of the whole function (body and all) only where they differ from it: most
                // functions have no invariant clauses (a copy per contract was a profile's hot spot).
                let augmented=(!shown.is_empty()).then(||{let mut augmented=f.clone();augmented.ensures.extend(shown);augmented});
                let assuming=(!assumed.is_empty()).then(||{
                    let mut assuming=f.clone();assuming.requires=assumed;
                    // Resolved only for its `requires`; a contract has at least one `ensures`.
                    assuming.ensures=vec![a::Postcondition{kind:a::PostconditionKind::Predicate(a::Expr{kind:a::Expression::Bool(true),span:f.span.clone()}),span:f.span.clone()}];
                    assuming
                });
                let f=augmented.as_ref().unwrap_or(f);
                let scalar=||{
                    let bools=checker.scope.values().filter(|l|l.ty==Ty::Bool).map(|l|l.id).collect();
                    let scalars=checker.scope.values().filter(|l|matches!(l.ty,Ty::Unsigned(_)|Ty::Bool|Ty::I64)).map(|l|l.id).collect();
                    let result=matches!(sig.result,Ty::Unsigned(_)|Ty::Bool|Ty::I64).then_some(sig.result==Ty::Bool);
                    let integers=checker.scope.values().filter(|l|l.ty==Ty::I64).map(|l|l.id).collect();
                    let resolve=|f:&a::Function|crate::compiler::contract::resolve(f,&parameters,&bools,&scalars,&integers,sig.result==Ty::I64,result,checker.layouts.sum(sig.result).map(|s|s.names.as_slice()),&|local,method|checker.observer(local,method),&|local,names,starred|checker.clause_field(local,names,starred));
                    let mut contract=resolve(f)?;
                    if let Some(assuming)=&assuming{contract.assumed=resolve(assuming)?.requires;}
                    // Invariants over a container's elements: each clause, and how many elements
                    // there are, resolved as a `requires` is (two clauses an invariant).
                    let over=element_clauses(f,&sig.params,checker.layouts)?;
                    if !over.is_empty(){
                        let mut reading=f.clone();
                        reading.requires=over.iter().flat_map(|clause|[clause.body.clone(),a::Expr{kind:a::Expression::Binary("<=",Box::new(a::Expr{kind:a::Expression::Integer(0),span:clause.span.clone()}),Box::new(clause.upper.clone())),span:clause.span.clone()}]).collect();
                        reading.ensures=vec![a::Postcondition{kind:a::PostconditionKind::Predicate(a::Expr{kind:a::Expression::Bool(true),span:f.span.clone()}),span:f.span.clone()}];
                        let resolved=resolve(&reading)?.requires;
                        for (clause,pair) in over.iter().zip(resolved.chunks(2)){
                            let [body,crate::compiler::contract::Clause{value:crate::compiler::contract::Prop::Compare(_,_,upper),..}]=pair else{return Err(Diagnostic::new("W0401",clause.span.clone(),"a type invariant over elements ends its range at the length of a field"))};
                            contract.elements.push(crate::compiler::contract::ElementInvariant{upper:upper.clone(),body:body.value.clone(),span:clause.span.clone()});
                        }
                    }
                    Ok(contract)
                };
                // A function that only reads what it is given, and never says `memory.` in its body
                // or its clauses, needs no memory model: the proof over numbers reads it, and follows
                // more kinds of body. One that can change or consume a value keeps the memory model,
                // which is what covers a type's invariant.
                let numbers=!crate::compiler::lending::function_names(f,"memory")
                    &&sig.params.iter().all(|ty|matches!(ty,Ty::Borrow{mutable:false,..})||checker.layouts.properties(*ty).copy);
                Some(match modelled{
                    Some(Ok(contract))=>match (numbers&&contract.memory.is_some()).then(||scalar()){Some(Ok(plain))=>plain,_=>contract},
                    // The clauses are not in a memory or record model's vocabulary: they may still be
                    // plain claims about numbers and fields read through methods.
                    // A type's invariant that does not make sense is reported as itself, before any other model is tried.
                    Some(Err(refusal)) if refusal.message.starts_with("a type invariant")=>return Err(refusal),
                    Some(Err(refusal))=>scalar().map_err(|_|refusal)?,
                    None=>scalar()?,
                })
            }else{None};
            checker.scalar_proof = contract.as_ref().is_some_and(|c|c.memory.is_none()&&c.record.is_none()&&c.storage.is_none());
            if checker.scalar_proof && let Some(name) = invariant_escape(f, &sig.params, &|ty| checker.scalar_invariant(ty)) {
                return Err(Diagnostic::new("W0401", f.span.clone(), format!("`{name}`'s fields are written here, which may break its type's invariant until the function returns: use it only through its fields (`(*{name}).field`), not lent, read whole or with a method called on it")));
            }
            checker.memory_proof = contract.as_ref().is_some_and(|c|c.memory.is_some());
            checker.memory_element = contract.as_ref().and_then(|c|c.memory.as_ref()).map(|memory|memory.element);
            checker.record_proof = contract.as_ref().and_then(|c|c.record.as_ref()).map(|record|(sig.params.len() as u32,record.element));
            let mut entry = Vec::new();
            if f.name == "main" {
                // Each secret parameter is its slot, numbered in order (`host::secret_slot`).
                let mut slots = 0;
                let mut dirs = 0;
                for (name, source_ty) in &f.parameters {
                    let ty = checker.layouts.ty_in(source_ty,checker.definition,&checker.generics)?;
                    let local = checker.new_local(ty, false, None)?;
                    if { checker.local_names.insert(local.id, name.clone()); checker.scope.insert(name.clone(), local).is_some() } { return Err(error("duplicate main capability name")); }
                    let kind = match ty { Ty::Console => Value::EntryConsole, Ty::Input => Value::EntryInput,
                        Ty::Host(host) if let Some(kind) = host.slot_kind() => { slots += 1; Value::EntryHost(host, crate::compiler::host::secret_slot(slots - 1, kind)) }
                        Ty::Host(host @ crate::compiler::host::Host::Dir(_)) if program.test_adapter == Some(f.module) => {
                            dirs += 1;
                            Value::EntryHost(host, 16 * dirs + host.code())
                        }
                        Ty::Host(host) => Value::EntryHost(host, host.code()), _ => Value::EntryAllocator };
                    entry.push(Stmt::Set(local.id, Expr { ty, kind }));
                }
            }
            checker.checks += 1; // actual source body checking entry, not a post-hoc inventory
            // A function with a result may end in its value: a last expression is returned, through
            // a last `if`/`else`, `match`, `using` or block (book: tour-functions.md). Such a body
            // was refused before ("can fall through"), so no program that compiled changes meaning.
            let mut source_body = f.body.clone();
            checker.labels(&mut source_body,&mut assertion_sites,&f.name)?;
            deref_lets(&mut source_body);
            if sig.result != Ty::Unit { source_body = tail(source_body); }
            let checked = checker.block(&source_body);
            // Fail closed: a caller that asked for a hint and went on without one must not decide
            // what the program means, or which refusal is shown, once the work has run out.
            if checker.hint_work > HINT_WORK { return Err(Diagnostic::new("W0401", f.span.clone(), HINT_WORK_MESSAGE)); }
            let (mut body, returns) = checked?;
            if sig.result != Ty::Unit && !returns {
                return Err(error(format!("function can fall through without a return: the end of `{}` can be reached, and it returns a value", f.name.split('@').next().unwrap_or(&f.name))));
            }
            let expired = checker.expire_visible()?;
            if !expired.is_empty() { body.push(Stmt::Expire(expired)); }
            entry.extend(body);
            let body = entry;
            effects.last_mut().expect("pushed above").sites = std::mem::take(&mut checker.sites);
            functions.push(Function {
                definition: sig.definition,
                type_parameters: sig.generics.iter().enumerate().map(|(index,name)|TypeParameter{ty:Ty::Param{owner:sig.definition,index:index as u32},name:name.clone(),family:if sig.any[index]{ParameterFamily::AnyStorage}else{ParameterFamily::CopyStorage},work:sig.bounds[index].iter().any(|b|crate::compiler::methods::once_bound(b)),bounded:!sig.bounds[index].is_empty()}).collect(),
                calls: checker.calls,
                contract,
                name: f.name.clone(),
                checked_nodes: checker.nodes,
                params: sig.params.clone(),
                result: sig.result,
                locals: checker.locals,
                local_names: checker.local_names,
                body,
                addressed: checker.addressed,
                checks: checker.checks,
                element_limits: checker.element_limits,
                steps: checker.steps,
                overwrites: checker.overwrites,
                call_spans: checker.call_spans,
                span: (f.span.start, f.span.end),
                required: false,
                nopanic: f.nopanic.clone(),
                module: f.module,
                pure: f.pure,
                suspends: f.suspends,
            });
            Ok(())
        };
        let outcome = if first.is_none() { one() } else {
            // After a refusal, state another function left may not hold: a panic ends the list.
            let hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(one));
            std::panic::set_hook(hook);
            match caught { Ok(outcome) => outcome, Err(_) => break }
        };
        if needs_contract.get() && f.name!="main" && f.ensures.is_empty() {
            required.insert(f.name.clone());
        }
        if let Err(mut e) = outcome {
            if e.span == (0..0) && let Some(span) = span { e.span = span; }
            // The same refusal at the same place, made again in another function (a type's
            // invariant outside the clauses' vocabulary is resolved for each function that
            // receives the type), is said once.
            let said = first.as_ref().is_some_and(|f: &Diagnostic| f.span == e.span && f.message == e.message)
                || LATER.with(|later| later.borrow().iter().any(|l| l.span == e.span && l.message == e.message));
            if said {}
            else if first.is_none() { first = Some(e); }
            else { LATER.with(|later| later.borrow_mut().push(e)); }
            if LATER.with(|later| later.borrow().len()) >= 20 { break; }
        }
    }
    *at = None;
    if let Some(first) = first { return Err(first); }
    for (&id,span) in &program.proof_views {
        crate::compiler::scalar_vc::view_projection(&functions,id).map_err(|mut e|{e.span=span.clone();e})?;
    }
    layouts.validate_reference_storage()?;
    let implements = program.implements.iter().map(|i| (i.trait_name.clone(), i.target.clone())).collect();
    Ok(Program { assertion_sites,
        functions,
        main,
        types: layouts,
        effects,
        implements,
        secret_slots,
        main_parameters: crate::compiler::caps::main_grant(program),
    })
}
