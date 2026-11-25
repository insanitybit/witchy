use super::*;

impl Checker<'_> {
    /// The one-parameter method of this name on a local's type, for a clause that reads a field through it.
    pub(super) fn observer(&self, local: u32, method: &str) -> Option<(FunctionId, bool)> {
        let owner = match self.locals.get(local as usize)?.ty { Ty::Borrow { pointee, .. } => pointee.ty(), owner => owner };
        // A type parameter's: a pure method of a trait it is bounded by, read as a value of it.
        if let Ty::Param { index, .. } = owner {
            let bounds = self.signatures.values().find(|sig| sig.definition == self.definition)?.bounds.get(index as usize)?.clone();
            return bounds.iter().find_map(|bound| {
                let signature = self.signatures.get(&format!("{}.{method}", bound.split('<').next().unwrap_or(bound)))?;
                (signature.pure && signature.params.len() == 1 && matches!(signature.result, Ty::Unsigned(_))).then_some((signature.definition, false))
            });
        }
        let signature = self.signatures.get(&crate::compiler::methods::name(self.layouts.nominal(owner)?, method))?;
        (signature.params.len() == 1).then_some((signature.definition, signature.result == Ty::I64))
    }
    /// The number a clause names directly: the field at these names of a local, or of what the
    /// local refers to. With no names it is `*local`, a reference to a number. A sealed type's
    /// fields are named only in its own file, as its methods are the way to read it elsewhere.
    pub(super) fn clause_field(&self, local: u32, names: &[&str], starred: bool) -> Option<(Vec<u32>, Ty)> {
        let mut ty = if local == crate::compiler::contract::RESULT { self.result } else { self.locals.get(local as usize)?.ty };
        let mut shared = matches!(ty, Ty::Borrow { mutable: false, .. });
        match ty { Ty::Borrow { pointee, .. } if local != crate::compiler::contract::RESULT => ty = pointee.ty(), _ if starred || names.is_empty() => return None, _ => {} }
        let mut path = Vec::new();
        // `#armK` first: the value the result's variant K holds (a `match result:` arm's binder).
        let names = match names.split_first() {
            Some((first, rest)) if local == crate::compiler::contract::RESULT && first.starts_with("#arm") => {
                let k: u32 = first.strip_prefix("#arm")?.parse().ok()?;
                ty = *self.layouts.sum(ty)?.arms.get(k as usize)?;
                shared = matches!(ty, Ty::Borrow { mutable: false, .. });
                path.push(crate::compiler::scalar_vc::ARM | k);
                rest
            }
            _ => names,
        };
        // `xs.len()` of a slice: the length the prover follows for the slice's reads (`SLICE_LENGTH`).
        if let [only] = names && *only == "#len" && path.is_empty() && matches!(ty, Ty::Slice(_)) && local != crate::compiler::contract::RESULT {
            return Some((vec![crate::compiler::scalar_vc::SLICE_LENGTH], Ty::Unsigned(crate::compiler::scalar::Scalar::U64)));
        }
        // A returned `&[T]`'s length, keyed as a slice local's is (`result.len()` in an `ensures`).
        if let [only] = names && *only == "#len" && path.is_empty() && local == crate::compiler::contract::RESULT
            && matches!(ty, Ty::Borrow { mutable: false, pointee } if matches!(pointee.ty(), Ty::Slice(_))) {
            return Some((vec![crate::compiler::scalar_vc::SLICE_LENGTH], Ty::Unsigned(crate::compiler::scalar::Scalar::U64)));
        }
        for name in names {
            if *name == "#shared_projection" {
                if let Ty::Borrow { mutable: false, pointee } = ty { return Some((path, pointee.ty())); }
                return shared.then_some((path, ty));
            }
            shared = false;
            // The cells of a buffer, for an `elements.` clause: the path to the buffer itself.
            if *name == "#buffer" { return matches!(ty, Ty::Buffer(_)).then_some((path, Ty::U64)); }
            // `v[i]` in an invariant over a container's elements: any one element, as the
            // container's `index` method reads it (scalar_vc `ELEMENT_METHOD`, resolved there to
            // the buffer that method's body reads).
            if *name == "#element" {
                let owner = self.layouts.nominal(ty)?;
                let sig = self.signatures.get(&crate::compiler::methods::name(owner, "index"))?;
                let [Ty::Borrow { mutable: false, pointee }, Ty::Unsigned(Scalar::U64)] = sig.params.as_slice() else { return None };
                let mut bindings = vec![None; sig.generics.len()];
                self.layouts.unify(pointee.ty(), ty, sig.definition, &mut bindings).ok()?;
                ty = self.layouts.view_type(sig.result, sig.definition, &bindings, 0)?;
                path.push(crate::compiler::scalar_vc::ELEMENT_METHOD | sig.definition.0);
                continue;
            }
            if let Some(index) = name.strip_prefix("#payload") {
                let index: usize = index.parse().ok()?;
                ty = *self.layouts.sum(ty)?.arms.get(index)?;
                path.push(crate::compiler::scalar_vc::ARM + index as u32);
                continue;
            }
            if let Some(method) = name.strip_prefix("#view") {
                let (home,method)=crate::compiler::spec_expand::member_scope(method);
                let owner = self.layouts.nominal(ty)?;
                let sig = self.signatures.get(&crate::compiler::methods::name(owner, method))?;
                if !sig.public && home!=Some(sig.module) && !self.may_reach(sig.module, Some(owner)) { return None; }
                let [Ty::Borrow { mutable: false, pointee }] = sig.params.as_slice() else { return None };
                let mut bindings = vec![None; sig.generics.len()];
                self.layouts.unify(pointee.ty(), ty, sig.definition, &mut bindings).ok()?;
                let substitute = |t| self.layouts.view_type(t, sig.definition, &bindings, 0);
                // A method that answers a number (`items.len()`): the number it returns, which
                // the proof reads as the field its body returns (scalar_vc `read`, `projection`).
                // It is the last step: nothing is read past a number.
                if matches!(sig.result, Ty::Unsigned(_) | Ty::I64 | Ty::Bool) {
                    path.push(crate::compiler::scalar_vc::OBSERVE_METHOD | sig.definition.0);
                    ty = sig.result;
                    continue;
                }
                ty = match sig.result {
                    Ty::Borrow { mutable: false, pointee } => { shared = true; substitute(pointee.ty())? }
                    Ty::Weak(id) => {
                        let item = substitute(self.layouts.buffers()[id as usize].element)?;
                        Ty::Weak(self.layouts.buffers().iter().position(|b| b.element == item)? as u32)
                    }
                    _ => return None,
                };
                path.push(crate::compiler::scalar_vc::VIEW_METHOD | sig.definition.0);
                continue;
            }
            if *name == "#variant" && self.layouts.sum(ty).is_some() {
                path.push(crate::compiler::scalar_vc::SUM_TAG);
                ty = Ty::U64;
                continue;
            }
            if matches!(*name, "#incarnation" | "#address") && matches!(ty, Ty::Boxed(_) | Ty::Weak(_) | Ty::Buffer(_)) {
                let identity = *name == "#incarnation";
                path.push(if identity { crate::compiler::scalar_vc::BOX_ID } else { crate::compiler::scalar_vc::BOX_ADDRESS });
                ty = if identity { Ty::U64 } else { Ty::Unsigned(Scalar::U32) };
                continue;
            }
            let (home,name)=crate::compiler::spec_expand::member_scope(name);
            if !self.sealed_reach(ty) && (home.is_none() || self.layouts.sealed_in(ty)!=home) { return None; }
            // `xs[k]`, a literal index into a fixed array, is a step of its own (scalar_vc's
            // `ELEMENT`), below the array's length.
            if let Some(k) = name.strip_prefix('[').and_then(|n| n.strip_suffix(']')).and_then(|n| n.parse::<u32>().ok()) {
                let Some(Kind::Array(element, length)) = self.layouts.get(ty).map(|a| a.kind.clone()) else { return None };
                if (k as u64) >= length as u64 || k >= crate::compiler::scalar_vc::ELEMENT { return None; }
                path.push(crate::compiler::scalar_vc::ELEMENT | k);
                ty = element;
                continue;
            }
            let layout = self.layouts.get(ty)?;
            if !matches!(layout.kind, Kind::Record(_)) { return None; }
            let (ordinal, field) = layout.fields.iter().enumerate().find(|(_, field)| field.name == name)?;
            path.push(ordinal as u32);
            ty = field.ty;
        }
        matches!(ty, Ty::Unsigned(_) | Ty::I64 | Ty::Bool).then_some((path, ty))
    }
}
