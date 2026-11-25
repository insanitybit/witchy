use super::diagnostics::shadowed;
use super::*;

impl Checker<'_> {
    /// Consume a whole record once and give each field its own binding, so every
    /// owner inside still has to be moved on or closed.
    pub(super) fn open(&mut self, out: &mut Vec<Stmt>, value: Expr, names: Vec<(usize, String)>, span: Option<(usize, usize)>, mutable: bool) -> Result<()> {
        for (index, (_, name)) in names.iter().enumerate() {
            // `_` names nothing, so it may stand for several fields.
            if name != "_" && (self.scope.contains_key(name) || names[..index].iter().any(|(_, seen)| seen == name)) { return Err(error(shadowed(name, self.bound_at.get(name.as_str()).copied()))); }
        }
        // Each name that holds a reference needs the loans the whole value carries, as a field
        // read from it does (`inherit`): every such name takes them all again and keeps them
        // to its own last use (`settle`). A `var` could be bound again, which a reference is not.
        let lends = self.layouts.lends(value.ty);
        if lends && mutable { return Err(error("a value that holds a reference is opened with `let`, not `var`: a reference is bound once")); }
        let carried = if lends { self.carried.clone() } else { Vec::new() };
        let ty = value.ty;
        let fields = self.layouts.get(ty).unwrap().fields.clone();
        let whole = self.new_local(ty, false, span)?;
        out.push(Stmt::Set(whole.id, value));
        if !self.layouts.properties(ty).copy { self.flow.consume(whole.id)?; if self.at.start > 0 { self.moved_at.insert(whole.id, self.at.start); } }
        for (ordinal, name) in names {
            let field = &fields[ordinal];
            let place = Place { root: whole.id, identity: whole.id, root_ty: ty, ty: field.ty, pointer: false, mutable: false,
                projections: vec![Projection::Field { ordinal: ordinal as u32 }] };
            let local = self.new_local(field.ty, mutable, span)?;
            // A field under `_` is still a local in scope, under a name no source can write: an
            // owner there has to be closed, and cannot be, so it is refused as any live owner is.
            if name != "_" { self.local_names.insert(local.id, name.clone()); }
            if lends && name != "_" && self.layouts.lends(field.ty) {
                self.lend_again(&carried);
                self.holds.insert(local.id, carried.clone());
                self.opened_views.push((name.clone(), local.id));
            }
            self.scope.insert(if name == "_" { format!("_#{}", local.id) } else { name }, local);
            let kind = if self.layouts.properties(field.ty).copy { Value::ReadPlace(place) } else { Value::Extract { wrapper: whole.id, place } };
            out.push(Stmt::Set(local.id, Expr { ty: field.ty, kind }));
        }
        Ok(())
    }
}
