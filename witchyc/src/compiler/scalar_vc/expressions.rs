use super::*;

impl Context<'_> {
    pub(super) fn number(&mut self, e: &Expr) -> Result<Math> {
        self.count()?;
        if let Value::Project(..) = &e.kind && let Some(part) = self.element_part(e)? { return Ok(part); }
        Ok(match &e.kind {
            Value::BoxOp(crate::compiler::operations::BoxOp::Address | crate::compiler::operations::BoxOp::WeakBits, _, args) => self.box_address(&args[0])?,
            Value::U64(n) => Math::Literal(*n),
            Value::Bool(b) => Math::Literal(u64::from(*b)),
            // The whole path, an element at a literal index included (`fields`).
            Value::ReadPlace(place) if fields(place).is_some() => self.leaf((place.root, fields(place).expect("guarded")), e.ty),
            Value::SliceLen(inner) if self.slice_length(inner).is_some() => { let key = self.slice_length(inner).expect("guarded"); self.leaf(key, e.ty) }
            // An element of an array value at a literal index within it: that element's leaf, as
            // `xs[0]` in a clause names it. A computed index is not followed.
            Value::Index(array, at, length) if matches!(at.kind, Value::U64(k) if k < *length as u64 && k < ELEMENT as u64)
                && matches!(&array.kind, Value::ReadPlace(place) if fields(place).is_some()) => {
                let (Value::ReadPlace(place), Value::U64(k)) = (&array.kind, &at.kind) else { unreachable!("guarded") };
                let mut path = fields(place).expect("guarded");
                path.push(ELEMENT | *k as u32);
                self.leaf((place.root, path), e.ty)
            }
            Value::Widen(inner) if scalar(inner.ty).is_some() => self.number(inner)?,
            Value::Binary(op @ (Binary::Add | Binary::Sub | Binary::Mul | Binary::Div | Binary::Rem), a, b) if scalar(a.ty).is_some() && scalar(b.ty).is_some() => {
                let (a, b) = (self.number(a)?, self.number(b)?);
                if self.proving && matches!(op, Binary::Add | Binary::Sub | Binary::Mul) && matches!(e.ty, Ty::Unsigned(_)) && matches!(scalar(e.ty), Some(Some(_))) {
                    self.covered.insert(e as *const Expr as usize);
                }
                self.arithmetic(*op, a, b, e.ty)?
            }
            Value::Binary(..) | Value::Not(_) if e.ty == Ty::Bool => { let truth = self.truth(e)?; self.reify(truth) }
            // An I64 literal is the integer its bits spell.
            Value::Signed(Signed::FromBits, operands) if matches!(operands[0].kind, Value::U64(_)) => { let Value::U64(bits) = operands[0].kind else { unreachable!() }; Math::Int(bits as i64) }
            Value::Signed(op @ (Signed::Add | Signed::Sub | Signed::Mul | Signed::Div | Signed::Rem), operands) => {
                let (a, b) = (self.number(&operands[0])?, self.number(&operands[1])?);
                self.signed(*op, a, b)?
            }
            Value::Signed(Signed::Eq | Signed::Ne | Signed::Lt | Signed::Le | Signed::Gt | Signed::Ge, _) => { let truth = self.truth(e)?; self.reify(truth) }
            // Between an I64 and its bits nothing is followed.
            // `I64.bits(x)` is x modulo 2^64; `I64.from_bits(n)` the integer whose bits n is.
            Value::Signed(op @ (Signed::FromBits | Signed::Bits), operands) => {
                let inner = self.number(&operands[0])?;
                let value = Math::Cast(*op == Signed::FromBits, Box::new(inner));
                self.bind(value)
            }
            Value::Call(call) => match self.call(call)?.0 { Some(value) => value, None => self.fresh(e.ty) },
            // A bit operation's result is an unknown of its type; `a & b` is at most each side and
            // `a >> n` at most `a`. A shift traps from the type's width on: an obligation here, as
            // an overflow is, and a fact after.
            Value::Bits(op, operands) => {
                let values = operands.iter().map(|operand| self.number(operand)).collect::<Result<Vec<_>>>()?;
                let result = self.fresh(e.ty);
                match op {
                    Bits::And => for value in &values { self.hypotheses.push(Prop::Compare("≤", result.clone(), value.clone())); },
                    Bits::Shl | Bits::Shr => {
                        let Some(Some(maximum)) = scalar(e.ty) else { return Ok(result) };
                        let check = Prop::Compare("<", values[1].clone(), Math::Literal(u64::from(64 - maximum.leading_zeros())));
                        if self.proving { self.body("a shift by as many bits as the value has, or more", check.clone())?; }
                        self.hypotheses.push(check);
                        if *op == Bits::Shr { self.hypotheses.push(Prop::Compare("≤", result.clone(), values[0].clone())); }
                    }
                    // At most the width; and a nonzero value below 2^k has its lowest set bit below
                    // bit k, so fewer than k zeros under it (the masks of 8, 16 and 32 lanes).
                    Bits::TrailingZeros => {
                        self.hypotheses.push(Prop::Compare("≤", result.clone(), Math::Literal(64)));
                        for bits in [8u64, 16, 32, 64] {
                            let nonzero = Prop::Compare("<", Math::Literal(0), values[0].clone());
                            let fits = if bits == 64 { nonzero } else { Prop::Binary("∧", Box::new(nonzero), Box::new(Prop::Compare("<", values[0].clone(), Math::Literal(1 << bits)))) };
                            self.hypotheses.push(Prop::Binary("→", Box::new(fits), Box::new(Prop::Compare("<", result.clone(), Math::Literal(bits)))));
                        }
                    }
                    // One bit for each of the sixteen byte lanes.
                    Bits::Packed(packed) if packed.op == crate::compiler::operations::LaneOp::Mask && packed.bits == 8 =>
                        self.hypotheses.push(Prop::Compare("<", result.clone(), Math::Literal(65536))),
                    Bits::CountOnes => self.hypotheses.push(Prop::Compare("≤", result.clone(), Math::Literal(64))),
                    Bits::Or | Bits::Xor | Bits::Not | Bits::Packed(_) | Bits::WrapAdd | Bits::WrapSub | Bits::WrapMul | Bits::MulHigh => {}
                }
                result
            }
            _ => { self.walk(e)?; self.fresh(e.ty) }
        })
    }
    /// A Bool expression as a proposition. `&&` and `||` evaluate their right side conditionally.
    pub(super) fn truth(&mut self, e: &Expr) -> Result<Prop> {
        self.count()?;
        Ok(match &e.kind {
            Value::Bool(b) => Prop::Bool(*b),
            Value::Not(inner) => not(&self.truth(inner)?),
            Value::Binary(op @ (Binary::And | Binary::Or), a, b) => {
                let left = self.truth(a)?;
                let (guard, symbol) = if matches!(op, Binary::And) { (left.clone(), "∧") } else { (not(&left), "∨") };
                let right = self.guarded(&guard, |context| context.truth(b))?;
                Prop::Binary(symbol, Box::new(left), Box::new(right))
            }
            Value::Binary(op @ (Binary::Eq | Binary::Ne | Binary::Lt | Binary::Le | Binary::Gt | Binary::Ge), a, b) => {
                let symbol = match op { Binary::Eq => "=", Binary::Ne => "≠", Binary::Lt => "<", Binary::Le => "≤", Binary::Gt => ">", _ => "≥" };
                if a.ty == Ty::Bool && b.ty == Ty::Bool && matches!(op, Binary::Eq | Binary::Ne) {
                    let same = Prop::Binary("↔", Box::new(self.truth(a)?), Box::new(self.truth(b)?));
                    if matches!(op, Binary::Ne) { not(&same) } else { same }
                } else if matches!((scalar(a.ty), scalar(b.ty)), (Some(Some(_)), Some(Some(_)))) {
                    Prop::Compare(symbol, self.number(a)?, self.number(b)?)
                } else { self.effects(a)?; self.effects(b)?; Prop::Truth(self.fresh(Ty::Bool)) }
            }
            Value::Signed(op @ (Signed::Eq | Signed::Ne | Signed::Lt | Signed::Le | Signed::Gt | Signed::Ge), operands) => {
                let symbol = match op { Signed::Eq => "=", Signed::Ne => "≠", Signed::Lt => "<", Signed::Le => "≤", Signed::Gt => ">", _ => "≥" };
                Prop::Compare(symbol, self.number(&operands[0])?, self.number(&operands[1])?)
            }
            _ => Prop::Truth(self.number(e)?),
        })
    }
    /// An expression evaluated for what it does: scalar parts exactly, the rest for their calls and `&mut` loans.
    pub(super) fn effects(&mut self, e: &Expr) -> Result<()> {
        if tracked(e.ty) { self.number(e)?; Ok(()) } else { self.walk(e) }
    }
    pub(super) fn place(&mut self, place: &crate::compiler::logical_place::Place) -> Result<()> {
        for projection in &place.projections {
            if let crate::compiler::logical_place::Projection::Index { value, length } = projection { self.index(value, *length)?; }
        }
        Ok(())
    }
    /// An array index, evaluated; and when a proof is being made and the facts already bound it
    /// below the length, the goal that it is, so the run-time check may go (`proved_index`). An
    /// index the facts do not bound is left to its check: no goal is added that might not prove.
    pub(super) fn index(&mut self, value: &Expr, length: u32) -> Result<()> {
        if !(self.proving && tracked(value.ty)) { return self.effects(value); }
        let at = self.number(value)?;
        if self.upper(&at).is_some_and(|most| most < length as u64) {
            self.body("an array index that may be past the end", Prop::Compare("<", at, Math::Literal(length as u64)))?;
            self.covered.insert(value as *const Expr as usize | INDEX_NODE);
        }
        Ok(())
    }
    /// The key of a slice local's length: a slice read from a local (or a field of one) that is
    /// not lent with `&mut`, whose length cannot change until the local is assigned.
    pub(super) fn slice_length(&self, slice: &Expr) -> Option<Key> {
        let (Value::ReadPlace(place) | Value::Move { place, .. }) = &slice.kind else { return None };
        if self.lent.contains(&place.root) { return None; }
        Some((place.root, join(&fields(place)?, &[SLICE_LENGTH])))
    }
    /// `s.part_proved(from, to)`: the goals that `from <= to` and `to <= s.len()`. Where the slice
    /// is not one whose length is followed, nothing is asked and the part keeps its range test.
    fn slice_within(&mut self, slice: &Expr, from: &Expr, to: &Expr) -> Result<()> {
        self.effects(slice)?;
        if !self.proving { self.effects(from)?; return self.effects(to); }
        let (first, last) = (self.number(from)?, self.number(to)?);
        let Some(key) = self.slice_length(slice) else { return Ok(()) };
        let length = self.leaf(key, Ty::Unsigned(crate::compiler::scalar::Scalar::U64));
        self.body("a part of a slice that may start after it ends", Prop::Compare("≤", first, last.clone()))?;
        self.body("a part of a slice that may end past the slice", Prop::Compare("≤", last, length))?;
        self.covered.insert(from as *const Expr as usize | SLICE_NODE);
        Ok(())
    }
    /// `s.read_le64(i)`: the goal that the eight bytes from `i` are inside the slice. The word
    /// read is not followed. Where the slice's length is not followed nothing is asked, and the
    /// read keeps its test.
    fn slice_word(&mut self, slice: &Expr, at: &Expr) -> Result<()> {
        if !self.proving { return self.effects(at); }
        let index = self.number(at)?;
        let Some(key) = self.slice_length(slice) else { return Ok(()) };
        let length = self.leaf(key, Ty::Unsigned(crate::compiler::scalar::Scalar::U64));
        self.body("eight bytes read that may run past the end of a slice", Prop::Compare("≤", Math::Add(Box::new(index), Box::new(Math::Literal(8))), length))?;
        self.covered.insert(at as *const Expr as usize | SLICE_NODE);
        Ok(())
    }
    /// `s[i]`: when the path already says `i < s.len()` in so many words, the goal that it is, so
    /// the check may go (`SLICE_PROVED`). Otherwise nothing is asked, and the read keeps its check:
    /// an obligation that could not be shown would refuse the whole function.
    fn slice_index(&mut self, slice: &Expr, at: &Expr) -> Result<()> {
        if !(self.proving && tracked(at.ty)) { return self.effects(at); }
        let index = self.number(at)?;
        let Some(key) = self.slice_length(slice) else { return Ok(()) };
        let length = self.leaf(key, Ty::Unsigned(crate::compiler::scalar::Scalar::U64));
        let below = |p: &Prop| match p {
            Prop::Compare("<", a, b) => *a == index && *b == length,
            Prop::Compare(">", a, b) => *a == length && *b == index,
            Prop::Not(inner) => matches!(&**inner, Prop::Compare("≤", a, b) if *a == length && *b == index) || matches!(&**inner, Prop::Compare("≥", a, b) if *a == index && *b == length),
            _ => false,
        };
        // A `nopanic` function is refused if the read keeps its check, so nothing is lost by
        // asking: what an invariant over elements says of an offset is then enough.
        if self.hypotheses.iter().any(below) || self.midpoint_below(&index, &length) || self.chained_below(&index, &length) || self.function.nopanic.is_some() {
            self.body("a slice index that may be past the end", Prop::Compare("<", index, length))?;
            self.covered.insert(at as *const Expr as usize | SLICE_NODE);
        }
        Ok(())
    }
    /// `index < X` and `X <= length` (or `X < length`), each a fact or a conjunct of one, the names
    /// followed through their defining equations (`i = for#N.count`, `for#N.end = n`): one linear
    /// step, which `omega` shows (`for i in 0..n` under `requires n <= s.len()`).
    fn chained_below(&self, index: &Math, length: &Math) -> bool {
        fn conjuncts<'p>(p: &'p Prop, out: &mut Vec<&'p Prop>) { if let Prop::Binary("∧", l, r) = p { conjuncts(l, out); conjuncts(r, out); } else { out.push(p); } }
        let mut facts = Vec::new();
        for fact in &self.hypotheses { conjuncts(fact, &mut facts); }
        // Everything a fact says equals `m`, a few steps out.
        let equal = |m: &Math| -> Vec<Math> {
            let mut class = vec![m.clone()];
            let mut at = 0;
            while at < class.len() && class.len() < 8 {
                let here = class[at].clone();
                for fact in &facts {
                    if let Prop::Compare("=", x, y) = fact {
                        let other = if *x == here { Some(y) } else if *y == here { Some(x) } else { None };
                        if let Some(other) = other && !class.contains(other) { class.push(other.clone()); }
                    }
                }
                at += 1;
            }
            class
        };
        let lows = equal(index);
        let lengths = equal(length);
        // What is at most the length: the length itself and anything a fact puts at or below it.
        let mut caps = lengths.clone();
        for fact in &facts {
            match fact {
                Prop::Compare("≤" | "<", x, y) | Prop::Compare("≥" | ">", y, x) if lengths.contains(y) => caps.extend(equal(x)),
                _ => {}
            }
        }
        facts.iter().any(|fact| matches!(fact, Prop::Compare("<", x, y) | Prop::Compare(">", y, x) if lows.contains(x) && caps.contains(y)))
    }
    /// A binary search's midpoint, `lo + (hi - lo) / 2` or `(lo + hi) / 2` (written, or the
    /// value a name was given), where facts say `lo < hi` and `hi <= length` (or `hi < length`):
    /// below `length` by linear arithmetic with a division by 2, which `omega` shows. Only this
    /// shape is recognised, so the goal it asks is one the prover does not fail on.
    fn midpoint_below(&self, index: &Math, length: &Math) -> bool {
        fn holds(facts: &[Prop], f: &dyn Fn(&Prop) -> bool) -> bool {
            fn any(p: &Prop, f: &dyn Fn(&Prop) -> bool) -> bool { f(p) || matches!(p, Prop::Binary("∧", l, r) if any(l, f) || any(r, f)) }
            facts.iter().any(|p| any(p, f))
        }
        let less = |a: &Math, b: &Math| holds(&self.hypotheses, &|p| matches!(p, Prop::Compare("<", x, y) if x == a && y == b) || matches!(p, Prop::Compare(">", y, x) if x == a && y == b));
        let at_most = |a: &Math, b: &Math| less(a, b) || holds(&self.hypotheses, &|p| matches!(p, Prop::Compare("≤", x, y) if x == a && y == b) || matches!(p, Prop::Compare("≥", y, x) if x == a && y == b));
        let shape = |m: &Math| -> Option<(Math, Math)> {
            match m {
                // lo + (hi - lo) / 2
                Math::Add(lo, half) => match &**half {
                    Math::Div(span, two) if matches!(**two, Math::Literal(2)) => match &**span {
                        Math::Sub(hi, lo2) if lo2 == lo => Some(((**lo).clone(), (**hi).clone())),
                        _ => None,
                    },
                    _ => None,
                },
                // (lo + hi) / 2
                Math::Div(sum, two) if matches!(**two, Math::Literal(2)) => match &**sum {
                    Math::Add(lo, hi) => Some(((**lo).clone(), (**hi).clone())),
                    _ => None,
                },
                _ => None,
            }
        };
        let mut candidates = vec![index.clone()];
        // The value a name was given (`let mid = ...`), as its defining equation states.
        for fact in &self.hypotheses {
            if let Prop::Compare("=", x, value) = fact && x == index { candidates.push(value.clone()); }
            if let Prop::Compare("=", value, x) = fact && x == index { candidates.push(value.clone()); }
        }
        candidates.iter().filter_map(|m| shape(m)).any(|(lo, hi)| less(&lo, &hi) && at_most(&hi, length))
    }
    /// The outcome of the call whose Result `e` hands on: the call itself, or a whole local
    /// that holds its result unchanged (the outcome waits in `pending` until the local is
    /// changed, lent or matched).
    pub(super) fn handed_on(&mut self, e: &Expr) -> Result<Option<Outcome>> {
        if self.types.sum(e.ty).is_none() { return Ok(None); }
        match &e.kind {
            Value::BoxOp(op @ (crate::compiler::operations::BoxOp::Ref | crate::compiler::operations::BoxOp::Mut | crate::compiler::operations::BoxOp::Take), _, args) => Ok(Some(self.box_variant(*op, &args[0])?)),
            Value::BoxOp(crate::compiler::operations::BoxOp::New, _, args) => Ok(Some(self.new_box(args)?)),
            Value::Call(call) => Ok(Some(self.call(call)?.1)),
            Value::Move { place, .. } | Value::ReadPlace(place) if place.projections.is_empty() && !place.pointer => Ok(self.pending.remove(&place.root)),
            _ => Ok(None),
        }
    }
    pub(super) fn walk(&mut self, e: &Expr) -> Result<()> {
        self.count()?;
        match &e.kind {
            Value::StaticStr(_) | Value::Unit | Value::U64(_) | Value::Bool(_) | Value::EntryConsole | Value::EntryInput | Value::EntryAllocator | Value::TaskWord | Value::InTask | Value::EntryHost(..) => {}
            // A `&mut` reference that is kept, not handed straight to one operation: a store through it
            // may come at any later point.
            Value::ReadPlace(place) | Value::Move { place, .. } if matches!(e.ty, Ty::Borrow { mutable: true, .. }) => { self.place(place)?; self.lend_under(place.root, Vec::new()); }
            Value::ReadPlace(place) | Value::Move { place, .. } | Value::Extract { place, .. } => self.place(place)?,
            Value::Address(place) => { self.place(place)?; if matches!(e.ty, Ty::Borrow { mutable: true, .. }) { self.lend_under(place.root, prefix(place)); } }
            Value::Stored(place, inner) => { self.effects(inner)?; self.place(place)?; self.forget(place.root, &prefix(place)); }
            Value::BoxOp(crate::compiler::operations::BoxOp::Enclosing, _, args) => {
                self.enclosing(args)?;
                self.covered.insert(e as *const Expr as usize | BOX_NODE);
            }
            Value::BoxOp(crate::compiler::operations::BoxOp::FollowLoan | crate::compiler::operations::BoxOp::FollowLoanMut, _, args) => {
                if !self.proving { return Err(error("following a loan requires its own scalar proof")); }
                let (root, path) = self.loan_base(&args[0], 0)
                    .ok_or_else(|| error("weak access requires an initialized element loan with a known allocation origin"))?;
                if !fields_known(&args[1]) { return Err(error("weak access requires a named pointer or fixed field")); }
                let base = self.leaf((root, join(&path, &[BOX_ADDRESS])), Ty::Unsigned(crate::compiler::scalar::Scalar::U32));
                let identity = self.leaf((root, join(&path, &[BOX_ID])), Ty::U64);
                let (address, target) = self.pointer_parts(&args[1])?;
                self.effects(&args[0])?;
                self.body("weak address must designate the borrowed element", equal(address, base))?;
                self.body("weak allocation must be the borrowed incarnation", equal(target, identity))?;
                self.covered.insert(e as *const Expr as usize | BOX_NODE);
            }
            Value::BoxOp(op, _, args) if op.proved() => {
                if !self.proving { return Err(error("a box refinement requires its own scalar proof")); }
                let owner = &args[0];
                if !matches!(owner.kind, Value::U64(0)) && !box_place(owner).is_some_and(|p| fields(p).is_some()) {
                    return Err(error("a proved box operation takes a named owner or a fixed field"));
                }
                let (word, incarnation) = self.pointer_parts(owner)?;
                if matches!(op, crate::compiler::operations::BoxOp::Follow | crate::compiler::operations::BoxOp::FollowMut) {
                    let weak = &args[1];
                    if !box_place(weak).is_some_and(|p| fields(p).is_some()) { return Err(error("following a weak pointer requires a named pointer or fixed field")); }
                    let (pointer, target) = self.pointer_parts(weak)?;
                    self.body("following a weak pointer of another allocation", equal(target, incarnation))?;
                    self.body("following a weak pointer of another address", equal(pointer, word.clone()))?;
                }
                let empty = *op == crate::compiler::operations::BoxOp::EndNull;
                self.body(if empty { "ending a box that may own a value" } else { "lending a box that may be null" },
                    Prop::Compare(if empty { "=" } else { ">" }, word, Math::Literal(0)))?;
                self.covered.insert(e as *const Expr as usize | BOX_NODE);
                if matches!(op, crate::compiler::operations::BoxOp::LendMut | crate::compiler::operations::BoxOp::FollowMut) {
                    self.forget_box_contents(owner);
                }
            }
            Value::BoxOp(crate::compiler::operations::BoxOp::Ref | crate::compiler::operations::BoxOp::Mut, _, args) => {
                // These lend the separately allocated payload. The reference can
                // never write the holder's address word, even through a mutable loan.
                self.box_address(&args[0])?;
                if matches!(e.kind, Value::BoxOp(crate::compiler::operations::BoxOp::Mut, _, _)) { self.forget_box_contents(&args[0]); }
            }
            Value::TryAlloc { allocator, capacity, .. } => { self.lend(allocator)?; self.effects(capacity)?; }
            Value::AllocationKey(inner, _) | Value::Capacity(inner, _) | Value::TryRelease(inner, _) => self.lend(inner)?,
            Value::Discard(inner) | Value::ProofUtf8(inner) | Value::Widen(inner) | Value::Not(inner)
            | Value::Project(inner, _) | Value::Retag(inner, _) | Value::Repeat(inner, _) => self.effects(inner)?,
            Value::Storage { receiver, index, value, .. } => {
                self.lend(receiver)?; self.effects(index)?;
                if let Some(value) = value { self.effects(value)?; }
            }
            Value::Aggregate(fields) | Value::Signed(_, fields) | Value::Float(_, fields) | Value::Bits(_, fields) | Value::BoxOp(_, _, fields) => for field in fields { self.effects(field)?; },
            Value::Sum { payload, .. } => self.effects(payload)?,
            Value::Index(a, b, length) => { self.effects(a)?; self.index(b, *length)?; }
            Value::Binary(_, a, b) | Value::SliceAt(a, b, _) => { self.effects(a)?; self.effects(b)?; }
            // The write goes through a unique slice, whose making already lent what it changes
            // (`SliceOf` below; a `&mut [T]` parameter's elements are never facts here), as a host
            // operation filling a slice it was given.
            Value::SliceReadLe64(slice, at) => { self.effects(slice)?; self.slice_word(slice, at)?; }
            Value::SliceCopy(a, b) => { self.effects(a)?; self.effects(b)?; }
            Value::SliceIndex(slice, at, true) => { self.effects(slice)?; self.slice_index(slice, at)?; }
            // Below the length by construction: no goal.
            Value::SliceIndex(slice, at, false) => { self.effects(slice)?; self.effects(at)?; }
            // A mutable slice of an array lends it as `&mut` does: stores through the slice, here
            // or in a callee, change its elements (review: a write through `slice_mut` was unseen).
            Value::SliceOf(place, _) => { self.place(place)?; if matches!(e.ty, Ty::Borrow { mutable: true, .. }) { self.lend_under(place.root, prefix(place)); } }
            Value::SliceLen(inner) => self.effects(inner)?,
            Value::SlicePart(a, b, c) => { self.effects(a)?; self.effects(b)?; self.effects(c)?; }
            Value::SliceWithin(slice, from, to) => self.slice_within(slice, from, to)?,
            Value::Call(call) => { self.call(call)?; }
            Value::Print(place, value, _) => { self.place(place)?; self.effects(value)?; }
            Value::Read(place, _) => self.place(place)?,
            Value::Host(_, place, operands) => { if let Some(place) = place { self.place(place)?; } for operand in operands { self.effects(operand)?; } }
            // A pause is invisible to the function that pauses (nothing outside a task refers
            // into it), and a step changes only the task's own block: like a host operation,
            // only what the operands do. A function with a contract has none (check.rs).
            Value::TaskSpawn { allocator, bytes, entry } => { self.effects(allocator)?; self.effects(bytes)?; if let Value::Call(call) = &entry.kind { for arg in &call.args { self.effects(arg)?; } } }
            Value::TaskStep(value, word) => { self.effects(value)?; self.effects(word)?; }
            Value::TaskFinish(value) | Value::TaskStatus(value) | Value::Pause(value) => self.effects(value)?,
            // The two values change places, and what is known of their scalar parts goes with them.
            Value::Exchange(left, right) => {
                self.place(left)?;
                self.place(right)?;
                let followed = !self.lent.contains(&left.root) && !self.lent.contains(&right.root);
                match (fields(left), fields(right)) {
                    (Some(a), Some(b)) if followed => {
                        let mut moved = Vec::new();
                        for (from, source, to, target) in [(left.root, &a, right.root, &b), (right.root, &b, left.root, &a)] {
                            for ((local, path), value) in &self.state {
                                if *local == from && path.starts_with(source) { moved.push(((to, join(target, &path[source.len()..])), value.clone())); }
                            }
                        }
                        self.forget(left.root, &a);
                        self.forget(right.root, &b);
                        self.state.extend(moved);
                    }
                    _ => { self.forget(left.root, &prefix(left)); self.forget(right.root, &prefix(right)); }
                }
            }
        }
        Ok(())
    }
    /// What is known of the value at `place`, by the path of each part within it: what goes with
    /// the value when it is moved whole (as `gather` takes it for a `let`). Nothing for a place
    /// this walk does not follow.
    pub(super) fn known_under(&self, place: &Place) -> Vec<(Vec<u32>, Math)> {
        let Some(source) = fields(place) else { return Vec::new() };
        if self.lent.contains(&place.root) { return Vec::new(); }
        self.state.iter().filter(|((local, fields), _)| *local == place.root && fields.starts_with(&source))
            .map(|((_, fields), value)| (fields[source.len()..].to_vec(), value.clone()))
            .filter(|(rest, _)| rest.first() != Some(&MEMBER_STAMP)).collect()
    }
    /// A reference handed to one operation. What it may change is unknown afterwards; the loan ends with the operation.
    pub(super) fn lend(&mut self, e: &Expr) -> Result<()> {
        match &e.kind {
            Value::Address(place) | Value::ReadPlace(place) | Value::Move { place, .. } if matches!(e.ty, Ty::Borrow { .. }) => {
                self.place(place)?;
                if matches!(e.ty, Ty::Borrow { mutable: true, .. }) { self.forget(place.root, &prefix(place)); }
                Ok(())
            }
            _ => self.effects(e),
        }
    }
}
