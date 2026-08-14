//! The interpreter's lexical environment: variable bindings + scope chain
//! (`Env`) and the assignment-kind marker (`Assign`). Verbatim move from the
//! evaluator; pure environment state.

use std::rc::Rc;

// foldhash (not SipHash): `mentioned` holds program identifiers collected from
// a closure body — compiler-internal, never attacker-controlled — matching the
// interpreter's own FxHashSet convention (see interpreter.rs).
use foldhash::HashSet as FxHashSet;

use super::Value;

/// Lexically scoped variable bindings. Functions are not closures: a call
/// starts a fresh `Env` so a function body sees only its parameters and the
/// global function table.
pub(super) enum Assign {
    Done,
    Immutable,
    Unbound,
}

#[derive(Default, Debug)]
pub struct Env {
    /// A stack of scopes; each scope is a small list of bindings carrying whether
    /// the binding is mutable (`var`/`own`) or not (`let`). Scopes are
    /// usually tiny (a couple of params/locals), so a linear scan beats a
    /// `HashMap`'s allocation and hashing on the hot call path. Lookups scan most
    /// recent first, so a later `let` shadows an earlier one.
    ///
    /// Names are `Rc<str>`: bindings are created far more often than distinct
    /// names exist (every call re-binds its params; every loop iteration
    /// re-binds its variable), so defining clones a pointer instead of copying
    /// a `String` (the interner / per-function name cache own the one real
    /// allocation per distinct name).
    scopes: Vec<Vec<(Rc<str>, Value, bool)>>,
    /// Cleared scope vecs kept for reuse: loops push/pop a scope per
    /// iteration, and recycling the allocation removes a malloc/free pair
    /// from every iteration. Capacity is not a semantic: excluded from
    /// `Clone`/`PartialEq` (manual impls below), so a cloned env (a closure
    /// capture) or an env comparison behaves exactly as before.
    spare: Vec<Vec<(Rc<str>, Value, bool)>>,
}

/// `spare` is a reuse pool, not state: clones start with an empty pool.
impl Clone for Env {
    fn clone(&self) -> Self {
        Self { scopes: self.scopes.clone(), spare: Vec::new() }
    }
}

/// `spare` is a reuse pool, not state: equality is over bindings only.
impl PartialEq for Env {
    fn eq(&self, other: &Self) -> bool {
        self.scopes == other.scopes
    }
}

impl Env {
    pub(super) fn new() -> Self {
        Self {
            scopes: vec![Vec::new()],
            spare: Vec::new(),
        }
    }
    pub(super) fn push(&mut self) {
        self.scopes.push(self.spare.pop().unwrap_or_default());
    }
    pub(super) fn pop(&mut self) {
        if let Some(mut scope) = self.scopes.pop() {
            scope.clear();
            if self.spare.len() < 16 {
                self.spare.push(scope);
            }
        }
    }
    pub(super) fn define(&mut self, name: Rc<str>, value: Value, mutable: bool) {
        self.scopes.last_mut().unwrap().push((name, value, mutable));
    }
    pub(super) fn get(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes.iter().rev() {
            for (n, v, _) in scope.iter().rev() {
                if &**n == name {
                    return Some(v);
                }
            }
        }
        None
    }
    /// Reassign an existing binding in place; rejects immutable (`let`) bindings.
    pub(super) fn assign(&mut self, name: &str, value: Value) -> Assign {
        for scope in self.scopes.iter_mut().rev() {
            for (n, slot, mutable) in scope.iter_mut().rev() {
                if &**n == name {
                    if *mutable {
                        // Taking an exclusive reference promotes an owner slot
                        // into a stable cell. Keep that cell in place so later
                        // assignments through the owner and through references
                        // observe the same storage.
                        if let Value::ReferenceCell(cell) = slot {
                            *cell.borrow_mut() = value;
                        } else {
                            *slot = value;
                        }
                        return Assign::Done;
                    }
                    return Assign::Immutable;
                }
            }
        }
        Assign::Unbound
    }

    /// A pruned snapshot for closure capture: only bindings whose names appear
    /// in the closure body (`mentioned`), innermost occurrence winning — the
    /// same resolution `get`'s reverse scan produces. Observationally identical
    /// to cloning the whole environment (a name the body never mentions can
    /// never be looked up), without the O(everything) copy per closure created
    /// or applied.
    pub(super) fn capture(&self, mentioned: &FxHashSet<String>) -> Env {
        let mut scope: Vec<(Rc<str>, Value, bool)> = Vec::new();
        for s in &self.scopes {
            for (n, v, m) in s {
                if mentioned.contains(&**n) {
                    match scope.iter_mut().find(|(en, _, _)| en == n) {
                        Some(slot) => *slot = (n.clone(), v.clone(), *m),
                        None => scope.push((n.clone(), v.clone(), *m)),
                    }
                }
            }
        }
        Env { scopes: vec![scope], spare: Vec::new() }
    }

    /// Mutable access to a binding's slot plus its mutability, innermost first
    /// (the same binding `assign` would write).
    pub(super) fn slot_mut(&mut self, name: &str) -> Option<(&mut Value, bool)> {
        for scope in self.scopes.iter_mut().rev() {
            for (n, slot, mutable) in scope.iter_mut().rev() {
                if &**n == name {
                    return Some((slot, *mutable));
                }
            }
        }
        None
    }
}
