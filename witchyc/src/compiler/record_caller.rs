//! Actual caller prefixes; one guarded obligation per primary typed record site.
use super::{logical_body::*,record_contract::{Predicate,Term},record_model::{Model,Check},record_state::State,proof::{Goal,SourceIdentity},storage_contract::error};
use crate::diagnostic::Diagnostic;
use std::collections::{BTreeMap,BTreeSet};
type Result<T> = std::result::Result<T,Diagnostic>;
pub(super) const MODEL:&str="witchy.checked-storage-record-caller-allocation.v1";
#[derive(Clone)]
pub(super) struct Site {pub callee:FunctionId,pub parts:Vec<Check>,pub reached:bool,pub dependencies:BTreeSet<u32>,pub order:Option<usize>}
pub(super) struct Caller<'a>{pub functions:&'a[Function],pub sites:BTreeMap<u32,Site>,pub summaries:&'a BTreeMap<u32,super::record_post::OwnedPost>}
fn number(t:&Term)->bool{match t{Term::Literal(_)|Term::Capacity(_)=>true,Term::Path(p)=>p.ty==Ty::U64,Term::Bound=>false,Term::Add(a,b)=>number(a)&&number(b)}}
pub(super) fn supported(p:&Predicate)->Result<()>{let okay=match p{
    Predicate::Bool(_)=>true,Predicate::Compare(_,a,b)=>number(a)&&number(b),Predicate::Not(p)=>return supported(p),
    Predicate::Binary(_,a,b)=>{supported(a)?;return supported(b);},
    Predicate::Prefix(_,n,c)=>number(n)&&number(c),Predicate::Empty(_,i)=>number(i),_=>false,
};if okay{Ok(())}else{Err(error("record caller requires payload-insensitive Prefix/Empty/U64 predicates; unsupported content or history clause"))}}
pub(super) fn prepare(program:&Program,summaries:&BTreeMap<u32,super::record_post::OwnedPost>,bodies:&[Goal])->Result<super::proof_body::Prepared>{
    let mut goals=vec![];let mut evidence=vec![];
    for f in &program.functions{
        let sites=super::contract_sites::record_sites(&f.body,&program.functions);if sites.is_empty(){continue;}
        // No heap state or callee posts are assumed here. Universally checked
        // empty-requires bodies need only a complete typed-site inventory, even
        // when ordinary callers contain loops or effects outside this interpreter.
        if sites.values().all(|id|program.functions[id.0 as usize].contract.as_ref().expect("record contract").record.as_ref().expect("record").requires.is_empty()){
            for(site,callee)in sites{
                goals.push(Goal{source_name:f.name.clone(),statement:"True".into(),nodes:1,source:Some(SourceIdentity{kind:"call_precondition",function:f.name.clone(),callee:Some(program.functions[callee.0 as usize].name.clone()),site:Some(site),storage:false,record:true,memory:false}),prover:None});
            }continue;
        }
        // A body proved over owned records shows each callee's requirement inside its own
        // proof, against the actual arguments; the site needs no separate obligation.
        if f.contract.as_ref().is_some_and(|c|c.record.is_some()){
            for(site,callee)in sites{
                goals.push(Goal{source_name:f.name.clone(),statement:"True".into(),nodes:1,source:Some(SourceIdentity{kind:"call_precondition",function:f.name.clone(),callee:Some(program.functions[callee.0 as usize].name.clone()),site:Some(site),storage:false,record:true,memory:false}),prover:None});
            }continue;
        }
        if f.contract.is_some(){return Err(error("record caller closure inside another contracted body is unsupported"));}
        for callee in sites.values(){for p in &program.functions[callee.0 as usize].contract.as_ref().expect("record contract").record.as_ref().expect("record").requires{supported(p)?;}}
        let caller=Caller{functions:&program.functions,summaries,sites:sites.into_iter().map(|(site,callee)|(site,Site{callee,parts:vec![],reached:false,dependencies:BTreeSet::new(),order:None})).collect()};
        let mut m=Model::new(&program.types,None,Some(caller),0);let mut state=State{locals:BTreeMap::new(),foci:BTreeMap::new(),path:vec![],dependencies:BTreeSet::new()};m.enter(f,&mut state)?;
        super::record_state::run(&mut m,&f.body,vec![state])?;
        let sites=std::mem::take(&mut m.caller.as_mut().expect("caller").sites);let mut bytes=0usize;
        let first=goals.len();
        for(site,entry)in &sites{
            evidence.push(super::proof_body::SiteEvidence::interpreted(f,*site,&program.functions[entry.callee.0 as usize],entry.reached));
            let mut parts=entry.parts.clone();
            // The typed inventory includes sites whose literal control path never runs.
            if !entry.reached{parts.push(Check{path:vec![("False".into(),true)],text:"False".into(),index:false});}
            if parts.is_empty(){parts.push(Check{path:vec![],text:"True".into(),index:false});}
            m.take_checks(parts);let mut goal=m.finish(f,SourceIdentity{kind:"call_precondition",function:f.name.clone(),callee:Some(program.functions[entry.callee.0 as usize].name.clone()),site:Some(*site),storage:false,record:true,memory:false})?;
            if !entry.dependencies.is_empty(){goal.prover.as_mut().expect("record prover").insert_str(0,&format!("{}\n",super::record_post::MARKER));}
            bytes=bytes.saturating_add(goal.statement.len());if bytes>super::MAX_SOURCE_BYTES{return Err(error("record caller total statement budget exceeded"));}goals.push(goal);
        }
        let batch=bodies.iter().chain(&goals[first..]).collect::<Vec<_>>();
        super::record_post::dependencies(f,&program.functions,&sites,summaries,&batch)?;
    }Ok(super::proof_body::Prepared{goals,sites:evidence,proved_arithmetic:Default::default()})
}
