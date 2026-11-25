//! Bounded interpretation of the PRIMARY typed body. Source Flow is not replayed.
use super::{logical_body::*,logical_place::{Place,Projection},record_value::Symbol,record_model::Model,record_focus::Focus,storage_contract::error};
use crate::diagnostic::Diagnostic;
use std::collections::{BTreeMap,BTreeSet};
type Result<T> = std::result::Result<T,Diagnostic>;
#[derive(Clone)]
pub(super) struct State {pub locals:BTreeMap<u32,Symbol>,pub foci:BTreeMap<u32,Focus>,pub path:Vec<(String,bool)>,pub dependencies:BTreeSet<u32>}
fn ordinals(p:&Place)->Result<Vec<usize>>{if p.pointer{return Err(error("record contracts do not support runtime reference places"));}p.projections.iter().map(|p|match p{Projection::Field{ordinal}=>Ok(*ordinal as usize),_=>Err(error("record contracts do not support indexed places"))}).collect()}
fn exclusive_place(m: &Model<'_>, place: &Place) -> bool {
    place.pointer && place.mutable
        && m.contract.and_then(|contract| contract.mutable_owner) == Some(place.root)
}
fn modeled_ordinals(m: &Model<'_>, place: &Place) -> Result<Vec<usize>> {
    if exclusive_place(m, place) {
        let mut local = place.clone();
        local.pointer = false;
        ordinals(&local)
    } else {
        ordinals(place)
    }
}
pub(super) fn read(s:&State,p:&Place)->Result<Symbol>{if p.pointer&&p.projections.is_empty()&&matches!(s.locals.get(&p.root),Some(Symbol::Permission)){return Ok(Symbol::Permission);}s.locals.get(&p.root).and_then(|v|v.field(&ordinals(p).ok()?)).cloned().ok_or_else(||error("record contracts missing logical place"))}
fn scalar(v:&Symbol)->Result<&str>{if matches!(v,Symbol::Nat(_)|Symbol::Prop(_)){if let Some(text)=v.text(){return Ok(text);}}Err(error("record contracts scalar content is not modeled for opaque generic elements"))}
fn fork(m:&mut Model<'_>,s:&State,guard:String,positive:bool)->Result<State>{m.count()?;let mut s=s.clone();s.path.push((guard,positive));Ok(s)}
fn branch(m:&mut Model<'_>,s:&State,guard:&str,positive:bool)->Result<Option<State>> {
    if guard=="True"||guard=="False" {return Ok(((guard=="True")==positive).then(||s.clone()));}
    Ok(Some(fork(m,s,guard.into(),positive)?))
}
fn values(m:&mut Model<'_>,state:State,es:&[Expr],needed_after:bool)->Result<Vec<(State,Vec<Symbol>)>>{
    let mut work=vec![(state,vec![])];for(i,e)in es.iter().enumerate(){let after=needed_after||es[i+1..].iter().any(|e|m.needs_value(e));let mut next=vec![];for(s,vs)in work{for(s,v)in value(m,s,e,after)?{m.count()?;let mut vs=vs.clone();vs.push(v);next.push((s,vs));}}work=next;}Ok(work)
}
fn value(m:&mut Model<'_>,s:State,e:&Expr,needed_after:bool)->Result<Vec<(State,Symbol)>> {
    m.count()?;
    match &e.kind {
        // A literal's static bytes: nothing the record model reads, and nothing it changes.
        Value::StaticStr(_)=>Ok(vec![(s,Symbol::Unknown)]),
        Value::Unit=>Ok(vec![(s,Symbol::Unit)]),Value::U64(n)=>Ok(vec![(s,Symbol::Nat(format!("({n} : Nat)")))]),Value::Bool(b)=>Ok(vec![(s,Symbol::Prop(if *b{"True"}else{"False"}.into()))]),
        Value::ReadPlace(p)|Value::Move{place:p,..}|Value::Extract{place:p,..}|Value::Address(p)=>{
            let v=if exclusive_place(m,p) {
                let mut local=p.clone();
                local.pointer=false;
                read(&s,&local)?
            } else { read(&s,p)? };
            Ok(vec![(s,v)])
        },
        Value::SliceLen(source)=>{
            let mut out = Vec::new();
            for (state, source) in value(m,s,source,needed_after)? {
                let Symbol::SliceLength(length) = source else {
                    return Err(error("record proof needs a modeled slice length"));
                };
                out.push((state,Symbol::Nat(length)));
            }
            Ok(out)
        },
        Value::Aggregate(es)=>Ok(values(m,s,es,needed_after)?.into_iter().map(|(s,v)|(s,Symbol::Record(v))).collect()),
        Value::Sum{arm,payload}=>Ok(value(m,s,payload,needed_after)?.into_iter().map(|(s,v)|(s,Symbol::Sum(*arm,Box::new(v)))).collect()),
        Value::Project(v,i)=>{let mut out=vec![];for(s,v)in value(m,s,v,needed_after)?{let v=v.field(&[*i as usize]).cloned().ok_or_else(||error("record contracts invalid symbolic field"))?;out.push((s,v));}Ok(out)},
        Value::AllocationKey(v,_)=>{
            let mut out=vec![];
            for (s,v) in value(m,s,v,needed_after)? {
                let Symbol::Buffer(id)=v else{return Err(error("allocation key needs a live buffer loan"));};
                let focus=m.live_focus(&s,id)?;
                out.push((s,Symbol::Nat(m.key_word(&focus.key)?)));
            }
            Ok(out)
        },
        Value::Capacity(v,_)=>{let mut out=vec![];for(s,v)in value(m,s,v,needed_after)?{let Symbol::Buffer(id)=v else{return Err(error("record contracts capacity requires an allocation focus"));};let cap=m.live_focus(&s,id)?.capacity.clone();out.push((s,Symbol::Nat(cap)));}Ok(out)},
        Value::Not(v)=>{let mut out=vec![];for(s,v)in value(m,s,v,needed_after)?{let p=m.bind("Prop",format!("(¬ {})",scalar(&v)?))?;out.push((s,Symbol::Prop(p)));}Ok(out)},
        Value::Binary(op @ (Binary::And|Binary::Or),a,b)=>{
            let after=needed_after||m.needs_value(b);let mut out=vec![];for(s,left)in value(m,s,a,after)?{let left=scalar(&left)?;let and=matches!(op,Binary::And);
                if let Some(skipped)=branch(m,&s,left,!and)?{out.push((skipped,Symbol::Prop(if and{"False"}else{"True"}.into())));}
                if let Some(evaluated)=branch(m,&s,left,and)?{out.extend(value(m,evaluated,b,needed_after)?);}
            }Ok(out)
        },
        Value::Binary(op,a,b)=>{
            let after=needed_after||m.needs_value(b);let mut out=vec![];for(s,a)in value(m,s,a,after)?{for(mut s,b)in value(m,s,b,needed_after)?{
                let(a,b)=(scalar(&a)?,scalar(&b)?);let token=match op{Binary::Add=>"+",Binary::Sub=>"-",Binary::Div=>"/",Binary::Rem=>"%",Binary::Eq=>"=",Binary::Ne=>"≠",Binary::Lt=>"<",Binary::Le=>"≤",Binary::Gt=>">",Binary::Ge=>"≥",Binary::And=>"∧",Binary::Or=>"∨",_=>return Err(error("record contracts support only checked Add/Sub and pure comparisons/logic"))};
                let numeric=matches!(op,Binary::Add|Binary::Sub|Binary::Div|Binary::Rem);
                // Natural-number division and remainder agree with the unsigned ones for a nonzero divisor.
                if matches!(op,Binary::Div|Binary::Rem){let bound=format!("{b} > (0 : Nat)");if m.is_caller(){s.path.push((bound,true));}else{m.check(&s,bound,false)?;}}
                // Nat subtraction denotes a checked U64 result only after its
                // actual operands establish no underflow. Never assume this
                // body's own arithmetic obligation or an authored postcondition.
                if matches!(op,Binary::Sub){let bound=format!("{b} ≤ {a}");if m.is_caller(){s.path.push((bound,true));}else{m.check(&s,bound,false)?;}}
                let v=m.bind(if numeric{"Nat"}else{"Prop"},format!("({a} {token} {b})"))?;
                // Shown to fit the type the sum is written at: it is emitted with no test and no narrowing.
                if matches!(op,Binary::Add){if m.is_caller(){s.path.push((format!("{v} ≤ {}",u64::MAX),true));}else{m.check(&s,format!("{v} ≤ {}",match e.ty{Ty::Unsigned(scalar)=>scalar.layout().maximum(),_=>u64::MAX}),false)?;}}
                if matches!(op,Binary::Add|Binary::Sub)&&!m.is_caller(){m.shown_arithmetic.insert(e as *const Expr as usize);}
                out.push((s,if numeric{Symbol::Nat(v)}else{Symbol::Prop(v)}));
            }}Ok(out)
        },
        Value::Storage{op:StorageOp::Initialized,receiver,index,value:None,..}=>{
            let mut out=vec![];for(s,receiver)in value(m,s,receiver,needed_after||m.needs_value(index))?{
                let Symbol::Buffer(focus)=receiver else{return Err(error("record contracts initialized receiver is not an allocation focus"));};
                for(s,index)in value(m,s,index,needed_after)?{let index=scalar(&index)?;let f=m.live_focus(&s,focus)?;
                    let text=format!("({index} < {} ∧ {} {index} ≠ none)",f.capacity,f.cells);let p=m.bind("Prop",text)?;out.push((s,Symbol::Prop(p)));
                }
            }Ok(out)
        },
        Value::Storage{op:StorageOp::Take,receiver,index,value:None,..}=>{
            let mut out=vec![];
            // Evaluate actual arguments in source order. In particular, take the
            // cell snapshot AFTER index evaluation; unsupported call effects
            // must refuse, rather than leave a stale pre-argument heap focus.
            for(s,receiver)in value(m,s,receiver,true)?{
                let Symbol::Buffer(focus)=receiver else{return Err(error("record Take receiver requires an allocation focus"));};
                for(s,index)in value(m,s,index,true)?{
                    let index=scalar(&index)?.to_string();let descriptor=m.live_focus(&s,focus)?;
                    let old=descriptor.cells;let cap=descriptor.capacity;
                    let bound=m.bind("Prop",format!("{index} < {cap}"))?;
                    m.interest(&index)?;m.write_index(&index)?;
                    // Physical checked storage returns code 1 out of bounds,
                    // code 2 for an empty in-bounds cell, without mutation.
                    out.push((fork(m,&s,bound.clone(),false)?,Symbol::Sum(1,Box::new(Symbol::Nat("(1 : Nat)".into())))));
                    let bounded=fork(m,&s,bound,true)?;
                    let slot=m.bind("Option α",format!("{old} {index}"))?;
                    out.push((fork(m,&bounded,format!("{slot} = none"),true)?,Symbol::Sum(1,Box::new(Symbol::Nat("(2 : Nat)".into())))));
                    let mut full=fork(m,&bounded,format!("{slot} ≠ none"),true)?;
                    let updated=m.bind("Nat → Option α",format!("(fun j : Nat => if j = {index} then none else {old} j)"))?;
                    full.foci.get_mut(&focus).expect("focus").cells=updated;
                    // The payload IS the old selected Option, with nonnull
                    // provenance local to this Ok arm. No fresh alpha witness.
                    out.push((full,Symbol::Sum(0,Box::new(Symbol::Stored(slot)))));
                }
            }Ok(out)
        },
        // The proved take: shown in bounds and holding a value, which moves out and leaves the cell empty.
        Value::Storage{op:StorageOp::TakeUnchecked | StorageOp::ReplaceUnchecked,receiver,index,value:incoming,..} if !m.is_caller()=>{
            let mut out=vec![];
            for(s,receiver)in value(m,s,receiver,true)?{
                let Symbol::Buffer(focus)=receiver else{return Err(error("record unchecked Take receiver requires an allocation focus"));};
                for(s,index)in value(m,s,index,true)?{
                    let index=scalar(&index)?.to_string();let descriptor=m.live_focus(&s,focus)?;
                    let old=descriptor.cells;
                    m.check(&s,format!("{index} < {}",descriptor.capacity),false)?;
                    let slot=m.bind("Option α",format!("{old} {index}"))?;
                    m.check(&s,format!("{slot} ≠ none"),false)?;
                    m.seen_unchecked(e);m.interest(&index)?;m.write_index(&index)?;
                    let replacement = if let Some(incoming) = incoming {
                        let (payload, nonnull) = m.payload(&s, incoming, descriptor.element)?;
                        if nonnull.is_some() { return Err(error("record unchecked replace requires actual nonempty generic payload history")); }
                        m.check(&s, format!("{payload} ≠ none"), false)?;
                        payload
                    } else { "none".into() };
                    let mut taken=fork(m,&s,format!("{slot} ≠ none"),true)?;
                    let updated=m.bind("Nat → Option α",format!("(fun j : Nat => if j = {index} then {replacement} else {old} j)"))?;
                    taken.foci.get_mut(&focus).expect("focus").cells=updated;
                    out.push((taken,Symbol::Stored(slot)));
                }
            }Ok(out)
        },
        Value::Storage{op:StorageOp::InitUnchecked,receiver,index,value:Some(payload),..} if !m.is_caller()=>{
            let mut out=vec![];
            for(s,receiver)in value(m,s,receiver,m.needs_value(index)||m.needs_value(payload))?{
                let Symbol::Buffer(focus)=receiver else{return Err(error("record contracts unchecked Init receiver is not an allocation focus"));};
                for(mut s,index)in value(m,s,index,m.needs_value(payload))?{let index=scalar(&index)?.to_string();
                    let descriptor=m.live_focus(&s,focus)?;let (payload,nonnull)=m.payload(&s,payload,descriptor.element)?;
                    if nonnull.is_some(){return Err(error("record unchecked Init requires actual nonempty generic payload history"));}
                    m.check(&s,format!("{index} < {}",descriptor.capacity),false)?;
                    m.check(&s,format!("{} {index} = none",descriptor.cells),false)?;
                    m.check(&s,format!("{payload} ≠ none"),false)?;
                    m.seen_unchecked(e);m.interest(&index)?;m.write_index(&index)?;
                    let cells=m.bind("Nat → Option α",format!("(fun j : Nat => if j = {index} then {payload} else {} j)",descriptor.cells))?;
                    s.foci.get_mut(&focus).expect("focus").cells=cells;out.push((s,Symbol::Unit));
                }
            }Ok(out)
        },
        Value::Storage{op:StorageOp::Init,receiver,index,value:Some(payload),..}=>{
            let mut out=vec![];
            let after_index=needed_after||m.needs_value(payload);let after_receiver=after_index||m.needs_value(index);
            for(s,receiver)in value(m,s,receiver,after_receiver)?{let Symbol::Buffer(focus)=receiver else{return Err(error("record contracts Init receiver is not an allocation focus"));};
                for(s,index)in value(m,s,index,after_index)?{let index=scalar(&index)?.to_string();
                    let descriptor=m.live_focus(&s,focus)?;let old=descriptor.cells;let cap=descriptor.capacity;
                    let (payload,nonnull)=m.payload(&s,payload,descriptor.element)?;
                    let transition=m.bind("Option Nat × (Nat → Option α)",super::storage_model::init_option_at(&old,&index,&payload,&cap))?;
                    m.interest(&index)?;m.write_index(&index)?;
                    let bound=m.bind("Prop",format!("{index} < {cap}"))?;let empty=m.bind("Prop",format!("{old} {index} = none"))?;
                    let mut fail=fork(m,&s,bound.clone(),false)?;fail.foci.get_mut(&focus).expect("focus").cells=old.clone();out.push((fail,Symbol::Sum(1,Box::new(Symbol::Nat("(1 : Nat)".into())))));
                    let bounded=fork(m,&s,bound,true)?;let mut ok=fork(m,&bounded,empty,true)?;if let Some(nonnull)=nonnull{ok.path.push((nonnull,true));}ok.foci.get_mut(&focus).expect("focus").cells=format!("{transition}.2");out.push((ok,Symbol::Sum(0,Box::new(Symbol::Unit))));
                    let some=m.bind("Prop",format!("{old} {index} ≠ none"))?;
                    let mut full=fork(m,&bounded,some,true)?;full.foci.get_mut(&focus).expect("focus").cells=old;out.push((full,Symbol::Sum(1,Box::new(Symbol::Nat("(2 : Nat)".into())))));
                }
            }Ok(out)
        },
        Value::EntryConsole|Value::EntryInput|Value::EntryAllocator if m.is_caller()=>Ok(vec![(s,Symbol::Permission)]),
        Value::TryAlloc{allocator,capacity,type_id,..}=>{
            let after=needed_after||m.needs_value(capacity);let mut out=vec![];for(s,a)in value(m,s,allocator,after)?{if !matches!(a,Symbol::Permission){return Err(error("record caller allocation requires checked Allocator permission"));}for(s,c)in value(m,s,capacity,needed_after)?{
                let cap=scalar(&c)?.to_string();let choose=m.ghost("Prop")?;let code=m.ranged_ghost()?;
                out.push((fork(m,&s,choose.clone(),false)?,Symbol::Sum(1,Box::new(Symbol::Nat(code)))));
                let mut ok=fork(m,&s,choose,true)?;let id=m.allocation(&mut ok,type_id - 1,cap)?;out.push((ok,Symbol::Sum(0,Box::new(Symbol::Buffer(id)))));
            }}Ok(out)
        },
        Value::Exchange(left, right) => {
            let mut after = s;
            let left_fields = modeled_ordinals(m, left)?;
            let right_fields = modeled_ordinals(m, right)?;
            let left_value = after.locals.get(&left.root).and_then(|v| v.field(&left_fields))
                .cloned().ok_or_else(|| error("record proof exchange requires a modeled left place"))?;
            let right_value = after.locals.get(&right.root).and_then(|v| v.field(&right_fields))
                .cloned().ok_or_else(|| error("record proof exchange requires a modeled right place"))?;
            after.locals.get_mut(&left.root).and_then(|v| v.store(&left_fields, right_value))
                .ok_or_else(|| error("record proof exchange requires a modeled left place"))?;
            after.locals.get_mut(&right.root).and_then(|v| v.store(&right_fields, left_value))
                .ok_or_else(|| error("record proof exchange requires a modeled right place"))?;
            Ok(vec![(after, Symbol::Unit)])
        }
        Value::TryRelease(v,_) if !m.is_caller()=>{
            let mut out=vec![];for(s,v)in value(m,s,v,needed_after)?{let Symbol::Buffer(id)=v else{return Err(error("owned release requires tracked Buffer"));};let f=m.live_focus(&s,id)?;m.check(&s,f.live.clone(),false)?;let empty=m.bind("Prop",format!("∀ j : Nat, {} j = none",f.cells))?;
                let mut ok=fork(m,&s,empty.clone(),true)?;ok.foci.get_mut(&id).expect("focus").live="False".into();out.push((ok,Symbol::Sum(0,Box::new(Symbol::Unit))));
                out.push((fork(m,&s,empty,false)?,Symbol::Sum(1,Box::new(Symbol::Buffer(id)))));
            }Ok(out)
        },
        Value::Call(call) if !m.is_caller()=>{let mut out=vec![];for(s,args)in values(m,s,&call.args,true)?{
            let outcomes=m.body_called(s,call,args)?;let written=std::mem::take(&mut m.written);
            // A record lent to the callee is read back as the callee left it.
            for(mut s,v)in outcomes{for(i,symbol)in &written{
                let (Value::Address(p)|Value::ReadPlace(p))=&call.args[*i].kind else{return Err(error("record contracts lend a record from a local place"));};
                let fields=modeled_ordinals(m,p)?;
                s.locals.get_mut(&p.root).and_then(|target|target.store(&fields,symbol.clone())).ok_or_else(||error("record contracts invalid symbolic store"))?;}
                out.push((s,v));}}Ok(out)},
        Value::Call(call) if m.is_caller()=>{let before=needed_after||m.record_call(call);let mut out=vec![];for(s,args)in values(m,s,&call.args,before)?{out.extend(m.called(s,call,args,needed_after)?);}Ok(out)},
        // Widening, and narrowing a value already reduced below the target's range, keep the number.
        Value::Widen(v)=>value(m,s,v,needed_after),
        Value::ProofUtf8(_) => return Err(error("UTF-8 refinement requires a memory proof")),
        Value::Discard(v)=>Ok(value(m,s,v,needed_after)?.into_iter().map(|(s,_)|(s,Symbol::Unit)).collect()),
        _=>Err(error("record contracts unsupported runtime expression; no calls, allocation, loops or other storage operations")),
    }
}
pub(super) fn run(m:&mut Model<'_>,body:&[Stmt],states:Vec<State>)->Result<Vec<State>> {run_after(m,body,states,false)}
fn run_after(m:&mut Model<'_>,body:&[Stmt],states:Vec<State>,continuation:bool)->Result<Vec<State>> {
    let mut work=states;let mut required=vec![false;body.len()+1];
    if m.is_caller(){for i in (0..body.len()).rev(){required[i]=required[i+1]||m.needs(&body[i..i+1]);}}
    for(position,stmt)in body.iter().enumerate() {if m.is_caller()&&!continuation&&!required[position]{return Ok(work);}let future=m.is_caller()&&(continuation||required[position+1]);let mut next=vec![];
        for state in work {m.count()?;match stmt {
            Stmt::Set(id,e)=>for(mut s,v)in value(m,state,e,future)?{s.locals.insert(*id,v);next.push(s);},
            Stmt::Replace(_,p,e)|Stmt::StorePlace(p,None,e)=>{
                let fields=modeled_ordinals(m,p)?;for(mut s,v)in value(m,state,e,future)?{
                    s.locals.get_mut(&p.root).and_then(|target|target.store(&fields,v)).ok_or_else(||error("record contracts invalid symbolic store"))?;next.push(s);
                }
            },
            Stmt::StorePlace(p,Some(op @ (Binary::Add|Binary::Sub)),e)=>{
                // The destination is read before the right side runs, as the emitted code reads it.
                let fields=ordinals(p)?;let old=read(&state,p)?;for(mut s,v)in value(m,state,e,future)?{let add=matches!(op,Binary::Add);
                    if !add{let bound=format!("{} ≤ {}",scalar(&v)?,scalar(&old)?);if m.is_caller(){s.path.push((bound,true));}else{m.check(&s,bound,false)?;}}
                    let v=m.bind("Nat",format!("({} {} {})",scalar(&old)?,if add{"+"}else{"-"},scalar(&v)?))?;
                    if !add{}else if m.is_caller(){s.path.push((format!("{v} ≤ {}",u64::MAX),true));}else{m.check(&s,format!("{v} ≤ {}",match p.ty{Ty::Unsigned(scalar)=>scalar.layout().maximum(),_=>u64::MAX}),false)?;}
                    if !m.is_caller(){m.shown_arithmetic.insert(stmt as *const Stmt as usize);}
                    s.locals.get_mut(&p.root).and_then(|target|target.store(&fields,Symbol::Nat(v))).ok_or_else(||error("record contracts invalid symbolic Add store"))?;next.push(s);
                }
            },
            Stmt::Eval(e)=>for(s,_)in value(m,state,e,future)?{next.push(s);},
            Stmt::Return(e, _, _)=>for(s,v)in value(m,state,e,false)?{m.returned(&s,&v)?;},
            Stmt::If(e,a,b)=>{let after=future||m.needs(a)||m.needs(b);for(s,v)in value(m,state,e,after)?{if m.is_caller()&&!future&&!m.needs(a)&&!m.needs(b){next.push(s);continue;}let guard=scalar(&v)?.to_string();
                // Branches that only compute numbers rejoin as one state: each local they
                // disagree on becomes one unknown, tied to either value by the guard.
                if !m.is_caller()&&guard!="True"&&guard!="False"&&only_numbers(a)&&only_numbers(b){
                    let taken=fork(m,&s,guard.clone(),true)?;let yes=run_after(m,a,vec![taken],future)?;let skipped=fork(m,&s,guard.clone(),false)?;let no=run_after(m,b,vec![skipped],future)?;
                    if let ([yes],[no])=(yes.as_slice(),no.as_slice()){
                        let mut joined=s.clone();let base=s.path.len()+1;let mut same=true;
                        for(branch,positive)in [(yes,true),(no,false)]{for(text,holds)in &branch.path[base..]{joined.path.push((format!("{}{guard} → ({}{text})",if positive{""}else{"¬ "},if *holds{""}else{"¬ "}),true));}}
                        let ids=yes.locals.keys().chain(no.locals.keys()).copied().collect::<BTreeSet<_>>();
                        for id in ids{match(yes.locals.get(&id),no.locals.get(&id)){
                            (Some(Symbol::Nat(x)),Some(Symbol::Nat(y))) if x!=y=>{let n=m.ghost("Nat")?;joined.path.push((format!("{guard} → {n} = {x}"),true));joined.path.push((format!("¬ {guard} → {n} = {y}"),true));joined.locals.insert(id,Symbol::Nat(n));},
                            (Some(x),Some(y)) if format!("{x:?}")==format!("{y:?}")=>{joined.locals.insert(id,x.clone());},
                            // A local bound in only one branch is out of scope afterwards.
                            (Some(_),None)|(None,Some(_))=>{},
                            _=>{same=false;},
                        }}
                        if same{next.push(joined);continue;}
                    }
                    next.extend(yes);next.extend(no);continue;
                }
                if let Some(yes)=branch(m,&s,&guard,true)?{next.extend(run_after(m,a,vec![yes],future)?);}
                if let Some(no)=branch(m,&s,&guard,false)?{next.extend(run_after(m,b,vec![no],future)?);}
            }},
            Stmt::Match{value:e,arms}=>{let after=future||arms.iter().any(|a|m.needs(&a.body));for(mut s,v)in value(m,state,e,after)?{if m.is_caller()&&!future&&!arms.iter().any(|a|m.needs(&a.body)){next.push(s);continue;}let Symbol::Sum(arm,v)=v else{return Err(error("record contracts runtime match requires an actual Result"));};let a=&arms[arm as usize];if let Some(id)=a.binding{s.locals.insert(id,*v);}next.extend(run_after(m,&a.body,vec![s],future)?);}},
            Stmt::Trap(_) => {
                if !m.is_caller() {
                    m.check(&state, "False".into(), false)?;
                    m.checked_traps.insert(stmt as *const Stmt as usize);
                }
            }
            Stmt::Expire(_)=>next.push(state),
            Stmt::While(condition,body,invariants) if !m.is_caller()=>{
                if invariants.record.is_empty(){return Err(error("record contracts loops require invariants"));}
                let mut assigned=BTreeSet::new();assigned_locals(body,&mut assigned);
                // Locals first bound inside the body do not outlive an iteration.
                assigned.retain(|id|state.locals.contains_key(id));
                // Owners the loop may replace. They must be live on entry and at every back edge.
                let owners=|s:&State|{let mut ids=vec![];for id in &assigned{if let Some(v)=s.locals.get(id){owner_ids(v,&mut ids);}}ids};
                let carried=owners(&state);
                for id in &carried{let live=m.focus(&state,*id)?.live.clone();m.check(&state,live,false)?;}
                for(text,index)in m.invariant_parts(&state,&invariants.record)?{m.check(&state,text,index)?;}
                // Any iteration: what the body assigns is arbitrary, constrained only by the invariants.
                let mut head=state.clone();
                for id in &assigned{if let Some(old)=head.locals.get(id).cloned(){let fresh=m.arbitrary(&old,&mut head)?;head.locals.insert(*id,fresh);}}
                let facts=m.invariant_parts(&head,&invariants.record)?;m.imported_facts(&mut head,facts)?;
                for(s,v)in value(m,head.clone(),condition,true)?{let guard=scalar(&v)?.to_string();
                    if let Some(yes)=branch(m,&s,&guard,true)?{for back in run_after(m,body,vec![yes],future)?{
                        let kept=owners(&back);if kept.len()!=carried.len(){return Err(error("record loop body changes which locals own allocations"));}
                        for(id,focus)in &back.foci{
                            if kept.contains(id){m.check(&back,focus.live.clone(),false)?;}
                            else if !carried.contains(id)&&head.foci.get(id).is_some_and(|before|before.live==focus.live&&before.cells==focus.cells&&before.key==focus.key){}
                            else{m.check(&back,format!("¬ {}",focus.live),false)?;}
                        }
                        for(text,index)in m.invariant_parts(&back,&invariants.record)?{m.check(&back,text,index)?;}
                    }}
                    if let Some(no)=branch(m,&s,&guard,false)?{next.push(no);}
                }
            },
            _=>return Err(error("record contracts unsupported runtime statement; loops and effects are not proved")),
        }}
        if next.len()>4096{return Err(error("record contracts pending state complexity limit"));}work=next;
    }Ok(work)
}

/// Statements that only assign numbers computed from locals: no calls, memory, moves or returns.
fn only_numbers(body:&[Stmt])->bool{
    fn pure(e:&Expr)->bool{match &e.kind{Value::U64(_)|Value::Bool(_)=>true,Value::ReadPlace(p)=>!p.pointer&&p.projections.iter().all(|p|matches!(p,Projection::Field{..})),
        Value::Binary(_,a,b)=>pure(a)&&pure(b),Value::Not(v)|Value::Widen(v)=>pure(v),_=>false}}
    body.iter().all(|s|match s{Stmt::Set(_,e)=>matches!(e.ty,Ty::Unsigned(_))&&pure(e),Stmt::StorePlace(p,Some(_),e)=>p.projections.is_empty()&&!p.pointer&&pure(e),
        Stmt::Replace(_,p,e)|Stmt::StorePlace(p,None,e)=>p.projections.is_empty()&&!p.pointer&&matches!(e.ty,Ty::Unsigned(_))&&pure(e),
        Stmt::If(c,a,b)=>pure(c)&&only_numbers(a)&&only_numbers(b),Stmt::Expire(_)=>true,_=>false})
}
pub(super) fn owner_ids(v:&Symbol,out:&mut Vec<u32>){match v{Symbol::Buffer(id)=>out.push(*id),Symbol::Record(fields)=>for field in fields{owner_ids(field,out);},Symbol::Sum(_,payload)=>owner_ids(payload,out),_=>{}}}
/// Every local a block can write, including the binders of its matches.
fn assigned_locals(body:&[Stmt],out:&mut BTreeSet<u32>){for s in body{match s{
    Stmt::Set(id,_)=>{out.insert(*id);},Stmt::Replace(_,p,_)|Stmt::StorePlace(p,_,_)=>{out.insert(p.root);},
    Stmt::If(_,a,b)=>{assigned_locals(a,out);assigned_locals(b,out);},Stmt::While(_,a,_)=>assigned_locals(a,out),
    Stmt::Match{arms,..}=>for arm in arms{if let Some(id)=arm.binding{out.insert(id);}assigned_locals(&arm.body,out);},
    Stmt::Eval(_)|Stmt::Return(..)|Stmt::Expire(_)|Stmt::Break|Stmt::Trap(_)|Stmt::Continue=>{},
}}}
/// Inventory includes inactive paths and every evaluated argument/place descendant.
/// Pointer identities refer only to this immutable primary typed body.
pub(super) fn unchecked_nodes(body: &[Stmt]) -> BTreeSet<usize> {
    proof_only_nodes(body).storage
}
/// Every unsigned `+ - *` of a body and every statement that does one as it stores: what a
/// proof must have shown in range, node for node, before the checks are left out.
pub(super) fn arithmetic_nodes(body: &[Stmt]) -> BTreeSet<usize> {
    proof_only_nodes(body).arithmetic
}
pub(super) fn has_unchecked_operations(body: &[Stmt]) -> bool {
    let nodes = proof_only_nodes(body);
    nodes.utf8 || !nodes.storage.is_empty()
}
#[derive(Default)]
struct ProofOnlyNodes {
    storage: BTreeSet<usize>,
    utf8: bool,
    /// Every unsigned `+ - *`, and every statement that does one as it stores (`x += n`).
    arithmetic: BTreeSet<usize>,
}
fn proof_only_nodes(body: &[Stmt]) -> ProofOnlyNodes {
    fn place(p:&Place,out:&mut ProofOnlyNodes){for p in &p.projections{if let Projection::Index{value:v,..}=p{value(v,out);}}}
    fn value(e:&Expr,out:&mut ProofOnlyNodes){match &e.kind{
        Value::Storage{op,receiver,index,value:payload,..}=>{value(receiver,out);value(index,out);if let Some(v)=payload{value(v,out);}if op.proof_admitted(){out.storage.insert(e as *const Expr as usize);}},
        Value::ProofUtf8(source) => { out.utf8 = true; value(source, out); },
        Value::Call(c)=>for a in &c.args{value(a,out);},
        Value::ReadPlace(p)|Value::Address(p)|Value::Move{place:p,..}|Value::Extract{place:p,..}=>place(p,out),
        Value::Stored(p,v)=>{value(v,out);place(p,out)},
        Value::TryAlloc{allocator,capacity,..}=>{value(allocator,out);value(capacity,out);},
        Value::Binary(op,a,b) if matches!(op,Binary::Add|Binary::Sub|Binary::Mul)=>{out.arithmetic.insert(e as *const Expr as usize);value(a,out);value(b,out);},
        Value::Binary(_,a,b)|Value::Index(a,b,_)|Value::SliceAt(a,b,_)|Value::SliceIndex(a,b,_)|Value::SliceCopy(a,b)|Value::SliceReadLe64(a,b)=>{value(a,out);value(b,out);},
        Value::SliceOf(p,_)=>place(p,out),Value::SliceLen(v)=>value(v,out),
        Value::SlicePart(a,b,c)|Value::SliceWithin(a,b,c)=>{value(a,out);value(b,out);value(c,out);},
        Value::AllocationKey(v,_)|Value::Capacity(v,_)|Value::TryRelease(v,_)|Value::Discard(v)|Value::Sum{payload:v,..}|Value::Repeat(v,_)|Value::Retag(v,_)|Value::Project(v,_)|Value::Widen(v)|Value::Not(v)=>value(v,out),
        Value::Aggregate(fs)|Value::Signed(_,fs)|Value::Float(_,fs) | Value::Bits(_, fs) | Value::BoxOp(_, _, fs)=>for v in fs{value(v,out);},Value::Print(p,v,_)=>{place(p,out);value(v,out);},Value::Read(p,_)=>place(p,out),Value::Host(_,p,vs)=>{if let Some(p)=p{place(p,out);}for v in vs{value(v,out);}},Value::Exchange(a,b)=>{place(a,out);place(b,out);},
        Value::TaskSpawn{allocator,bytes,entry}=>{value(allocator,out);value(bytes,out);value(entry,out);},
        Value::TaskStep(v,w)=>{value(v,out);value(w,out);},
        Value::TaskFinish(v)|Value::TaskStatus(v)|Value::Pause(v)=>value(v,out),
        Value::StaticStr(_) | Value::Unit|Value::U64(_)|Value::Bool(_)|Value::EntryConsole|Value::EntryInput|Value::EntryAllocator|Value::TaskWord|Value::InTask|Value::EntryHost(..)=>{},
    }}
    fn walk(body:&[Stmt],out:&mut ProofOnlyNodes){for s in body{match s{
        Stmt::Set(_,v)|Stmt::Eval(v)|Stmt::Return(v, _, _)=>value(v,out),Stmt::StorePlace(p,op,v)=>{if op.is_some(){out.arithmetic.insert(s as *const Stmt as usize);}place(p,out);value(v,out);},Stmt::Replace(_,p,v)=>{place(p,out);value(v,out);},
        Stmt::If(c,a,b)=>{value(c,out);walk(a,out);walk(b,out);},Stmt::While(c,a,_)=>{value(c,out);walk(a,out);},
        Stmt::Match{value:v,arms}=>{value(v,out);for a in arms{walk(&a.body,out);}},Stmt::Expire(_)|Stmt::Break|Stmt::Trap(_)|Stmt::Continue=>{},
    }}}
    let mut out = ProofOnlyNodes::default();
    walk(body, &mut out);
    out
}
