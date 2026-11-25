//! Interpret a shared projection from its checked body. A recursive call must
//! descend through an owned, initialized storage cell. Each return describes a
//! place relative to the receiver under the discriminants encountered en route.
use super::*;

pub(super) const FOLD: u32 = 2 * VIEW_METHOD;
pub(super) fn folded(step: u32) -> bool { (FOLD..FOLD + VIEW_METHOD).contains(&step) }

pub(super) struct Case {
    pub tags: Vec<(Vec<u32>, usize, usize)>,
    pub selected: Vec<u32>,
}

fn applies(f: &Function) -> bool {
    matches!(f.params.as_slice(), [Ty::Borrow { mutable: false, .. }])
        && matches!(f.result, Ty::Borrow { mutable: false, .. })
        && f.contract.as_ref().is_none_or(|c| c.requires.is_empty() && c.storage.is_none()
            && c.record.as_ref().is_none_or(|r| r.requires.is_empty())
            && c.memory.as_ref().is_none_or(|m| m.requires.is_empty()))
}

fn location(e: &Expr, env: &BTreeMap<u32, Vec<u32>>) -> Option<Vec<u32>> {
    let p = box_place(e)?;
    if !p.projections.iter().all(|v| matches!(v, Projection::Field { .. })) { return None; }
    Some(join(env.get(&p.root)?, &prefix(p)))
}

/// A single, unconditional projection (including a proved read of slot zero).
pub(super) fn direct(f: &Function) -> Option<Vec<u32>> {
    if !applies(f) { return None; }
    let [Stmt::Return(e, _, _)] = f.body.as_slice() else { return None };
    let env = BTreeMap::from([(0, Vec::new())]);
    match &e.kind {
        Value::Storage { op: StorageOp::RefUnchecked, receiver, index, .. } if matches!(index.kind, Value::U64(0)) => {
            let mut out = location(receiver, &env)?;
            out.push(BOX_VALUE);
            Some(out)
        }
        _ => location(e, &env),
    }
}

struct View<'a> {
    functions: &'a [Function],
    definition: &'a Function,
    cases: Vec<Case>,
    left: usize,
}
impl View<'_> {
    fn expression(&self, e: &Expr, env: &BTreeMap<u32, Vec<u32>>) -> Option<Vec<u32>> {
        if let Some(path) = location(e, env) { return Some(path); }
        let Value::Call(call) = &e.kind else { return None };
        let [arg] = call.args.as_slice() else { return None };
        let mut path = location(arg, env)?;
        if call.definition == self.definition.definition {
            // The structural measure is the size of the owned footprint.
            // Every stored value is loan-free and the recursive receiver has
            // the same instantiated type. Sibling fields cannot supply a cycle.
            if !path.contains(&BOX_VALUE) || arg.ty != self.definition.params[0]
                || call.result != self.definition.result { return None; }
            path.push(FOLD + call.definition.0);
        } else {
            path.extend(direct(self.functions.get(call.definition.0 as usize)?)?);
        }
        Some(path)
    }

    fn block(&mut self, statements: &[Stmt], mut env: BTreeMap<u32, Vec<u32>>, tags: &[(Vec<u32>, usize, usize)], depth: usize) -> Option<()> {
        if depth > 32 { return None; }
        for (i, stmt) in statements.iter().enumerate() {
            self.left = self.left.checked_sub(1)?;
            match stmt {
                Stmt::Set(id, e) if matches!(e.ty, Ty::Borrow { mutable: false, .. }) => {
                    let path = self.expression(e, &env)?;
                    env.insert(*id, path);
                }
                Stmt::Expire(_) => {}
                Stmt::Return(e, _, _) if i + 1 == statements.len() => {
                    self.cases.push(Case { tags: tags.to_vec(), selected: self.expression(e, &env)? });
                    return Some(());
                }
                Stmt::Match { value, arms } if i + 1 == statements.len() && !arms.is_empty()
                    && matches!(value.ty, Ty::Borrow { mutable: false, .. }) => {
                    let sum = location(value, &env)?;
                    for (index, arm) in arms.iter().enumerate() {
                        let mut arm_env = env.clone();
                        if let Some(id) = arm.binding {
                            arm_env.insert(id, join(&sum, &[ARM + index as u32]));
                        }
                        let mut guards = tags.to_vec();
                        guards.push((sum.clone(), index, arms.len()));
                        self.block(&arm.body, arm_env, &guards, depth + 1)?;
                    }
                    return Some(());
                }
                _ => return None,
            }
        }
        None
    }
}

pub(super) fn cases(functions: &[Function], id: u32) -> Option<Vec<Case>> {
    let f = functions.get(id as usize)?;
    if !applies(f) { return None; }
    let mut view = View { definition: f, functions, cases: Vec::new(), left: 256 };
    view.block(&f.body, BTreeMap::from([(0, Vec::new())]), &[], 0)?;
    Some(view.cases)
}
