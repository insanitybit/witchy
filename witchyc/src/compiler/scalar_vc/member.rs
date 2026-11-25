//! Invert a finite path of owned, initialized projections. Each row of the
//! kernel statement describes one occurrence of the focus in that path. Column
//! numbers denote resolved receiver-relative field/arm/cell projections.
use super::*;
use super::claims::Claims;

pub(super) fn stamps(path: &[u32]) -> Result<(Vec<u32>, Vec<u32>)> {
    member_path(path, true).zip(member_path(path, false)).ok_or_else(|| error("expected a recursive initialized member focus"))
}

#[derive(Default)]
struct Columns(Vec<Vec<u32>>);

impl Columns {
    fn index(&mut self, path: &[u32]) -> usize {
        if let Some(at) = self.0.iter().position(|p| p == path) { return at; }
        self.0.push(path.to_vec());
        self.0.len() - 1
    }

    fn fact(&mut self, p: &Prop, claims: &Claims) -> Result<String> {
        let mut term = |m: &Math| -> Result<String> {
            match m {
                Math::Literal(n) => Ok(n.to_string()),
                Math::Binding(i) => {
                    let leaf = &claims.leaves[*i as usize];
                    if leaf.parameter != Some(0) || leaf.fields.iter().any(|s| view::folded(*s)
                        || matches!(*s, MEMBER_STAMP | ROOT_STAMP)) { return Err(error("the local focus lemma must describe only its own fixed projections")); }
                    Ok(format!("(s i {})", self.index(&leaf.fields)))
                }
                _ => Err(error("the local focus lemma requires field comparisons")),
            }
        };
        Ok(match p {
            Prop::Bool(b) => if *b { "True" } else { "False" }.into(),
            Prop::Compare(op, a, b) => format!("({} {op} {})", term(a)?, term(b)?),
            Prop::Not(p) => format!("(¬ {})", self.fact(p, claims)?),
            Prop::Binary(op, a, b) => format!("({} {op} {})", self.fact(a, claims)?, self.fact(b, claims)?),
            Prop::Truth(m) => format!("({} = 1)", term(m)?),
        })
    }
}

impl Context<'_> {
    pub(super) fn enclosing(&mut self, args: &[Expr]) -> Result<()> {
        if !self.proving { return Err(error("lending an enclosing allocation requires a checked proof")); }
        let [walk, member, edge, focus] = args else { return Err(error("invalid enclosing-loan operands")); };
        let (root, walk) = self.shared_path(walk, 0).ok_or_else(|| error("expected a pure structural root projection"))?;
        let (resource, tag) = stamps(&walk)?;
        let at = walk.iter().position(|s| view::folded(*s)).expect("stamps checked");
        let family = walk[at];
        let suffix = &walk[at+1..];

        let Value::Call(call) = &focus.kind else { return Err(error("the focus must call its verified shared projection method")); };
        let [arg] = call.args.as_slice() else { return Err(error("the enclosing focus takes only its receiver")); };
        let (base, path) = self.shared_path(arg, 0).ok_or_else(|| error("the enclosing focus requires a shared receiver"))?;
        if base != root || path != walk[..at] { return Err(error("the walk and focus must begin at the same owning place")); }
        let f = &self.functions[call.definition.0 as usize];
        let cases = view::cases(self.functions, f.definition.0).ok_or_else(|| error("the local focus must be a pure shared projection"))?;
        let [here] = cases.as_slice() else { return Err(error("the local focus must return one fixed initialized cell")); };
        let Some((last, buffer)) = here.selected.split_last() else { return Err(error("the focus must lend an initialized cell")); };
        if *last != BOX_VALUE || !here.tags.is_empty() || buffer.iter().any(|k| view::folded(*k)) { return Err(error("the local focus must project a fixed slot-zero loan")); }
        let source = box_place(member).filter(|p| p.projections.is_empty() && self.function.locals[p.root as usize].ty == member.ty)
            .ok_or_else(|| error("the source member must be a named shared loan"))?;
        let weak = box_place(edge).filter(|p| p.pointer && p.root == source.root && !p.projections.is_empty()
            && p.projections.iter().all(|s| matches!(s, Projection::Field { .. }))).ok_or_else(|| error("the operand must load a fixed weak field from the actual member"))?;
        let edge_field = join(&here.selected, &prefix(weak));
        let member_stamp = self.raw_leaf((source.root, tag), Ty::U64);
        let root_stamp = self.raw_leaf((root, resource), Ty::U64);
        self.body("the input loan belongs to this recursive footprint", equal(member_stamp, root_stamp))?;
        let (address, _) = self.pointer_parts(edge)?;
        self.body("an enclosing loan needs a nonnull stored link", Prop::Compare("≠", address, Math::Literal(0)))?;
        let outer = self.leaf((root, join(&walk[..at], &join(&edge_field, &[BOX_ADDRESS]))), Ty::Unsigned(super::super::scalar::Scalar::U32));
        self.body("the outer focus ends at a null link", equal(outer, Math::Literal(0)))?;

        let contract = scalar_contract(f).ok_or_else(|| error("the local focus needs a verified source contract"))?;
        let claims = Claims::scalar(self.functions, f, contract)?;
        if !claims.requires.is_empty() { return Err(error("the local focus lemma must apply to each closed owner")); }
        let mut cols = Columns::default();
        let (link_addr, link_id) = (cols.index(&join(&edge_field, &[BOX_ADDRESS])), cols.index(&join(&edge_field, &[BOX_ID])));
        let (own_addr, own_id) = (cols.index(&join(buffer, &[BOX_ADDRESS])), cols.index(&join(buffer, &[BOX_ID])));
        let local = claims.ensures.iter().map(|(_, p)| cols.fact(p, &claims)).collect::<Result<Vec<_>>>()?;
        let mut steps = Vec::new();
        let mut leaf = false;
        let cases = view::cases(self.functions, family - view::FOLD).ok_or_else(|| error("unresolved recursive ownership walk"))?;
        for case in cases {
            if case.selected.last() != Some(&family) {
                if join(&case.selected, suffix) != here.selected { return Err(error("every terminal arm must select the declared initialized focus")); }
                leaf = true;
                continue;
            }
            let descent = &case.selected[..case.selected.len()-1];
            if !descent.contains(&BOX_VALUE) || descent.iter().any(|s| view::folded(*s)) { return Err(error("membership descends only through strict owned cell projections")); }
            let mut step = case.tags.iter().map(|(sum, i, _)| format!("s i {} = {i}", cols.index(&join(sum, &[SUM_TAG])))).collect::<Vec<_>>();
            for (col, part) in [(link_addr, BOX_ADDRESS), (link_id, BOX_ID)] {
                let child = cols.index(&join(descent, &join(&edge_field, &[part])));
                step.push(format!("s (i+1) {col} = s i {child}"));
            }
            steps.push(format!("({})", step.join(" ∧ ")));
        }
        if !leaf || steps.is_empty() || local.is_empty() { return Err(error("expected a finite recursive focus and a local preservation lemma")); }
        // Every member has a finite ownership path from this borrowed root.
        // A row at i+1 is the actual child of row i under one typed match arm.
        // The checked method's postcondition applies to each such closed owner.
        // The two boundary premises were separately required of the actual
        // emitted operands above; the theorem supplies both identity and loan.
        // Include the exact typed paths used by each column in the audited
        // request. Only compiler-generated integer ordinals enter the comment.
        let schema = cols.0.iter().enumerate().map(|(i, path)| format!("{i}={path:?}")).collect::<Vec<_>>().join(" ");
        let statement = format!("/- projection columns {schema} -/ ∀ (n : Nat) (s : Nat → Nat → Nat), (∀ i, i ≤ n → {}) → (∀ i, i < n → {}) → s 0 {link_addr} = 0 → s n {link_addr} ≠ 0 → ∃ j, j < n ∧ s n {link_addr} = s j {own_addr} ∧ s n {link_id} = s j {own_id}", local.join(" ∧ "), steps.join(" ∨ "));
        let prover = "  intro n s closed steps outer live\n  cases n with\n  | zero => exact False.elim (live outer)\n  | succ j =>\n    refine ⟨j, Nat.lt_succ_self _, ?_⟩\n    have h := closed j (Nat.le_succ _)\n    have along := steps j (Nat.lt_succ_self _)\n    constructor <;> witchy_inline_omega\n";
        self.member_goals.push(Goal { source_name: self.function.name.clone(), statement, nodes: self.nodes,
            source: Some(SourceIdentity { kind: "enclosing_member", function: self.function.name.clone(), callee: call.contract_site.map(|_| f.name.clone()), site: call.contract_site, storage: false, record: false, memory: false }),
            prover: Some(prover.into()) });
        Ok(())
    }
}
