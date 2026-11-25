//! Enumerate actual typed call occurrences, including unused/control contexts.
use super::{logical_body::*, logical_place::{Place,Projection}, proof::{Goal,SourceIdentity}};
use crate::diagnostic::Diagnostic;
struct Visit<'a> { functions:&'a [Function], caller:&'a Function, goals:Vec<Goal> }
impl Visit<'_> {
    fn place(&mut self,p:&Place) {for projection in &p.projections {if let Projection::Index{value,..}=projection{self.value(value);}}}
    fn value(&mut self,e:&Expr) {
        match &e.kind {
            Value::Call(call)=>{
                for arg in &call.args{self.value(arg);}
                let callee=&self.functions[call.definition.0 as usize];
                if callee.contract.as_ref().is_some_and(|c|c.storage.is_some()) {
                    let site=call.contract_site.expect("primary contracted call site");
                    self.goals.push(Goal{source_name:self.caller.name.clone(),statement:"True".into(),nodes:1,
                        source:Some(SourceIdentity{kind:"call_precondition",function:self.caller.name.clone(),callee:Some(callee.name.clone()),site:Some(site),storage:true,record:false,memory:false}),prover:None});
                }
            },
            Value::TryAlloc{allocator,capacity,..}=>{self.value(allocator);self.value(capacity);},
            Value::Storage{receiver,index,value,..}=>{self.value(receiver);self.value(index);if let Some(v)=value{self.value(v);}},
            Value::ReadPlace(p)|Value::Address(p)|Value::Move{place:p,..}|Value::Extract{place:p,..}=>self.place(p),
            Value::Stored(p,v)=>{self.value(v);self.place(p)},
            Value::Print(p,v,_)=>{self.place(p);self.value(v);},Value::Read(p,_)=>self.place(p),Value::Host(_,p,vs)=>{if let Some(p)=p{self.place(p);}for v in vs{self.value(v);}},Value::Exchange(a,b)=>{self.place(a);self.place(b);},
            Value::TaskSpawn{allocator,bytes,entry}=>{self.value(allocator);self.value(bytes);self.value(entry);},
            Value::TaskStep(v,w)=>{self.value(v);self.value(w);},
            Value::TaskFinish(v)|Value::TaskStatus(v)|Value::Pause(v)=>self.value(v),
            Value::Binary(_,a,b)|Value::Index(a,b,_)|Value::SliceAt(a,b,_)|Value::SliceIndex(a,b,_)|Value::SliceCopy(a,b)|Value::SliceReadLe64(a,b)=>{self.value(a);self.value(b);},
            Value::SliceOf(p,_)=>self.place(p),Value::SliceLen(v)=>self.value(v),
            Value::SlicePart(a,b,c)|Value::SliceWithin(a,b,c)=>{self.value(a);self.value(b);self.value(c);},
            Value::AllocationKey(v,_)|Value::Capacity(v,_)|Value::TryRelease(v,_)|Value::Discard(v)|Value::Sum{payload:v,..}|Value::Repeat(v,_)|Value::Retag(v,_)|Value::Project(v,_)|Value::Widen(v)|Value::ProofUtf8(v)|Value::Not(v)=>self.value(v),
            Value::Aggregate(fields)|Value::Signed(_,fields)|Value::Float(_,fields) | Value::Bits(_, fields) | Value::BoxOp(_, _, fields)=>for f in fields{self.value(f);},
            Value::StaticStr(_) | Value::Unit|Value::U64(_)|Value::Bool(_)|Value::EntryConsole|Value::EntryInput|Value::EntryAllocator|Value::TaskWord|Value::InTask|Value::EntryHost(..)=>{},
        }
    }
    fn body(&mut self,body:&[Stmt]) {
        for s in body {match s {
            Stmt::Set(_,v)|Stmt::Eval(v)|Stmt::Return(v, _, _)=>self.value(v),
            Stmt::StorePlace(p,_,v)|Stmt::Replace(_,p,v)=>{self.place(p);self.value(v);},
            Stmt::If(c,a,b)=>{self.value(c);self.body(a);self.body(b);},
            Stmt::While(c,b,_)=>{self.value(c);self.body(b);},
            Stmt::Match{value,arms}=>{self.value(value);for a in arms{self.body(&a.body);}},
            Stmt::Expire(_)|Stmt::Break|Stmt::Trap(_)|Stmt::Continue=>{},
        }}
    }
}
pub(super) fn prepare(program:&Program)->Result<Vec<Goal>,Diagnostic> {
    if !program.functions.iter().any(|f|f.contract.as_ref().is_some_and(|c|c.storage.is_some())){return Ok(vec![]);}
    let mut goals=vec![];
    for f in &program.functions {
        if let Some(c)=f.contract.as_ref().and_then(|c|c.storage.as_ref()){goals.push(super::storage_model::body(f,c).map_err(|e|super::proof_advice::at_function(e,f))?);}
        let mut v=Visit{functions:&program.functions,caller:f,goals:vec![]};v.body(&f.body);goals.extend(v.goals);
    }
    Ok(goals)
}

#[cfg(test)]
mod tests {
    use super::*;
    const INIT:&str="fn initialize<T>(buffer: &mut Buffer<T>, index: U64, item: T) -> Result<(), U64>:\n    ensures match result:\n        Ok:\n            memory.same_allocation(buffer, old(buffer))\n            memory.capacity(buffer) == memory.capacity(old(buffer))\n            index < memory.capacity(old(buffer))\n            memory.empty(old(buffer), index)\n            memory.cell(buffer, index, item)\n            memory.unchanged_except(buffer, old(buffer), index)\n        Err(code):\n            (code == 1 && index >= memory.capacity(old(buffer))) || (code == 2 && index < memory.capacity(old(buffer)) && !memory.empty(old(buffer), index))\n            memory.unchanged(buffer, old(buffer))\n    proof:\n        auto\n    return memory.init(buffer, index, item)\n";
    fn prepared(source:&str)->Result<Vec<Goal>,Diagnostic> {
        let syntax=crate::compiler::parse(source)?;
        let logical=super::super::check::program(&syntax)?;
        let goals=super::super::source_vc::prepare(&logical)?;
        super::super::concrete::program(logical)?;
        Ok(goals)
    }
    #[test]
    fn unused_generic_checked_init_has_one_alpha_generic_actual_transition() {
        let source=format!("{INIT}fn main():\n    return\n");
        let goals=prepared(&source).unwrap();assert_eq!(goals.len(),1);
        assert!(goals[0].statement.contains("α Key : Type"));
        assert!(goals[0].statement.contains("Option Nat × (Nat → Option α)"));
        assert!(goals[0].statement.contains("then some g1 else cells j"));
        assert!(goals[0].source.as_ref().unwrap().record);
        let changed=prepared(&source.replace("return memory.init(buffer, index, item)","return memory.init(buffer, 0, item)")).unwrap();
        assert_ne!(goals[0].statement,changed[0].statement);assert!(changed[0].statement.contains("if (0 : Nat) < cap"));
        let original=source.replace("index: U64, item: T)","index: U64, item: T, other: T)");
        let wrong=original.replace("return memory.init(buffer, index, item)","return memory.init(buffer, index, other)");
        let a=prepared(&original).unwrap();let b=prepared(&wrong).unwrap();
        assert_ne!(a[0].statement,b[0].statement);
        assert!(b[0].statement.contains("then some g2 else cells j"));
        assert!(b[0].statement.contains(" = some g1)")); // authored item unchanged
    }
    #[test]
    fn actual_nested_and_control_call_occurrences_have_distinct_sites() {
        let source=format!("{INIT}fn sink(x: Result<(), U64>):\n    return\nfn caller<T>(buffer: &mut Buffer<T>, index: U64, item: T):\n    if true:\n        let _ = initialize(buffer, index, item)\n    while false:\n        let _ = initialize(buffer, index, item)\n    sink(initialize(buffer, index, item))\n    match initialize(buffer, index, item):\n        Ok:\n            let _ = initialize(buffer, index, item)\n        Err(reason):\n            let _ = initialize(buffer, index, item)\nfn main():\n    return\n");
        let goals=prepared(&source).unwrap();assert_eq!(goals.len(),7);
        for (site,goal) in goals[1..].iter().enumerate() {
            let id=goal.source.as_ref().unwrap();assert_eq!(id.function,"caller");assert_eq!(id.callee.as_deref(),Some("initialize"));
            assert_eq!(id.site,Some(site as u32));assert_eq!(goal.statement,"True");assert!(goal.prover.is_none());
        }
    }
    #[test]
    fn snapshot_scope_body_and_actual_leaf_budget_fail_before_proof() {
        for source in [
            INIT.replace("Err(code):","Err(index):"),
            INIT.replace("Err(code):","Err(result):"),
            INIT.replace("memory.empty(old(buffer), index)","memory.empty(old(old(buffer)), index)"),
            INIT.replace("memory.empty(old(buffer), index)","code == 1"),
        ] {
            let failure=prepared(&format!("{source}fn main():\n    return\n")).err().unwrap();
            assert_eq!(failure.code,"W0401");assert!(failure.message.contains("record contracts"),"{}",failure.message);
        }
        for (count,accepted) in [(31,true),(33,false)] {
            let leaves="            true\n".repeat(count);
            let source=format!("fn initialize<T>(buffer: &mut Buffer<T>, index: U64, item: T) -> Result<(), U64>:\n    ensures match result:\n        Ok:\n{leaves}        Err(code):\n            true\n    proof:\n        auto\n    return memory.init(buffer, index, item)\nfn main():\n    return\n");
            let result=prepared(&source);
            if accepted{assert!(result.is_ok());}else{assert!(result.err().unwrap().message.starts_with("record contracts allow at most 32 combined predicate clauses"));}
        }
    }
}
