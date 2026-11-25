//! Abstract Init semantics and bounded proposition rendering, independent of layout.
use super::{logical_body::*, storage_contract::{error,Number,Predicate,StorageContract,View}, proof::{Goal,SourceIdentity}};
use crate::diagnostic::Diagnostic;
type Result<T> = std::result::Result<T,Diagnostic>;
pub(super) const MODEL:&str="witchy.checked-storage-init.v1";
/// Shared abstract primitive term. Callers supply bounded, already typed symbols.
pub(super) fn transition(cells:&str,index:&str,payload:&str)->String {
    transition_at(cells,index,payload,"cap")
}
pub(super) fn transition_at(cells:&str,index:&str,payload:&str,cap:&str)->String {
    init_option_at(cells,index,&format!("some {payload}"),cap)
}
/// The option must represent a nonempty source element on the successful path.
pub(super) fn init_option_at(cells:&str,index:&str,payload:&str,cap:&str)->String {
    format!("if {index} < {cap} then (match {cells} {index} with | none => (none, fun j => if j = {index} then {payload} else {cells} j) | some _ => (some 2, {cells})) else (some 1, {cells})")
}
struct Render { text:String, nodes:usize }
impl Render {
    fn push(&mut self,s:&str)->Result<()> {
        if self.text.len()+s.len()>super::MAX_SOURCE_BYTES{return Err(error("storage contracts generated statement budget exceeded"));}
        self.text.push_str(s);Ok(())
    }
    fn count(&mut self)->Result<()> {self.nodes+=1;if self.nodes>4096{return Err(error("storage contracts VC complexity limit"));}Ok(())}
    fn number(&mut self,n:&Number)->Result<()> {
        self.count()?;self.push(&match n {Number::Literal(n)=>format!("({n} : Nat)"),Number::Parameter(id)=>format!("p{id}"),Number::Code=>"code".into(),Number::Capacity(View::Before|View::After)=>"cap".into()})
    }
    fn cells(view:View)->&'static str {match view{View::Before=>"cells",View::After=>"after"}}
    fn predicate(&mut self,p:&Predicate)->Result<()> {
        self.count()?;
        match p {
            Predicate::Bool(b)=>self.push(if *b{"True"}else{"False"}),
            Predicate::Compare(op,a,b)=>{self.push("(")?;self.number(a)?;self.push(op)?;self.number(b)?;self.push(")")},
            Predicate::Not(p)=>{self.push("(¬ ")?;self.predicate(p)?;self.push(")")},
            Predicate::Binary(op,a,b)=>{self.push("(")?;self.predicate(a)?;self.push(op)?;self.predicate(b)?;self.push(")")},
            Predicate::Empty(v,i)=>{self.push("(")?;self.push(Self::cells(*v))?;self.push(" ")?;self.number(i)?;self.push(" = none)")},
            Predicate::Cell(v,i,item)=>{self.push("(")?;self.push(Self::cells(*v))?;self.push(" ")?;self.number(i)?;self.push(&format!(" = some p{item})"))},
            Predicate::Same(View::Before|View::After,View::Before|View::After)=>self.push("(key = key)"),
            Predicate::Unchanged(a,b)=>{self.push("(key = key ∧ cap = cap ∧ ∀ j : Nat, ")?;self.push(Self::cells(*a))?;self.push(" j = ")?;self.push(Self::cells(*b))?;self.push(" j)")},
            Predicate::Except(a,b,i)=>{self.push("(key = key ∧ cap = cap ∧ ∀ j : Nat, j ≠ ")?;self.number(i)?;self.push(" → ")?;self.push(Self::cells(*a))?;self.push(" j = ")?;self.push(Self::cells(*b))?;self.push(" j)")},
        }
    }
    fn post(&mut self,c:&StorageContract,arm:u8)->Result<()> {
        self.push("(True")?;
        for (selected,p) in &c.clauses {if selected.is_none()||*selected==Some(arm){self.push(" ∧ ")?;self.predicate(p)?;}}
        self.push(")")
    }
}
/// What a local names before the return: a parameter, or (for an index) a literal.
#[derive(Clone,Copy)]
enum Alias{Parameter(u32),Literal(u64)}
/// A whole local read, or a parameter through the locals that alias it (`let i = index`).
fn alias(e:&Expr,f:&Function,aliases:&std::collections::BTreeMap<u32,Alias>)->Option<Alias> {
    match &e.kind {
        Value::U64(n)=>Some(Alias::Literal(*n)),
        Value::ReadPlace(p)|Value::Move{place:p,..} if !p.pointer&&p.projections.is_empty()=>
            if (p.root as usize)<f.params.len(){Some(Alias::Parameter(p.root))}else{aliases.get(&p.root).copied()},
        _=>None,
    }
}
fn parameter(e:&Expr,f:&Function,ty:Ty,aliases:&std::collections::BTreeMap<u32,Alias>)->Result<u32> {
    if let Some(Alias::Parameter(root))=alias(e,f,aliases) && f.params[root as usize]==ty {return Ok(root);}
    Err(error("storage contracts actual operands require parameters of the expected type"))
}
pub(super) fn body(f:&Function,c:&StorageContract)->Result<Goal> {
    // `let` bindings of a parameter, another such local or a literal may come first (Astra
    // audit: `let pos = index` before the return was refused before Lean saw it). Each is
    // followed to what it names, so the proof is the one of the direct form.
    let mut aliases=std::collections::BTreeMap::new();
    let mut statements=f.body.iter().filter(|s|!matches!(s,Stmt::Expire(_)));
    let mut next=statements.next();
    while let Some(Stmt::Set(local,value))=next {
        // A parameter written here would no longer be what the proof calls it.
        if (*local as usize)<f.params.len(){return Err(error("storage contracts allow only `let` of a parameter or a number before the return of memory.init"));}
        let named=alias(value,f,&aliases).ok_or_else(||error("storage contracts allow only `let` of a parameter or a number before the return of memory.init"))?;
        aliases.insert(*local,named);
        next=statements.next();
    }
    let Some(Stmt::Return(e, _, _))=next else{return Err(error("storage contracts require a direct return of memory.init"));};
    if statements.any(|s|!matches!(s,Stmt::Expire(_))){return Err(error("storage contracts require only the direct Init return"));}
    let Value::Storage{op:StorageOp::Init,receiver,index,value:Some(payload),..}=&e.kind else{return Err(error("storage contracts require a direct return of memory.init"));};
    if !matches!(&receiver.kind,Value::Address(p) if p.root==c.buffer&&p.pointer&&p.projections.is_empty()) {
        return Err(error("storage contracts Init must use the exact buffer parameter"));
    }
    let actual_index=match alias(index,f,&aliases) {
        Some(Alias::Literal(n))=>Number::Literal(n),
        _=>Number::Parameter(parameter(index,f,Ty::U64,&aliases)?)};
    let item=parameter(payload,f,Ty::Param{owner:f.definition,index:0},&aliases)?;
    let mut r=Render{text:String::new(),nodes:c.nodes};
    r.push("∀ (α Key : Type) (key : Key) (cap : Nat) (cells : Nat → Option α) ")?;
    let mut introductions="α Key key cap cells".to_string();
    for (id,ty) in f.params.iter().enumerate() {if id as u32!=c.buffer {
        r.push(&format!("(p{id} : {}) ",if *ty==Ty::U64{"Nat"}else{"α"}))?;
        introductions.push_str(&format!(" p{id}"));
    }}
    r.push(&format!(", cap ≤ {} → (∀ j : Nat, cap ≤ j → cells j = none) → ",u64::MAX))?;
    introductions.push_str(" hcap hout");
    for (id,ty) in f.params.iter().enumerate(){if *ty==Ty::U64 {
        r.push(&format!("p{id} ≤ {} → ",u64::MAX))?;introductions.push_str(&format!(" hp{id}"));
    }}
    // The returned variant and after-cells come from ONE actual primitive term.
    // Key and capacity are preserved by Init.
    let mut index_render=Render{text:String::new(),nodes:0};index_render.number(&actual_index)?;
    r.push("let transition : Option Nat × (Nat → Option α) := ")?;
    for _ in 0..3{r.count()?;}
    r.push(&transition("cells",&index_render.text,&format!("p{item}")))?;
    r.push("; let after := transition.2; match transition.1 with | none => ")?;
    r.post(c,0)?;r.push(" | some code => ")?;r.post(c,1)?;
    let proof=format!("  intros {introductions}\n  by_cases hbound : {} < cap\n  · cases hempty : cells {} with\n    | none => simp_all <;> witchy_inline_omega\n    | some occupied => simp_all <;> witchy_inline_omega\n  · simp only [if_neg hbound]\n    simp_all <;> witchy_inline_omega\n",index_render.text,index_render.text);
    Ok(Goal{source_name:f.name.clone(),statement:r.text,nodes:r.nodes,source:Some(SourceIdentity{kind:"function_body",function:f.name.clone(),callee:None,site:None,storage:true,record:false,memory:false}),prover:Some(proof)})
}
