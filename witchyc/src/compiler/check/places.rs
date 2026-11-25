use super::invariants::{invariant_free_field, invariant_observes_field};
use super::diagnostics::{distance, unknown_field};
use super::*;

impl Checker<'_> {
    /// A `&mut` reference that outlives the expression must not reach inside a sealed type:
    /// the writes made through it later are not seen by the proof that keeps the invariant.
    ///
    /// One case is no such write. In the type's own file, in a function whose proof over numbers
    /// covers the invariant (`scalar_invariant`), a reference into a field that no invariant of
    /// the type reads (`invariant_free_field`) can change nothing an invariant reads: a reference
    /// to a field reaches that field's storage and what it owns, never a sibling field. So the
    /// invariant holds whenever it held before, however long the reference lives and whoever
    /// holds it. The function's proof forgets what is under that field from the lend on, and
    /// nothing else (scalar_vc `lend_under`).
    ///
    /// A second case, for a reference a call keeps (`called`): `(*ring).slots.at_mut(i)` where
    /// the invariants read `slots` only through methods that answer a number (`slots.len()`),
    /// the field being one step from the value. The proof over numbers reads such a method as
    /// the field its body returns, and at the call it forgets, for good, every field of
    /// `slots` the result's references can reach by the callee's signature (scalar_vc `call`,
    /// `Types::unreachable_scalar_fields`). What it keeps is what the callee's `ensures` said
    /// and no later write through the result can change. So an invariant over `slots.len()`
    /// is shown at each return from facts that are still true, or not shown at all.
    pub(super) fn lasting(&self, place: &Place, called: bool) -> Result<()> {
        let mut ty = match place.root_ty { Ty::Borrow { pointee, .. } => pointee.ty(), ty => ty };
        let direct = place.projections.len() == 1;
        for projection in &place.projections {
            // In the type's own file, with no invariant to keep, the fields are the file's to write
            // and to lend.
            if !self.sealed_reach(ty) {
                return Err(error("a `&mut` reference into a field of a sealed type cannot be kept or returned outside its file, or at all if the type has an invariant"));
            }
            if let Some((_, clauses)) = self.layouts.invariants(ty) {
                let free = match projection {
                    Projection::Field { ordinal } => self.scalar_invariant(ty)
                        && self.layouts.get(ty).and_then(|layout| layout.fields.get(*ordinal as usize)).is_some_and(|field| invariant_free_field(clauses, &field.name)
                            || (called && direct && invariant_observes_field(clauses, &field.name))),
                    Projection::Index { .. } => false,
                };
                if !free {
                    return Err(error("a `&mut` reference into a field of a sealed type cannot be kept or returned outside its file, or at all if the type has an invariant: unless, in the type's own file and in a function proved over numbers, no invariant of the type reads that field"));
                }
            }
            let Some(layout) = self.layouts.get(ty) else { break };
            ty = match projection { Projection::Field { ordinal } => layout.fields[*ordinal as usize].ty, Projection::Index { .. } => layout.fields[0].ty };
        }
        Ok(())
    }
    pub(super) fn access(&self, root: u32, write: bool) -> Result<()> {
        let (shared, unique) = self.related(root);
        {
            if unique != 0 || (write && shared != 0) {
                let held = self.kept(root);
                return Err(error(if held { "this value is lent: a reference into it is still in use" } else { "root access conflicts with an active call-argument loan" }));
            }
        }
        Ok(())
    }
    pub(super) fn borrowed(&self, expr: &a::Expr) -> Result<Local> {
        if let a::Expression::Name(name) = &expr.kind {
            if let Some(local) = self.scope.get(name) {
                if matches!(local.ty, Ty::Borrow { .. }) { return Ok(*local); }
            }
        }
        Err(error(match &expr.kind {
            // `*(&n)`: written through, only `&mut` lends a place to write; read, it is `n`.
            a::Expression::Unary("&", _) => "`*(&place)` is the place itself, and nothing is written through `&`: write the place, or lend it with `&mut`",
            a::Expression::Name(name) if self.scope.contains_key(name) => "`*` reads through a reference, and this local holds a value, not a reference: use the name itself",
            _ => "`*` reads through a reference held in a name or parameter: bind the reference first (`let r = ...`), then `*r`",
        }))
    }
    pub(super) fn buffer_argument(&self, expr: &a::Expr) -> Result<u32> {
        let ty = match &expr.kind {
            a::Expression::Name(name) => match self.scope.get(name).map(|local| local.ty) {
                Some(Ty::Borrow { pointee: Pointee::Buffer(id), .. }) => Some(Ty::Buffer(id)), _ => None },
            a::Expression::Unary("&" | "&mut", value) => self.hint(value), _ => None,
        };
        if let Some(Ty::Buffer(id)) = ty { Ok(id) } else { Err(error("typed storage requires a Buffer reference")) }
    }
    pub(super) fn argument(&mut self, expr: &a::Expr, expected: Ty) -> Result<Expr> {
        if let Some(inner) = self.collapsed(expr, Some(expected)) { return self.argument(inner, expected); }
        if let Some(text) = self.text_as_str(expr, expected) { return self.expr_inner(&text, Some(expected)); }
        // `console.line("x ${n}")`: a format where a `&str` is expected; said as `expect` says it.
        if Some(expected) == self.layouts.known_str_borrow() && matches!(&expr.kind, a::Expression::Call(callee, _) if matches!(&callee.kind, a::Expression::Name(n) if n.starts_with("Format#"))) {
            return self.expect(expr, expected);
        }
        let brief = std::mem::take(&mut self.brief_lend);
        let Ty::Borrow { mutable, pointee } = expected else { return self.expect(expr, expected); };
        self.count()?;
        if self.layouts.lends(pointee.ty()) && !self.layouts.shared_view(pointee.ty()) {
            return Err(error("a reference points at a storable value, not at a value that holds a reference"));
        }
        // A reference read out of a record that holds one is passed on as the value it is.
        if !mutable && matches!(expr.kind, a::Expression::Field(..)) && self.hint(expr) == Some(expected) {
            return self.expr_expected(expr, Some(expected));
        }
        // A slice is passed as the two-word value it is (a slice local, or `all.part(1, 4)`),
        // keeping the loans it carries (docs/design-slices.md).
        if let Pointee::Slice(id) = pointee {
            // `&*s` or `&mut *s` of a slice is `s` itself, lent again (it re-entered here forever:
            // review of `borrows`, a compiler stack overflow). `&` of any other slice place is
            // not a way to make one.
            // `&array` or `&mut array` where a slice is expected is the whole array's slice,
            // `array.slice()` or `array.slice_mut()`: its length is the array's.
            let whole;
            let expr = match &expr.kind {
                a::Expression::Unary(lend @ ("&" | "&mut"), inner) => match &inner.kind {
                    a::Expression::Unary("*", slice) => slice.as_ref(),
                    _ if self.infer_hint(inner, None, 0, &mut 0).ok().flatten().and_then(|t| self.layouts.get(t)).is_some_and(|layout| matches!(layout.kind, Kind::Array(..))) => {
                        let method = if *lend == "&mut" { "slice_mut" } else { "slice" };
                        whole = a::Expr { kind: a::Expression::Call(Box::new(a::Expr { kind: a::Expression::Field(inner.clone(), method.into()), span: expr.span.clone() }), Vec::new()), span: expr.span.clone() };
                        &whole
                    }
                    _ => return Err(error("a slice is made with `.slice()` of an array, `.as_slice()` of a Vec or `.part(from, to)` of a slice, not with `&`")),
                },
                _ => expr,
            };
            let value = self.expr_expected(expr, self.layouts.is_str(id).then_some(expected))?;
            if value.ty != expected && !(value.ty == Ty::Borrow { mutable: true, pointee } && !mutable) {
                let given = match value.ty { Ty::Borrow { pointee, .. } => pointee.ty(), other => other };
                return Err(self.borrow_mismatch(given, pointee.ty(), mutable, given == pointee.ty()));
            }
            return Ok(Expr { ty: expected, kind: value.kind });
        }
        // `&v[i]` of a container: its `cell` call, lent as any call's reference is.
        let celled = self.celled(expr)?;
        let expr = celled.as_ref().unwrap_or(expr);
        // So is a reference a call makes (`tasks.cell(i)`), keeping the loans it carries until
        // the call it is given to ends. A function proved by a model borrows named values only.
        if matches!(expr.kind, a::Expression::Call(..)) && !self.modelled() {
            let value = self.expr(expr)?;
            if value.ty != expected && !(value.ty == Ty::Borrow { mutable: true, pointee } && !mutable) {
                let given = match value.ty { Ty::Borrow { pointee, .. } => pointee.ty(), other => other };
                return Err(self.borrow_mismatch(given, pointee.ty(), mutable, given == pointee.ty()));
            }
            return Ok(Expr { ty: expected, kind: value.kind });
        }
        // An argument bound ahead of an `if` or `?` in a later one (early_exit's `spill`) is read
        // as `move ?spill...`: a reference given on by its hidden name, as the name it was.
        let unspilled;
        let expr = match &expr.kind {
            a::Expression::Unary("move", inner) if matches!(&inner.kind, a::Expression::Name(n) if n.starts_with("?spill")) => { unspilled = (**inner).clone(); &unspilled }
            _ => expr,
        };
        let place = match &expr.kind {
            a::Expression::Name(_) => self.borrowed_place(expr)?,
            // A variant that holds nothing is a value, though it is written as a name.
            a::Expression::Unary("&", target) if !mutable && (!Self::place_syntax(target)
                || matches!(&target.kind, a::Expression::Name(name) if !self.scope.contains_key(name) && self.layouts.has_variant(name))) => {
                // A temporary: it gets a local of its own for the reference to point at.
                if self.modelled() { return Err(error("a function with a contract borrows named values only: bind the value with `let` first")); }
                let value = self.expect(target, pointee.ty())?;
                if !self.layouts.properties(value.ty).copy {
                    return Err(error("a borrowed temporary must be a Copy value: bind an owner with `let`, so that it can be consumed afterwards"));
                }
                let local = self.new_local(value.ty, false, None)?;
                self.addressed.insert(local.id);
                self.loan(local.id, false)?;
                let place = Place { root: local.id, identity: local.id, root_ty: value.ty, ty: value.ty, pointer: false, mutable: false, projections: Vec::new() };
                return Ok(Expr { ty: expected, kind: Value::Stored(place, Box::new(value)) });
            }
            a::Expression::Unary("&mut", target) if mutable && !Self::place_syntax(target) =>
                return Err(error("a value with no name cannot be changed through a reference: bind it with `var` first")),
            a::Expression::Unary(op @ ("&" | "&mut"), target) => {
                if (*op == "&mut") != mutable { return Err(error("borrow argument mutability mismatch")); }
                self.place(target)?
            }
            _ => return Err(error("borrow arguments require temporary references or parameter reborrows")),
        };
        // A full `Dir` lent where a `Dir<Read>` or `Dir<Write>` is wanted: the same handle, fewer rights.
        let narrowed = !mutable && crate::compiler::host::narrows(place.ty, pointee.ty());
        if let (Ty::Host(given), Ty::Host(wanted)) = (place.ty, pointee.ty()) && given != wanted && !narrowed {
            return Err(Diagnostic::new("W0401", expr.span.clone(), format!("a {} is not a {}: a capability gives up rights, never gains them", given.name(), wanted.name())));
        }
        if (place.ty != pointee.ty() && !narrowed) || (mutable && !place.mutable) {
            return Err(self.borrow_mismatch(place.ty, pointee.ty(), mutable, place.ty == pointee.ty() || narrowed));
        }
        // A record that holds a reference lives in locals; no part of it has an address.
        // A whole shared view may be lent shared: the reference keeps the view's own loans
        // (docs/design-references-to-views.md, step 1).
        let whole_view = place.projections.is_empty() && self.layouts.shared_view(place.root_ty);
        // A part that holds no reference (`&mut w.pos`) is lent like any field: the view has a
        // place in the frame, and the loan covers that field's path (adversarial review round 23).
        let plain_part = !place.projections.is_empty() && self.layouts.shared_view(place.root_ty) && !self.layouts.lends(place.ty);
        if !place.pointer && self.layouts.lends(place.root_ty) && !matches!(place.root_ty, Ty::Borrow { .. }) && !whole_view && !plain_part {
            return Err(error("a part of a value that holds a reference is read, not borrowed: bind it with `let` first"));
        }
        // Granting mutable access delegates the same write authority as a store.
        // Whole-value reborrows still reach public APIs; projections must have
        // permission from every enclosing sealed/invariant-bearing record.
        if mutable { self.unsealed_lend(&place, true, brief)?; }
        if !place.pointer { self.addressed.insert(place.root); }
        self.available(&place)?;
        if place.pointer && place.root >= self.parameters {
            // A reference held in a local lends what the local already borrows. A `&mut` local
            // is itself lent meanwhile, so it cannot be handed on twice or used beside the loan.
            // The loan is the part reached (`(*r).x` beside `(*r).y`), as through a parameter;
            // the whole local overlaps every part, so `r` itself still cannot be used meanwhile.
            if matches!(self.locals[place.root as usize].ty, Ty::Borrow { mutable: true, .. }) { let id = self.loan_id(&place); self.loan(id, mutable)?; }
            self.inherit(place.root);
        } else if whole_view {
            // `&v` of a view: a loan on its storage, kept apart from what it carries.
            let entry = (place.identity, vec![VIEW_ITSELF]);
            let index = match self.paths.iter().position(|known| *known == entry) { Some(i) => i, None => { self.paths.push(entry); self.paths.len() - 1 } };
            self.loan(FIELD_LOAN + index as u32, mutable)?;
            self.inherit(place.root);
        } else { let id = self.loan_id(&place); self.loan(id, mutable)?; }
        Ok(Expr { ty: expected, kind: Value::Address(place) })
    }
    /// A borrow argument whose evaluation runs nothing: a reference passed on, or `&`/`&mut` of a
    /// place written with names, fields, one dereference and indexes that are names or literals.
    pub(super) fn still(&self, expr: &a::Expr) -> bool {
        fn place(expr: &a::Expr) -> bool {
            match &expr.kind {
                a::Expression::Name(_) => true,
                a::Expression::Unary("*", value) => matches!(&value.kind, a::Expression::Name(_)),
                a::Expression::Field(value, _) => place(value),
                a::Expression::Index(value, index) => place(value) && matches!(&index.kind, a::Expression::Name(_) | a::Expression::Integer(_)),
                _ => false,
            }
        }
        match &expr.kind {
            a::Expression::Name(name) => self.scope.contains_key(name),
            a::Expression::Unary("&" | "&mut", target) => place(target) && !matches!(&target.kind, a::Expression::Name(name) if !self.scope.contains_key(name)),
            _ => false,
        }
    }
    pub(super) fn place_syntax(expr: &a::Expr) -> bool {
        match &expr.kind {
            a::Expression::Name(_) => true,
            a::Expression::Unary("*", value) => matches!(&value.kind, a::Expression::Name(_)),
            a::Expression::Field(value, _) | a::Expression::Index(value, _) => Self::place_syntax(value),
            _ => false,
        }
    }
    pub(super) fn borrowed_place(&self, expr: &a::Expr) -> Result<Place> {
        let local = self.borrowed(expr)?;
        let Ty::Borrow { mutable, pointee } = local.ty else { unreachable!() };
        Ok(Place { root: local.id, identity: local.id, root_ty: pointee.ty(), ty: pointee.ty(), pointer: true,
            mutable, projections: Vec::new() })
    }
    pub(super) fn place(&mut self, expr: &a::Expr) -> Result<Place> {
        self.nested(|this| { this.count()?; this.place_inner(expr) })
    }
    /// A field or element reached through a reference by its name alone, `r.field` or `r[i]`,
    /// written out as `(*r).field` or `(*r)[i]`. Everything after this sees only the long form.
    /// `&text` of a `text: &str` (or of any shared slice reference): that reference itself, as a
    /// shared reference is copied. So a `for` over an array of `&str` binds each one, and
    /// `map.get(&name)` of a `name: &str` looks it up.
    /// `&"pen"` lends the literal's `Text` (`map.get(&"pen")`, a `&F` with `F: Format`), unless a
    /// `&str` is what is expected: then it is the literal itself.
    pub(super) fn collapsed<'e>(&self, expr: &'e a::Expr, expected: Option<Ty>) -> Option<&'e a::Expr> {
        let a::Expression::Unary("&", inner) = &expr.kind else { return None };
        if self.literal(inner) { return (expected.is_some() && expected == self.layouts.known_str_borrow()).then_some(&**inner); }
        matches!(self.hint(inner), Some(Ty::Borrow { mutable: false, pointee: Pointee::Slice(_) })).then_some(&**inner)
    }
    /// `&"pen"`, lent as its `Text` (`collapsed`).
    pub(super) fn lent_literal(&self, expr: &a::Expr) -> Option<Ty> {
        let a::Expression::Unary("&", inner) = &expr.kind else { return None };
        if !self.literal(inner) { return None; }
        let text = self.layouts.named_record("Text")?;
        Some(Ty::Borrow { mutable: false, pointee: Pointee::from_ty(text)? })
    }
    /// What an array holds for a value of type `t`: a literal's `&str` is stored as its `Text`.
    /// So is one inside a tuple or anonymous record (`[("/home", 120), ...]`).
    pub(super) fn stored_text(&mut self, t: Ty) -> Ty {
        if Some(t) == self.layouts.known_str_borrow() && let Some(text) = self.layouts.named_record("Text") { return text; }
        let Some(layout) = self.layouts.get(t) else { return t };
        if layout.origin.is_some() || !matches!(layout.kind, Kind::Record(_)) || !layout.fields.iter().any(|f| Some(f.ty) == self.layouts.known_str_borrow()) { return t; }
        let fields: Vec<(String, Ty)> = layout.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
        let fields = fields.into_iter().map(|(name, ty)| (name, self.stored_text(ty))).collect();
        self.layouts.anonymous(fields).unwrap_or(t)
    }
    /// A text literal as the parser writes it, `Text(index)` (parser/expression.rs).
    pub(super) fn literal(&self, expr: &a::Expr) -> bool {
        matches!(&expr.kind, a::Expression::Call(callee, args) if matches!(&callee.kind, a::Expression::Name(name) if name == "Text") && !self.scope.contains_key("Text")
            && matches!(args.as_slice(), [a::Expr { kind: a::Expression::Integer(index), .. }] if self.layouts.literal_bytes(*index).is_some()))
    }
    /// A `Text` (or `&Text`) where a `&str` is expected: its `as_str()`.
    pub(super) fn text_as_str(&self, expr: &a::Expr, expected: Ty) -> Option<a::Expr> {
        if Some(expected) != self.layouts.known_str_borrow() || self.literal(expr) { return None; }
        let given = self.hint(expr).or_else(|| matches!(&expr.kind, a::Expression::Unary("&", _)).then_some(Ty::Unit))?;
        let at = |kind| a::Expr { kind, span: expr.span.clone() };
        let value = match given {
            // `&t` of a `t: Text`: `t` itself.
            _ if let a::Expression::Unary("&", inner) = &expr.kind && self.hint(inner).is_some_and(|t| self.layouts.nominal(t) == Some("Text")) => (**inner).clone(),
            Ty::Borrow { pointee, .. } if self.layouts.nominal(pointee.ty()) == Some("Text") => at(a::Expression::Unary("*", Box::new(expr.clone()))),
            t if self.layouts.nominal(t) == Some("Text") => expr.clone(),
            _ => return None,
        };
        Some(at(a::Expression::Call(Box::new(at(a::Expression::Field(Box::new(value), "as_str".into()))), vec![])))
    }
    pub(super) fn through(&self, expr: &a::Expr) -> Option<a::Expr> {
        fn root(scope: &BTreeMap<String, Local>, expr: &a::Expr) -> Option<a::Expr> {
            let kind = match &expr.kind {
                a::Expression::Name(name) if matches!(scope.get(name).map(|local| local.ty), Some(Ty::Borrow { .. })) =>
                    a::Expression::Unary("*", Box::new(expr.clone())),
                a::Expression::Field(base, name) => a::Expression::Field(Box::new(root(scope, base)?), name.clone()),
                a::Expression::Index(base, index) => a::Expression::Index(Box::new(root(scope, base)?), index.clone()),
                _ => return None,
            };
            Some(a::Expr { kind, span: expr.span.clone() })
        }
        if !matches!(expr.kind, a::Expression::Field(..) | a::Expression::Index(..)) { return None; }
        root(&self.scope, expr)
    }
    pub(super) fn place_inner(&mut self, expr: &a::Expr) -> Result<Place> {
        if let Some(explicit) = self.through(expr) { return self.place_inner(&explicit); }
        match &expr.kind {
            a::Expression::Name(name) => {
                let local = self.scope.get(name).ok_or_else(|| {
                    // The nearest names written in scope (not the hidden ones the compiler makes).
                    let mut near: Vec<(usize, &String)> = self.scope.keys()
                        .filter(|n| n.chars().all(|c| c.is_alphanumeric() || c == '_'))
                        .map(|n| (distance(n, name), n)).filter(|(d, _)| *d <= 2.max(name.len() / 4)).collect();
                    near.sort();
                    match near.first() {
                        Some((_, n)) => error(format!("unknown name `{name}`: no variable or parameter of this name is in scope here; the nearest is `{n}`")),
                        None => error(format!("unknown name `{name}`: no variable or parameter of this name is in scope here")),
                    }
                })?;
                if matches!(local.ty, Ty::Borrow { .. } | Ty::Unit) {
                    return Err(error("reference/Unit bindings are not addressable places; references require dereference"));
                }
                Ok(Place { root: local.id, identity: local.id, root_ty: local.ty, ty: local.ty, pointer: false,
                    mutable: local.mutable, projections: Vec::new() })
            }
            a::Expression::Unary("*", value) if matches!(value.kind, a::Expression::Field(..)) && matches!(self.hint(value), Some(Ty::Borrow { .. })) =>
                Err(error("a reference held in a record is used by name: bind it with `let` first, as in `let items = view.items`")),
            a::Expression::Unary("*", value) => self.borrowed_place(value),
            a::Expression::Field(base, name) => {
                let mut place = self.place(base)?;
                if matches!(place.ty, Ty::Borrow { .. }) { return Err(error("a reference held in a record is used by name: bind it with `let` first, as in `let items = view.items`")); }
                let layout = self.layouts.get(place.ty).ok_or_else(|| error("field place requires a record"))?;
                if !matches!(layout.kind, Kind::Record(_)) { return Err(error("field place requires a record")); }
                let (ordinal, field) = layout.fields.iter().enumerate().find(|(_,f)| f.name == *name).ok_or_else(|| unknown_field(&layout.fields, name))?;
                // Another file may read a sealed type's data, but never the authority it holds:
                // that is what makes a sealed record a narrower capability.
                if !self.sealed_reach(place.ty) && self.layouts.holds_capability(field.ty) {
                    return Err(error("a capability inside a sealed type is reached only in the type's own file"));
                }
                place.projections.push(Projection::Field { ordinal: ordinal as u32 });
                place.ty = field.ty;
                Ok(place)
            }
            a::Expression::Index(base, index) => {
                let mut place = self.place(base)?;
                // Only a fixed array's element is a place; a container's is lent by its method.
                const NOT_A_PLACE: &str = "indexed place requires an array: `v[i]` of a Vec or another container is a copy of the element, not a place; lend one with `v.at(i)` (or `v.at_mut(i)`) and match its `Ok`";
                let layout = self.layouts.get(place.ty).ok_or_else(|| error(NOT_A_PLACE))?;
                let Kind::Array(element, length) = layout.kind else { return Err(error(NOT_A_PLACE)); };
                let value = self.expect(index, Ty::U64)?;
                place.projections.push(Projection::Index { value, length });
                place.ty = element;
                Ok(place)
            }
            _ => Err(error("unsupported place; requires a local root or borrowed parameter")),
        }
    }
}
