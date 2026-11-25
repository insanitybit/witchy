use super::diagnostics::moved_named;
use super::*;

impl Checker<'_> {
    pub(super) fn discardable(&self, ty: Ty) -> Result<()> {
        let p = self.layouts.properties(ty);
        if !p.copy && !p.implicit_discard { return Err(error("a value that holds an owner is dropped here: bind it and hand it on, or close or discard it")); }
        Ok(())
    }
    pub(super) fn available(&self, place: &Place) -> Result<()> {
        let checked = if !place.pointer && !self.layouts.properties(place.root_ty).copy {
            self.flow.require_owned(place.identity)
        } else { self.flow.require(place.identity) };
        // Name the value (Flow knows slots, not names).
        checked.map_err(|d| match self.scope.iter().find(|(_, local)| local.id == place.root) {
            Some((name, _)) => moved_named(d, name, self.moved_at.get(&place.root).copied()),
            None => d,
        })
    }
    pub(super) fn expire_locals(&mut self, locals: &[Local]) -> Result<Vec<u32>> {
        let mut expired = Vec::new();
        for local in locals {
            let p = self.layouts.properties(local.ty);
            if p.copy || matches!(local.ty, Ty::Borrow { .. }) { continue; }
            let expiring = self.flow.expire(local.id, p.implicit_discard).map_err(|e| {
                // Say which value is left holding an owner, and where it was made.
                let span = local.span.map_or(self.at.clone(), |(start, end)| start..end);
                // A name that has already left scope (a match arm's binding) is named by where it was bound.
                let what = match self.scope.iter().find(|(_, l)| l.id == local.id).map(|(n, _)| n.clone()) {
                    Some(name) if !name.contains('#') => format!("`{name}`"),
                    Some(_) => "a value with no name of its own".to_string(),
                    None if local.span.is_some() => "the value bound here".to_string(),
                    None => return e,
                };
                // How this type is ended, when it has a method for it: `text.close()`.
                let how = self.layouts.nominal(local.ty).map(|owner| owner.to_string()).and_then(|owner| {
                    let shown = owner.split('@').next().unwrap_or(&owner).to_string();
                    // `close` empties a container of plain values; one of owners is discarded.
                    let ways: &[&str] = if self.layouts.holds_owning_buffer(local.ty) { &["discard"] } else { &["close", "discard", "release"] };
                    ways.iter().copied().find(|m| self.signatures.get(format!("{owner}.{m}").as_str())
                        .is_some_and(|sig| sig.public || sig.module == self.module)).map(|m| (shown, m))
                });
                let call = match (&how, what.strip_prefix('`').and_then(|w| w.strip_suffix('`'))) {
                    (Some((_, "discard")), Some(name)) => format!(" (`{name}.discard()`, with `import discard`)"),
                    (Some((_, m)), Some(name)) => format!(" (`{name}.{m}()`)"),
                    (Some((ty, m)), None) => format!(" (a `{ty}` with `.{m}()`)"),
                    _ => String::new(),
                };
                Diagnostic::new("W0401", span, format!("{what} still holds an owner when it goes out of scope: hand it on, or close or discard it first{call}"))
            })?;
            if expiring { expired.push(local.id); }
        }
        Ok(expired)
    }
    pub(super) fn expire_visible(&mut self) -> Result<Vec<u32>> {
        let locals = self.scope.values().copied().collect::<Vec<_>>();
        self.expire_locals(&locals)
    }
    /// The local and field path a loan id stands for; a plain local is its whole value.
    pub(super) fn decode(&self, id: u32) -> (u32, &[u32]) {
        let id = id & !UNIQUE;
        if id >= FIELD_LOAN && let Some((local, path)) = self.paths.get((id - FIELD_LOAN) as usize) { (*local, path.as_slice()) } else { (id, &[]) }
    }
    /// Whether two loans may reach the same memory: one local, and one path begins the other.
    pub(super) fn overlaps(&self, a: u32, b: u32) -> bool {
        let ((la, pa), (lb, pb)) = (self.decode(a), self.decode(b));
        la == lb && (pa.starts_with(pb) || pb.starts_with(pa))
    }
    /// What borrowing `place` lends: the field path through an owned local, or through a
    /// parameter that is a reference, when there is one (`&p.left` beside `&mut p.right`),
    /// else the place's identity. (A reference held in a local lends what it borrows instead:
    /// `borrow`.)
    pub(super) fn loan_id(&mut self, place: &Place) -> u32 {
        if place.identity != place.root { return place.identity; }
        // A literal index is a step of the path too (`cells[0]` and `cells[1]` are different
        // places); any other index ends it, so it overlaps every element.
        const INDEX: u32 = 1 << 31;
        let path: Vec<u32> = place.projections.iter().map_while(|p| match p {
            Projection::Field { ordinal } => Some(*ordinal),
            // The two highest steps are the markers THROUGH and VIEW_ITSELF: a literal index that
            // would encode as one overlaps every element instead (review: no array is that long).
            Projection::Index { value, .. } => match value.kind { Value::U64(n) if n < (INDEX - 2) as u64 => Some(INDEX | n as u32), _ => None },
        }).collect();
        if path.is_empty() { return place.identity; }
        let entry = (place.identity, path);
        let index = match self.paths.iter().position(|known| *known == entry) { Some(i) => i, None => { self.paths.push(entry); self.paths.len() - 1 } };
        FIELD_LOAN + index as u32
    }
    /// The loans now held that reach the same memory as `root`, as (shared, unique) totals.
    pub(super) fn related(&self, root: u32) -> (usize, usize) {
        self.loans.iter().filter(|(id, _)| self.overlaps(**id, root)).fold((0, 0), |(s, u), (_, (a, b))| (s + a, u + b))
    }
    pub(super) fn loan(&mut self, root: u32, mutable: bool) -> Result<()> {
        let loan = self.related(root);
        if loan.1 != 0 || (mutable && loan.0 != 0) {
            return Err(error(if self.kept(root) { "this value is lent: a reference into it is still in use" } else { "overlapping unique call-argument loans" }));
        }
        let loan = self.loans.entry(root).or_default();
        if mutable { loan.1 += 1; } else { loan.0 += 1; }
        Ok(())
    }
}
