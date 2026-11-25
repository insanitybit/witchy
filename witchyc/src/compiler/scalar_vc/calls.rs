use super::*;
use super::claims::Claims;
use super::super::record_contract as record;

impl Context<'_> {
    /// A call: its arguments, the `requires` of a scalar contract as an obligation here, and what the
    /// callee's contract says of numbers as facts after. A result that is one number is returned;
    /// what is known of any other result is in the `Outcome`.
    pub(super) fn call(&mut self, call: &InstanceCall) -> Result<(Option<Math>, Outcome)> {
        let functions = self.functions;
        let callee = &functions[call.definition.0 as usize];
        let claims = match &callee.contract {
            Some(contract) => match (&contract.record, &contract.memory, &contract.storage) {
                (Some(record), _, _) => Claims::record(record),
                (_, Some(memory), _) => Claims::memory(memory),
                (None, None, None) => Claims::scalar(functions, callee, contract)?,
                _ => Claims::default(),
            },
            None => Claims::default(),
        };
        // Each named part of an argument, as the callee receives it. A value (a record passed by
        // value) is copied when its argument is evaluated, as the emitted code copies it, so a later
        // argument that writes the record does not change it; through a reference the callee reads
        // the place when it runs, after every argument.
        let mut outcome = Outcome::default();
        let mut before: Vec<Option<Math>> = vec![None; claims.leaves.len()];
        let mut arguments = Vec::new();
        // A callee that only moves its values around (rearrangement.rs): what is known of each
        // goes where the callee puts it, as after a `memory.exchange` written here. Only when
        // every `&mut` argument is a place this walk follows; otherwise nothing is carried.
        let moves = crate::compiler::rearrangement::summary(functions, call.definition.0 as usize).filter(|summary| summary.after.len() == call.args.len()
            && call.args.iter().zip(&summary.after).all(|(argument, after)| after.is_none() || (fields_known(argument)
                && matches!(&argument.kind, Value::Address(place) | Value::ReadPlace(place) | Value::Move { place, .. } if !self.lent.contains(&place.root)))));
        let given = moves.as_ref().map(|summary| summary.given()).unwrap_or_default();
        // What is known of each argument the callee moves on, by parameter: each part's path and value.
        let mut carried: BTreeMap<u32, Vec<(Vec<u32>, Math)>> = BTreeMap::new();
        for (index, argument) in call.args.iter().enumerate() {
            let moved = given.contains(&(index as u32)) && !matches!(argument.ty, Ty::Borrow { .. });
            if tracked(argument.ty) {
                let value = self.number(argument)?;
                let value = self.bind(value);
                if moved { carried.insert(index as u32, vec![(Vec::new(), value.clone())]); }
                arguments.push(Argument::Scalar(value));
                continue;
            }
            let changed = matches!(argument.ty, Ty::Borrow { mutable: true, .. });
            match &argument.kind {
                Value::Address(place) | Value::ReadPlace(place) | Value::Move { place, .. } => {
                    self.place(place)?;
                    // Passed by value from a place: what is known of it now, as a `let` would take it.
                    if moved { let known = self.known_under(place); carried.insert(index as u32, known); }
                    arguments.push(match fields(place) { Some(path) => Argument::Place { root: place.root, path, changed }, None if changed => Argument::Place { root: place.root, path: prefix(place), changed }, None => Argument::Other });
                }
                // A value built in the call (`replace(slot, Full(move item))`): evaluated as a
                // `let` evaluates it, which also says what its parts are.
                _ if moved => {
                    let (mut writes, mut waiting) = (Vec::new(), None);
                    self.gather(CARRIED, &[], argument, &mut writes, &mut waiting)?;
                    carried.insert(index as u32, writes.into_iter().map(|((_, path), value)| (path, value)).collect());
                    arguments.push(Argument::Other);
                }
                _ => { self.effects(argument)?; arguments.push(Argument::Other); }
            }
            if !matches!(argument.ty, Ty::Borrow { .. }) && let Some(Argument::Place { root, path, .. }) = arguments.last().cloned() {
                // The record theorem identifies the value stored in a returned buffer.
                // Capture its actual by-value argument before later arguments run.
                if let Some(record) = callee.contract.as_ref().and_then(|c| c.record.as_ref())
                    && self.types.result_sum(call.result).is_none() && fields_known(argument) {
                    for (selected, pred) in &record.ensures {
                        let record::Predicate::Cell(buffer, record::Term::Literal(0), record::Term::Path(item)) = pred else { continue };
                        if selected.is_some() || !matches!(item.ty, Ty::Param { .. }) { continue; }
                        let steps = |p: &record::Path| p.fields.iter().map(|&i| i as u32).collect::<Vec<_>>();
                        let cell = join(&steps(buffer), &[BOX_VALUE]);
                        let value = steps(item);
                        let (source, dest) = match (&buffer.root, &item.root) {
                            (record::Root::Arm(0), record::Root::Parameter(p)) if *p as usize == index && !buffer.before => (value, cell),
                            (record::Root::Parameter(p), record::Root::Arm(0)) if *p as usize == index && buffer.before && !item.before => (cell, value),
                            _ => continue,
                        };
                        // A storable payload cannot contain a loan. Moving the actual
                        // item transfers its owned descendants with it, including each
                        // previously established, finite projection into their cells.
                        let source = join(&path, &source);
                        for ((local, fields), v) in &self.state {
                            if *local == root && let Some(rest) = fields.strip_prefix(source.as_slice())
                                && dest.len() + rest.len() <= 64 {
                                outcome.values.insert((None, join(&dest, rest)), v.clone());
                            }
                        }
                    }
                }
                for (at, leaf) in claims.leaves.iter().enumerate() {
                    if leaf.parameter == Some(index as u32) && tracked(leaf.ty) && fields_known(argument) { before[at] = Some(self.leaf((root, join(&path, &leaf.fields)), leaf.ty)); }
                }
            }
        }
        // A method that only returns a field is read as that field.
        if let (None, Some(path), [Argument::Place { root, path: base, .. }]) = (&callee.contract, projection(self.functions, callee).or_else(|| observed(callee)), arguments.as_slice()) {
            if fields_known(&call.args[0]) { return Ok((Some(self.leaf((*root, join(base, &path)), call.result)), Outcome::default())); }
        }
        for (at, leaf) in claims.leaves.iter().enumerate() {
            if before[at].is_some() { continue; }
            before[at] = match leaf.parameter.and_then(|parameter| arguments.get(parameter as usize)) {
                Some(Argument::Scalar(value)) if leaf.fields.is_empty() => Some(value.clone()),
                Some(Argument::Place { root, path, .. }) if tracked(leaf.ty) && matches!(call.args[leaf.parameter.expect("argument") as usize].ty, Ty::Borrow { .. }) && fields_known(&call.args[leaf.parameter.expect("argument") as usize]) => Some(self.leaf((*root, join(path, &leaf.fields)), leaf.ty)),
                _ => None,
            };
        }
        if !claims.requires.is_empty() {
            let requires = claims.requires.iter().map(|clause| clause.rewrite(&mut |m| match m {
                Math::Binding(index) => before[*index as usize].clone().ok_or_else(|| {
                    let mut refused = error("function contracts requires names a value this call does not pass by name: the callee's `requires` reads a part of an argument made in the call; bind the argument with `let` first and pass the name");
                    if let Some((start, end)) = call.contract_site.and_then(|site| self.function.call_spans.get(site as usize)) { refused.span = *start..*end; }
                    refused
                }),
                other => Ok(other.clone()),
            })).collect::<Result<Vec<_>>>()?;
            let site = call.contract_site.ok_or_else(|| error("function contracts call has no site"))?;
            if self.split_diagnostics && let Some(contract) = &callee.contract && contract.requires.len() == requires.len() {
                // Each clause alone, with its span, for a refusal that quotes the one not shown.
                for (clause, written) in requires.into_iter().zip(&contract.requires) {
                    self.obligation(clause, Kind::Call { site, callee: callee.name.clone() })?;
                    if let Some(last) = self.obligations.last_mut() { last.clause = Some((written.span.start, written.span.end)); }
                }
            } else {
                self.obligation(all(requires), Kind::Call { site, callee: callee.name.clone() })?;
            }
        }
        // What a `&mut` parameter points at is read when the callee runs, after every argument.
        if let Some(summary) = &moves {
            for (index, argument) in call.args.iter().enumerate() {
                if summary.after[index].is_none() || !given.contains(&(index as u32)) { continue; }
                let (Value::Address(place) | Value::ReadPlace(place) | Value::Move { place, .. }) = &argument.kind else { continue };
                let known = self.known_under(place);
                carried.insert(index as u32, known);
            }
        }
        // What the call does to elements is read from its contract, after its facts (elements.rs).
        let lent_elements = self.elements_before(&arguments);
        self.element_guard = true;
        for argument in &arguments { if let Argument::Place { root, path, changed: true } = argument { self.forget(*root, path); } }
        self.element_guard = false;
        let sum = self.types.sum(call.result).is_some();
        let mut result = tracked(call.result).then(|| self.fresh(call.result));
        if let Some(summary) = &moves {
            // Each `&mut` argument now holds what the callee left there. This comes before the
            // callee's `ensures`, which then speak of these same values.
            for (argument, after) in arguments.iter().zip(&summary.after) {
                let (Argument::Place { root, path, changed: true }, Some(piece)) = (argument, after) else { continue };
                let mut writes = Vec::new();
                placed(piece, path, &carried, &mut writes);
                for (fields, value) in writes { self.remember((*root, fields), value); }
            }
            let mut writes = Vec::new();
            placed(&summary.result, &[], &carried, &mut writes);
            for (fields, value) in writes {
                if fields.is_empty() { if tracked(call.result) { result = Some(value); } continue; }
                if tracked(call.result) { continue; }
                // A variant's value is also what a match on the result binds (`select`).
                if sum && let Some(arm) = fields.first().filter(|step| (ARM..OBSERVED_FLOOR).contains(*step)) {
                    outcome.values.insert((Some((arm - ARM) as u8), fields[1..].to_vec()), value.clone());
                }
                outcome.values.insert((None, fields), value);
            }
        }
        for (arm, clause) in &claims.ensures {
            let mut known = true;
            let fact = clause.rewrite(&mut |m| {
                let Math::Binding(index) = m else { return Ok(m.clone()) };
                let leaf = &claims.leaves[*index as usize];
                let value = match (leaf.parameter.map(|parameter| &arguments[parameter as usize]), tracked(leaf.ty).then_some(())) {
                    (_, None) => None,
                    // The argument after the call: unchanged unless it was lent with `&mut`.
                    (Some(Argument::Place { root, path, changed: true }), Some(_)) if !leaf.before && before[*index as usize].is_some() => Some(self.leaf((*root, join(path, &leaf.fields)), leaf.ty)),
                    (Some(_), Some(_)) => before[*index as usize].clone(),
                    (None, Some(_)) if !sum && leaf.fields.is_empty() => result.clone(),

                    // What the variant holds: `ARM | k` first in the contract, a path inside the
                    // payload here (the variant itself is the key's first part, and must agree).
                    (None, Some(_)) if sum && arm.is_some() && leaf.fields.first().is_some_and(|step| (ARM..OBSERVED_FLOOR).contains(step) && Some((step - ARM) as u8) != *arm) => None,
                    (None, Some(_)) => {
                        let fields = match leaf.fields.first() { Some(step) if sum && arm.is_some() && (ARM..OBSERVED_FLOOR).contains(step) => leaf.fields[1..].to_vec(), _ => leaf.fields.clone() };
                        let key = (if sum { *arm } else { None }, fields);
                        if !outcome.values.contains_key(&key) { let value = self.fresh(leaf.ty); outcome.values.insert(key.clone(), value); }
                        outcome.values.get(&key).cloned()
                    }
                };
                known &= value.is_some();
                Ok(value.unwrap_or(Math::Literal(0)))
            })?;
            // A clause that names something not followed here says nothing.
            if !known { continue; }
            match arm { Some(arm) if sum => outcome.facts.push((*arm, fact)), _ => self.hypotheses.push(fact) }
        }
        if !(self.watched.is_empty() && self.declared.is_empty()) {
            let mut resolved = Vec::new();
            for claim in &claims.elements {
                let mut known = claim.index.is_some();
                let index = match &claim.index { None => None, Some(index) => Some(index.rewrite(&mut |m| {
                    let Math::Binding(at) = m else { return Ok(m.clone()) };
                    let leaf = &claims.leaves[*at as usize];
                    let value = match leaf.parameter.map(|parameter| &arguments[parameter as usize]) {
                        Some(Argument::Place { root, path, changed: true }) if !leaf.before && tracked(leaf.ty) && before[*at as usize].is_some() => Some(self.leaf((*root, join(path, &leaf.fields)), leaf.ty)),
                        Some(_) => before[*at as usize].clone(),
                        None => None,
                    };
                    known &= value.is_some();
                    Ok(value.unwrap_or(Math::Literal(0)))
                })?) };
                resolved.push(index.filter(|_| known));
            }
            self.elements_after(call, lent_elements, &claims, &resolved, &arguments, &mut outcome)?;
        }
        self.excused = self.cell_lent(call, &arguments);
        let kept = self.lend_result(call, callee);
        self.excused = false;
        kept?;
        Ok((result, outcome))
    }
    fn lend_result(&mut self, call: &InstanceCall, callee: &Function) -> Result<()> {
        // A result that may hold a `&mut` may reach into what the call was lent with `&mut`: that
        // is written later through the result, so it is unknown from here on, whatever the
        // callee's contract said of it.
        // (A `&mut` is never a type argument, so the callee's declared result says.)
        if self.types.lends_unique(callee.result) {
            for (index, argument) in call.args.iter().enumerate() {
                // A place lent here is lent from its fields down (or to its first index); a
                // reference read out of a local and handed on is that whole local.
                match (argument.ty, &argument.kind) {
                    // A record lent whole: what the result can point into, by the callee's
                    // signature with its type parameters opaque, is lent (`narrowed`).
                    (Ty::Borrow { mutable: true, .. }, Value::Address(place) | Value::ReadPlace(place) | Value::Move { place, .. })
                        if let Some((base, lent)) = self.narrowed(argument, place, callee, index) => {
                        for ordinal in lent { self.lend_under(place.root, join(&base, &[ordinal])); }
                        // The root is lent, as for any kept `&mut` into it (`lent`).
                        self.lent.insert(place.root);
                    }
                    (Ty::Borrow { mutable: true, .. }, Value::Address(place)) => self.lend_under(place.root, prefix(place)),
                    (Ty::Borrow { mutable: true, .. }, Value::ReadPlace(place) | Value::Move { place, .. }) => self.lend_under(place.root, Vec::new()),
                    _ => {}
                }
            }
        }
        Ok(())
    }
    /// A kept `&mut` to a record, given to a call whose result's references cannot reach some of
    /// its number fields (`Types::unreachable_scalar_fields`, by the callee's signature): the
    /// path lent and the fields under it that are lent, the others keeping what the call left
    /// there (`Vec.at_mut` answers a `&mut T`, never a `&mut U64` into the header, so the
    /// length its `ensures` keeps stays known). A place by fields only (`(*ring).slots`), or a
    /// `&mut` parameter handed on as it is (`vector.at_mut(i)` in a method of `vector`), whose
    /// local stands for what it points at. `None`: the place is lent whole, as before.
    ///
    /// Only for a callee none of whose type parameters has a trait bound: an opaque `T` cannot be
    /// made a `&mut U64` into the header, but a bounded one can, through an impl for `U64` that
    /// is given the record (review of 2026-11-24, `review/probes/trait-grab3.wy`).
    fn narrowed(&self, argument: &Expr, place: &Place, callee: &Function, index: usize) -> Option<(Vec<u32>, Vec<u32>)> {
        if callee.required || callee.type_parameters.iter().any(|parameter| parameter.bounded) { return None; }
        let (parameter, result) = (callee.params.get(index).copied(), callee.result);
        let Some(Ty::Borrow { mutable: true, pointee }) = parameter else { return None };
        let whole = match &argument.kind {
            Value::Address(_) => fields(place).is_some_and(|path| path.len() == prefix(place).len()),
            Value::ReadPlace(_) | Value::Move { .. } => place.projections.is_empty() && !place.pointer
                && self.function.locals.get(place.root as usize).is_some_and(|local| matches!(local.ty, Ty::Borrow { mutable: true, .. })),
            _ => false,
        };
        if !whole { return None; }
        let kept = self.types.unreachable_scalar_fields(pointee.ty(), result);
        if kept.is_empty() { return None; }
        let count = self.types.get(pointee.ty())?.fields.len();
        Some((prefix(place), (0..count).filter(|ordinal| !kept.contains(ordinal)).map(|ordinal| ordinal as u32).collect()))
    }
}
