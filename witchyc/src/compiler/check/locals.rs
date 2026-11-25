use super::invariants::{has_invariant_view, heap_invariant};
use super::*;

impl Checker<'_> {
    /// A reference argument that does not fit: what it points at is another type, or it lends
    /// `&mut` what was not declared with `var`. The message starts as it always did.
    pub(super) fn borrow_mismatch(&self, given: Ty, wanted: Ty, mutable: bool, same: bool) -> Diagnostic {
        let name = |t: Ty| self.layouts.type_name(t).unwrap_or_else(|_| "?".into());
        let message = if same && mutable {
            "borrow pointee mismatch or insufficient root permission: `&mut` lends something that was not declared with `var`, or a reference that is not `&mut` itself".to_string()
        } else {
            format!("borrow pointee mismatch or insufficient root permission: this lends a `{}` where a reference to a `{}` is expected", name(given), name(wanted))
        };
        error(message)
    }
    pub(super) fn new_local(&mut self, ty: Ty, mutable: bool, span: Option<(usize, usize)>) -> Result<Local> {
        let id = self.locals.len() as u32;
        let properties = self.layouts.properties(ty);
        if !properties.copy && !matches!(ty, Ty::Borrow { .. }) {
            if !properties.implicit_discard && !self.layouts.supported_linear(ty) {
                return Err(error("unsupported nondiscardable owner type"));
            }
            self.flow.introduce(id);
        }
        let local = Local { id, ty, mutable, span };
        self.locals.push(local);
        Ok(local)
    }
    pub(super) fn proof_required(&self, message: &'static str) -> Diagnostic {
        if !self.contracted {
            self.needs_contract.set(true);
        }
        error(message)
    }

    /// A sealed type is built, written and taken apart only in its own file.
    pub(super) fn unsealed(&self, ty: Ty) -> Result<()> {
        if !self.sealed_reach(ty) {
            return Err(error("sealed type: only its defining file may construct it, write its fields or take its owner"));
        }
        // An invariant holds everywhere only if every place that could break it is proved to keep it.
        if self.layouts.invariants(ty).is_some() && self.record_proof.is_none() && !self.memory_proof {
            return Err(self.proof_required("a type with an invariant is built, changed or taken apart only in a function whose proof covers it"));
        }
        Ok(())
    }
    /// A proof over numbers covers this type's invariant (docs/proposal-scalar-invariants.md):
    /// the type is sealed in this file and its invariant is over its own fields, so the
    /// function's contract assumes it of what it receives and shows it of what it changes
    /// through `&mut` and of what it returns (`invariant_clauses`).
    pub(super) fn scalar_invariant(&self, ty: Ty) -> bool {
        if !self.scalar_proof { return false; }
        let Some((module, invariants)) = self.layouts.invariants(ty) else { return false };
        let memory = heap_invariant(invariants);
        self.may_reach(module, self.layouts.nominal(ty)) && !memory
    }
    /// `unsealed`, where a proof over numbers covers the invariant at this place.
    pub(super) fn unsealed_covered(&self, ty: Ty, covered: bool) -> Result<()> {
        if covered && self.scalar_invariant(ty) {
            if !self.sealed_reach(ty) {
                return Err(error("sealed type: only its defining file may construct it, write its fields or take its owner"));
            }
            return Ok(());
        }
        self.unsealed(ty)
    }
    /// Whether nothing a function with these parameters and this result returns or stores can
    /// carry a `&mut` it was lent past its return: the result holds no reference, and neither
    /// does any parameter other than a plain reference to a value that holds none. By type
    /// alone, whatever the function is: one that could hand a reference on is not of this kind.
    pub(super) fn ends_at_return(&self, params: &[Ty], result: Ty) -> bool {
        !self.layouts.lends(result) && params.iter().all(|t| match t {
            Ty::Borrow { pointee, .. } => !self.layouts.lends(pointee.ty()),
            other => !self.layouts.lends(*other),
        })
    }
    /// Whether every method an invariant of `ty` calls (other than `memory.`'s) is called with no
    /// argument on one of the type's own fields and answers a number, taking only `&` to that
    /// field's value (`items.len()`): a scalar observation, read by the proof over numbers as the
    /// field its one-line body returns, and no view of storage. Any other form is a view.
    pub(super) fn numbers_only_views(&self, ty: Ty) -> bool {
        let Some((_, clauses)) = self.layouts.invariants(ty) else { return false };
        let Some(record) = self.layouts.get(ty) else { return false };
        let fields: Vec<(String, Ty)> = record.fields.iter().map(|field| (field.name.clone(), field.ty)).collect();
        let mut numbers = true;
        for clause in clauses {
            clause.clone().visit_mut(&mut |e| {
                let a::Expression::Call(callee, arguments) = &e.kind else { return false };
                let a::Expression::Field(base, method) = &callee.kind else { return false };
                if matches!(&base.kind, a::Expression::Name(n) if n == "memory") { return false; }
                let observer = match &base.kind {
                    a::Expression::Name(n) if arguments.is_empty() => fields.iter().find(|(name, _)| name == n).and_then(|(_, field)| {
                        let owner = self.layouts.nominal(*field)?;
                        let signature = self.signatures.get(&crate::compiler::methods::name(owner, method))?;
                        Some(matches!(signature.params.as_slice(), [Ty::Borrow { mutable: false, .. }]) && matches!(signature.result, Ty::Unsigned(_) | Ty::I64 | Ty::Bool))
                    }).unwrap_or(false),
                    _ => false,
                };
                numbers &= observer;
                true
            });
        }
        numbers
    }
    /// `lends`: the place is lent with `&mut`, to be written by whoever holds the reference.
    /// `brief`: that reference ends when the call it is given to returns (`ends_at_return`).
    pub(super) fn unsealed_write(&self, place: &Place, lends: bool) -> Result<()> { self.unsealed_lend(place, lends, false) }
    pub(super) fn unsealed_lend(&self, place: &Place, lends: bool, brief: bool) -> Result<()> {
        let mut ty = match place.root_ty { Ty::Borrow { pointee, .. } => pointee.ty(), ty => ty };
        // A field of a `&mut` parameter's value: the contract shows its invariant at every return.
        // (`root_ty` is the pointee here; the parameter's own type says it is a `&mut`.)
        let through_parameter = place.pointer && place.root < self.parameters
            && self.locals.get(place.root as usize).is_some_and(|local| matches!(local.ty, Ty::Borrow { mutable: true, .. }));
        for (level, projection) in place.projections.iter().enumerate() {
            // Direct assignments and exchanges are covered by the post-state proof. A retained
            // mutable projection could write after that proof. One that ends with a call
            // (`brief`) writes only before that call returns: where this
            // function's proof shows the invariant at every return (the `&mut` parameter's own
            // type, covered by a proof over numbers), that is a direct write made by the callee,
            // and the invariant is owed here as after an exchange. The parameter is then used
            // only through its fields until the return (`invariant_escape`).
            // A view that is only numbers (`slots.len()`) names no storage: what the proof reads
            // of it is the header field the method returns, which a kept lend forgets unless
            // the callee's signature shows the reference cannot reach it (`lasting`).
            let numbers = level == 0 && through_parameter && matches!(projection, Projection::Field { .. }) && self.numbers_only_views(ty);
            if lends && !numbers && self.layouts.invariants(ty).is_some_and(|(_, clauses)| has_invariant_view(clauses)) {
                let refusal = error("to change storage named by a shared view invariant, consume and unpack the owner, then reconstruct it under proof");
                if !(brief && through_parameter && level == 0) { return Err(refusal); }
                if !self.scalar_invariant(ty) {
                    // Not yet checked under a proof: ask for one, as a direct write does.
                    return Err(if self.contracted { refusal } else { self.proof_required("a type with an invariant is built, changed or taken apart only in a function whose proof covers it") });
                }
            }
            self.unsealed_covered(ty, through_parameter && level == 0)?;
            let Some(layout) = self.layouts.get(ty) else { break };
            ty = match projection { Projection::Field { ordinal } => layout.fields[*ordinal as usize].ty, Projection::Index { .. } => layout.fields[0].ty };
        }
        Ok(())
    }
}
