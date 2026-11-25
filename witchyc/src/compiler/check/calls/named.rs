//! Resolution and ownership checks for calls to named functions.
use super::super::invariants::reached_types;
use super::*;
use std::ops::Range;

impl Checker<'_> {
    pub(super) fn named_call(
        &mut self,
        name: &str,
        name_span: &Range<usize>,
        call_span: &Range<usize>,
        args: &[a::Expr],
        expected: Option<Ty>,
    ) -> Result<(Ty, Value)> {
        let sig = self
            .signatures
            .get(&self.function_named(name))
            .ok_or_else(|| self.unknown_function(name, name_span))?
            .clone();
        // In a derived method, by `may_reach`: the target's private methods are the recipe's to
        // call, as when its code was read in the type's file; the `For` type's are not, even
        // in the recipe's own file (review round 3: `For.mk(v)` minted its sealed type).
        let owner = name.rsplit_once('.').map(|(owner, _)| owner);
        // And a private function of the recipe's file whose signature reaches a type the
        // user chose (`For`, the target's fields) is not the recipe's to call there: it
        // could build or write it (review round 5: `set_v(&mut t, 999)`).
        // Only types this file seals or keeps an invariant on matter: an open type's fields are
        // anyone's to write (review round 6: private helpers over an open `Point` were refused).
        let steers = |this: &Self, types: &[Ty]| !sig.public && sig.module == this.module && !this.for_reach.is_empty()
            && types.iter().any(|t| reached_types(this.layouts, *t).into_iter().any(|r| this.layouts.nominal(r).is_some_and(|n| this.for_reach.contains(n))
                && (this.layouts.sealed_in(r) == Some(this.module) || this.layouts.invariants(r).is_some_and(|(m, _)| m == this.module))));
        let written: Vec<Ty> = sig.params.iter().copied().chain(std::iter::once(sig.result)).collect();
        let test_import = owner.is_none() && self.derived.is_none() && self.test_edges.contains(&(self.module, sig.module));
        if let Some(refusal) = self.out_of_reach(name, &sig, name_span) { return Err(refusal); }
        if steers(self, &written) || (!sig.public && !self.may_reach(sig.module, owner) && !test_import) {
            return Err(Diagnostic::new("W0401", name_span.clone(), format!("`{name}` is private to the module that declares it; that module marks what it offers with `pub`")));
        }
        if args.len() != sig.params.len() {
            return Err(Diagnostic::new("W0401", call_span.clone(), format!("call argument count mismatch: `{name}` takes {}, and {} {} given", sig.params.len(), args.len(), if args.len() == 1 { "is" } else { "are" })));
        }
        let substitutions=self.infer_call(&sig,args,expected,0,&mut 0)?;
        for (given, wanted) in substitutions.iter().zip(&sig.bounds) {
            for bound in wanted {
                let arguments = self.bound_arguments(bound, sig.definition, &sig.generics, Some(&substitutions))?;
                self.satisfies(*given, crate::compiler::methods::bound_name(bound), &arguments)?;
            }
        }
        let params=sig.params.iter().map(|t|self.layouts.substitute(*t,sig.definition,&substitutions)).collect::<Result<Vec<_>>>()?;
        let result=self.layouts.substitute(sig.result,sig.definition,&substitutions)?;
        // The same with the types inferred: a generic helper given a chosen type (review
        // round 6: `poke_it<X: Poke>(&mut (*self).t)` wrote the sealed `Token`).
        let inferred: Vec<Ty> = params.iter().copied().chain(std::iter::once(result)).collect();
        if steers(self, &inferred) {
            return Err(Diagnostic::new("W0401", name_span.clone(), format!("`{name}` is private to the module that declares it, and here it would be given a type the deriving file chose")));
        }
        let edge=CallEdge{definition:sig.definition,substitutions:substitutions.clone()};
        self.sites.push((edge.clone(),(call_span.start,call_span.end)));
        if !self.calls.contains(&edge){self.calls.push(edge);}
        // Inference changes no ownership. Evaluate/check actual arguments ONCE in source order.
        let (saved, mark) = (self.loans.clone(), self.carried.len());
        // A reference to a place that is there whatever the other arguments do is taken
        // last: `items.push_mut(allocator, items.len())` reads `items` before lending it.
        // The emitted order is the written one; taking the address runs nothing, so no
        // argument can tell the difference. A function proved by a memory or record
        // model keeps the written order: its models read arguments in place.
        let late = |this: &Self, arg: &a::Expr, t: Ty| matches!(t, Ty::Borrow { .. }) && this.still(arg);
        // What each argument borrowed, so that an argument the result cannot point into can be freed.
        let mut borrowed: Vec<Vec<u32>> = args.iter().map(|_| Vec::new()).collect();
        // Whether a `&mut` lent to this call ends with it, judged on the signature as
        // written and as given types here.
        let brief = self.ends_at_return(&sig.params, sig.result) && self.ends_at_return(&params, result);
        let mut lent = |this: &mut Self, index: usize, arg: &a::Expr, t: Ty| {
            let before = this.loans.clone();
            this.brief_lend = brief;
            let value = this.argument(arg, t);
            this.brief_lend = false;
            borrowed[index] = this.loans.iter().flat_map(|(root, (shared, unique))| {
                let had = before.get(root).copied().unwrap_or_default();
                std::iter::repeat_n(*root, shared.saturating_sub(had.0)).chain(std::iter::repeat_n(*root | UNIQUE, unique.saturating_sub(had.1)))
            }).collect();
            value
        };
        let reordered = !self.modelled() && args.iter().zip(&params).all(|(arg, t)| late(self, arg, *t) || !(matches!(t, Ty::Borrow { .. }) || self.layouts.lends(*t)));
        let args = if reordered {
            let mut checked: Vec<Option<Result<Expr>>> = args.iter().map(|_| None).collect();
            for references in [false, true] {
                for (index, (arg, t)) in args.iter().zip(&params).enumerate() {
                    if late(self, arg, *t) == references && !checked.iter().flatten().any(|done| done.is_err()) { checked[index] = Some(lent(self, index, arg, *t)); }
                }
            }
            // The first refusal in the order checked; every argument was reached unless one refused.
            let mut values = Vec::new();
            let mut refusal = None;
            for value in checked { match value { Some(Ok(value)) => values.push(value), Some(Err(e)) => { refusal.get_or_insert(e); } None => {} } }
            match refusal { Some(e) => Err(e), None => Ok(values) }
        } else { args.iter().zip(&params).enumerate().map(|(index,(arg,t))|lent(self,index,arg,*t)).collect::<Result<Vec<_>>>() };
        if args.is_err() { self.loans = saved.clone(); }
        let args=args?;
        if self.layouts.lends_unique(result) {
            for arg in &args { if let (Ty::Borrow { mutable: true, .. }, Value::Address(place)) = (arg.ty, &arg.kind) { self.lasting(place, true)?; } }
        }
        self.keep(saved, mark, result);
        // A result whose only references are `&mut` cannot come from anything the call
        // was lent shared: no `&mut` is made from a `&`, and a `&` points at no value
        // that holds a reference. So what the call borrowed shared is free again, as
        // it is after a call that returns no reference: `map.get_mut(&key)` keeps the
        // map, not the key.
        if self.layouts.lends_unique(result) && !self.layouts.lends_shared(result) {
            let (unique, shared): (Vec<u32>, Vec<u32>) = self.carried.split_off(mark).into_iter().partition(|root| root & UNIQUE != 0);
            self.carried.extend(unique);
            self.release(&shared);
        }
        // The same for each reference argument the result's references cannot point
        // into, by type: `map.get(&key)` keeps the map and not the key when no part of
        // a key can be the value, and a `&mut` cursor is free once `next` has returned.
        if self.layouts.lends(result) {
            // Judged on the signature as it is written: the function's own type
            // parameters are opaque to it, whatever they stand for at this call.
            for (taken, parameter) in borrowed.iter().zip(&sig.params) {
                let open = |t: Ty| open_parameter(t, sig.definition, &sig.generics, &sig.bounds);
                let index = borrowed.iter().position(|other| std::ptr::eq(other, taken)).expect("an argument");
                if !matches!(parameter, Ty::Borrow { .. }) || (self.layouts.may_lend_to(*parameter, sig.result, &open) && named_by_borrows(&sig.borrows, index)) { continue; }
                for root in taken {
                    if let Some(at) = self.carried[mark..].iter().position(|kept| kept == root) {
                        self.carried.remove(mark + at);
                        self.release(std::slice::from_ref(root));
                    }
                }
            }
        }
        // `&v` of a view lends its storage apart from what it carries (`VIEW_ITSELF`).
        // When no reference in the result can reach that storage, by type, the result
        // points only past the references `v` holds, whose loans it keeps: so `v`'s
        // storage is free again. A `Span` from `Words.next(&words, ...)` is part of the
        // String `words` points into, never of `words`, and may be returned.
        if self.layouts.lends(result) {
            let pointees = self.layouts.references(result);
            for arg in &args {
                let (Ty::Borrow { mutable: false, pointee }, Value::Address(place)) = (arg.ty, &arg.kind) else { continue };
                if place.pointer || !place.projections.is_empty() || self.layouts.lends_unique(pointee.ty()) { continue; }
                if pointees.iter().any(|(_, p)| self.layouts.owns_storage_of(pointee.ty(), *p)) { continue; }
                let entry = (place.identity, vec![VIEW_ITSELF]);
                let Some(index) = self.paths.iter().position(|known| *known == entry) else { continue };
                let root = FIELD_LOAN + index as u32;
                if let Some(at) = self.carried[mark..].iter().position(|kept| *kept == root) {
                    self.carried.remove(mark + at);
                    self.release(std::slice::from_ref(&root));
                }
            }
        }
        // What each variant of the result may carry: the loans taken by the arguments its
        // clause names. A loan no argument took (none should be) stays with every variant.
        self.split = None;
        if !sig.borrows.is_empty() {
            let kept = self.carried[mark..].to_vec();
            let parts = sig.borrows.iter().map(|from| kept.iter().copied().filter(|root| match from {
                None => true,
                Some(from) => from.iter().any(|index| borrowed[*index].contains(root)) || !borrowed.iter().any(|taken| taken.contains(root)),
            }).collect()).collect();
            self.split = Some((call_span.clone(), sig.definition, kept, parts));
        }
        let contract_site=if sig.contracted {let site=self.contract_sites;self.contract_sites+=1;self.call_spans.push((call_span.start,call_span.end));Some(site)}else{None};
        Ok((result, Value::Call(InstanceCall { definition:sig.definition, instance:None, contract_site, substitutions, args, result })))
    }
}
