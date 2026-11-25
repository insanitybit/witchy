//! Actual borrowed/owned helper calls. Facts come only from current checked clauses.
use super::{logical_body::*,memory_contract as mc,record_model::Model,record_state::State,record_value::Symbol,storage_contract::error};
use crate::diagnostic::Diagnostic;
use std::collections::BTreeMap;
type Result<T> = std::result::Result<T,Diagnostic>;
fn path<'a>(p:&mc::Path,now:&'a BTreeMap<u32,Symbol>,before:&'a BTreeMap<u32,Symbol>)->Result<&'a Symbol>{
    (if p.before{before}else{now}).get(&p.root).and_then(|v|v.field(&p.fields)).ok_or_else(||error("owned memory call missing actual path"))
}
/// A clause of a numeric contract as Lean text over the record model's values: the callee's
/// parameters are the actual arguments and `result` is its answer. `None` for anything the
/// clause language has that this does not render (fields, methods, integers, Bools as numbers);
/// the call is then treated as before.
fn scalar_math(m:&super::contract::Math,args:&[Symbol],result:Option<&str>)->Option<String>{
    use super::contract::Math as M;
    let pair=|a:&M,b:&M,op:&str|Some(format!("({} {op} {})",scalar_math(a,args,result)?,scalar_math(b,args,result)?));
    match m{
        M::Literal(n)=>Some(format!("({n} : Nat)")),
        M::Binding(i)=>match args.get(*i as usize)?{Symbol::Nat(text)=>Some(text.clone()),_=>None},
        M::Result=>result.map(str::to_string),
        M::Add(a,b)=>pair(a,b,"+"),M::Sub(a,b)=>pair(a,b,"-"),M::Mul(a,b)=>pair(a,b,"*"),M::Div(a,b)=>pair(a,b,"/"),M::Mod(a,b)=>pair(a,b,"%"),
        M::Int(_)|M::Cast(..)|M::Observe{..}|M::Field{..}=>None,
    }
}
/// `truth` is the answer when the callee returns a Bool: a clause reads it only as `Truth(Result)`.
fn scalar_prop(p:&super::contract::Prop,args:&[Symbol],number:Option<&str>,truth:Option<&str>)->Option<String>{
    use super::contract::{Math as M,Prop as P};
    match p{
        P::Bool(b)=>Some(if *b{"True"}else{"False"}.into()),
        P::Compare(op,a,b)=>Some(format!("({} {op} {})",scalar_math(a,args,number)?,scalar_math(b,args,number)?)),
        P::Not(v)=>Some(format!("(¬ {})",scalar_prop(v,args,number,truth)?)),
        P::Binary(op,a,b)=>Some(format!("({} {op} {})",scalar_prop(a,args,number,truth)?,scalar_prop(b,args,number,truth)?)),
        P::Truth(M::Result)=>truth.map(str::to_string),
        P::Truth(_)=>None,
    }
}
impl Model<'_>{
    /// A call to a function whose contract is numeric only: its parameters and its answer are
    /// numbers (or the answer a Bool), and it touches no memory. What it requires is shown
    /// here against the actual arguments; its answer is a new value of which what it ensures is
    /// known. The callee's own body is proved by the numeric walk, which relies on no record or
    /// memory contract, so the two cannot lean on each other. `None`: not such a call.
    fn scalar_called(&mut self,s:&State,callee:&Function,call:&InstanceCall,args:&[Symbol])->Result<Option<Vec<(State,Symbol)>>>{
        let Some(contract)=callee.contract.as_ref().filter(|c|c.storage.is_none()&&c.record.is_none()&&c.memory.is_none()) else{return Ok(None)};
        if !call.substitutions.is_empty()||!callee.params.iter().all(|t|matches!(t,Ty::Unsigned(_)))||!matches!(callee.result,Ty::Unsigned(_)|Ty::Bool){return Ok(None);}
        if contract.requires.iter().chain(&contract.ensures).any(|c|c.arm.is_some()){return Ok(None);}
        // Render every clause before changing anything, so a call this cannot read is left as before.
        let mut due=vec![];for c in &contract.requires{let Some(text)=scalar_prop(&c.value,args,None,None) else{return Ok(None)};due.push(text);}
        let mut after=s.clone();let answer=self.entry_value(callee.result,&mut after)?;
        let(number,truth)=match &answer{Symbol::Nat(n)=>(Some(n.as_str()),None),Symbol::Prop(p)=>(None,Some(p.as_str())),_=>return Ok(None)};
        let mut known=vec![];for c in &contract.ensures{let Some(text)=scalar_prop(&c.value,args,number,truth) else{return Ok(None)};known.push(text);}
        for text in due{self.check(s,text,false)?;}
        for text in known{after.path.push((text,true));}
        Ok(Some(vec![(after,answer)]))
    }
    fn memory_focus(&self,p:&mc::Path,bindings:&BTreeMap<u32,Symbol>,s:&State,old:&State)->Result<super::record_focus::Focus>{
        let Symbol::Buffer(id)=path(p,bindings,self.call_before.as_ref().unwrap_or(bindings))? else{return Err(error("owned memory clause requires actual Buffer"));};
        self.focus(if p.before{old}else{s},*id).cloned()
    }
    fn memory_number(&mut self,n:&mc::Number,bindings:&BTreeMap<u32,Symbol>,s:&State,old:&State)->Result<String>{
        self.count()?;Ok(match n{mc::Number::Literal(n)=>format!("({n} : Nat)"),mc::Number::Path(p)=>match path(p,bindings,self.call_before.as_ref().unwrap_or(bindings))?{Symbol::Nat(v)=>v.clone(),_=>return Err(error("owned memory clause requires actual U64"))},mc::Number::AllocationKey(p)=>{let f=self.memory_focus(p,bindings,s,old)?;self.key_word(&f.key)?},mc::Number::Capacity(p)=>self.memory_focus(p,bindings,s,old)?.capacity,mc::Number::Bound=>"j".into(),mc::Number::Add(a,b)=>format!("({} + {})",self.memory_number(a,bindings,s,old)?,self.memory_number(b,bindings,s,old)?)})
    }
    fn memory_flat(&mut self,p:&mc::Predicate,bindings:&BTreeMap<u32,Symbol>,s:&State,old:&State,result:&Symbol)->Result<Vec<(String,bool)>>{
        self.count()?;
        Ok(match p{
            mc::Predicate::Binary("∧",a,b)=>{let mut out=self.memory_flat(a,bindings,s,old,result)?;out.extend(self.memory_flat(b,bindings,s,old,result)?);out},
            mc::Predicate::Forall(lo,hi,body)=>{let lo=self.memory_number(lo,bindings,s,old)?;let hi=self.memory_number(hi,bindings,s,old)?;let body=self.memory_predicate(body,bindings,s,old,result)?;
                let text=format!("∀ j : Nat, ({lo} ≤ j ∧ j < {hi}) → {body}");
                let read=body.match_indices(" j ≠ none").chain(body.match_indices(" j = none")).filter_map(|(at,_)|body[..at].rsplit(|c:char|!c.is_alphanumeric()&&c!='_').next().filter(|t|!t.is_empty()).map(str::to_string)).collect();
                self.occupancy.insert(text.clone(),read);vec![(text,true)]},
            mc::Predicate::SamePrefix(a,b,n)=>{let fa=self.memory_focus(a,bindings,s,old)?;let fb=self.memory_focus(b,bindings,s,old)?;let n=self.memory_number(n,bindings,s,old)?;vec![(format!("{n} ≤ {}",fa.capacity),false),(format!("{n} ≤ {}",fb.capacity),false),(format!("∀ j : Nat, j < {n} → {} j = {} j ∧ {} j ≠ none",fa.cells,fb.cells,fb.cells),true)]},
            mc::Predicate::Except(a,b,_)|mc::Predicate::Unchanged(a,b)|mc::Predicate::From(a,b,_)=>{let fa=self.memory_focus(a,bindings,s,old)?;let fb=self.memory_focus(b,bindings,s,old)?;let quant=match p{mc::Predicate::Except(_,_,n)=>format!("∀ j : Nat, j ≠ {} → {} j = {} j",self.memory_number(n,bindings,s,old)?,fa.cells,fb.cells),mc::Predicate::From(_,_,n)=>format!("∀ j : Nat, {} ≤ j → {} j = {} j",self.memory_number(n,bindings,s,old)?,fa.cells,fb.cells),_=>format!("∀ j : Nat, {} j = {} j",fa.cells,fb.cells)};vec![(format!("{} = {}",fa.key,fb.key),false),(format!("{} = {}",fa.capacity,fb.capacity),false),(quant,true)]},
            mc::Predicate::EmptyAll(p)=>{let f=self.memory_focus(p,bindings,s,old)?;vec![(format!("∀ j : Nat, {} j = none",f.cells),true)]},
            mc::Predicate::EmptyFrom(p,n)=>{let f=self.memory_focus(p,bindings,s,old)?;let n=self.memory_number(n,bindings,s,old)?;vec![(format!("∀ j : Nat, {n} ≤ j → {} j = none",f.cells),true)]},
            mc::Predicate::FullRange(p,a,b)=>{let f=self.memory_focus(p,bindings,s,old)?;let a=self.memory_number(a,bindings,s,old)?;let b=self.memory_number(b,bindings,s,old)?;vec![(format!("∀ j : Nat, ({a} ≤ j ∧ j < {b}) → {} j ≠ none",f.cells),true)]},
            mc::Predicate::EmptyRange(p,a,b)=>{let f=self.memory_focus(p,bindings,s,old)?;let a=self.memory_number(a,bindings,s,old)?;let b=self.memory_number(b,bindings,s,old)?;vec![(format!("∀ j : Nat, ({a} ≤ j ∧ j < {b}) → {} j = none",f.cells),true)]},
            _=>vec![(self.memory_predicate(p,bindings,s,old,result)?,false)],
        })
    }
    fn memory_predicate(&mut self,p:&mc::Predicate,bindings:&BTreeMap<u32,Symbol>,s:&State,old:&State,result:&Symbol)->Result<String>{
        self.count()?;Ok(match p{
            mc::Predicate::Bool(b)=>if *b{"True"}else{"False"}.into(),
            mc::Predicate::Compare(op,a,b)=>format!("({} {op} {})",self.memory_number(a,bindings,s,old)?,self.memory_number(b,bindings,s,old)?),
            mc::Predicate::Not(p)=>format!("(¬ {})",self.memory_predicate(p,bindings,s,old,result)?),
            mc::Predicate::Binary(op,a,b)=>format!("({} {op} {})",self.memory_predicate(a,bindings,s,old,result)?,self.memory_predicate(b,bindings,s,old,result)?),
            mc::Predicate::Initialized(p,n)=>{let f=self.memory_focus(p,bindings,s,old)?;let n=self.memory_number(n,bindings,s,old)?;format!("({n} < {} ∧ {} {n} ≠ none)",f.capacity,f.cells)},
            mc::Predicate::Empty(p,n)=>{let f=self.memory_focus(p,bindings,s,old)?;let n=self.memory_number(n,bindings,s,old)?;format!("({} {n} = none)",f.cells)},
            mc::Predicate::Cell(p,n,item)=>{let f=self.memory_focus(p,bindings,s,old)?;let n=self.memory_number(n,bindings,s,old)?;let item=match item{mc::Element::Result=>result,mc::Element::Path(p)=>path(p,bindings,self.call_before.as_ref().unwrap_or(bindings))?};let slot=self.element_option(s,item)?;format!("({slot} ≠ none ∧ {} {n} = {slot})",f.cells)},
            mc::Predicate::Utf8Boundary(p,index)=>{
                let f=self.memory_focus(p,bindings,s,old)?;
                let index=self.memory_number(index,bindings,s,old)?;
                format!("(¬ Witchy.Utf8.Continuation (({} {index}).getD 256))",f.cells)
            },
            mc::Predicate::Utf8(p,start,count)=>{
                let f=self.memory_focus(p,bindings,s,old)?;
                let start=self.memory_number(start,bindings,s,old)?;
                let count=self.memory_number(count,bindings,s,old)?;
                format!("(Witchy.Utf8.ValidCells {} {start} {count})",f.cells)
            },
            mc::Predicate::SliceContents(..) => return Err(error("record proof does not model slice contents yet")),
            mc::Predicate::Same(a,b)=>format!("({} = {})",self.memory_focus(a,bindings,s,old)?.key,self.memory_focus(b,bindings,s,old)?.key),
            // The Bool the callee returned.
            mc::Predicate::Result=>match result{Symbol::Prop(answer)=>answer.clone(),_=>return Err(error("memory import result is not a Bool"))},
            // Only what `memory_flat` takes apart itself goes there; anything else would come
            // back. The match names every kind, so a new one must choose a side here.
            mc::Predicate::Forall(..)|mc::Predicate::SamePrefix(..)|mc::Predicate::Except(..)|mc::Predicate::Unchanged(..)|mc::Predicate::From(..)
                |mc::Predicate::EmptyAll(..)|mc::Predicate::EmptyFrom(..)|mc::Predicate::FullRange(..)|mc::Predicate::EmptyRange(..)=>format!("({})",self.memory_flat(p,bindings,s,old,result)?.into_iter().map(|(s,_)|format!("({s})")).collect::<Vec<_>>().join(" ∧ ")),
        })
    }
    /// The same record with every number replaced by one nothing is known about yet.
    fn changed_numbers(&mut self,value:&Symbol)->Result<Symbol>{self.count()?;Ok(match value{
        Symbol::Nat(_)=>Symbol::Nat(self.ranged_ghost()?),Symbol::Prop(_)=>Symbol::Prop(self.ghost("Prop")?),
        Symbol::Buffer(id)=>Symbol::Buffer(*id),Symbol::Unit=>Symbol::Unit,
        Symbol::Record(fields)=>Symbol::Record(fields.iter().map(|f|self.changed_numbers(f)).collect::<Result<_>>()?),
        _=>return Err(error("a borrowed record that holds an element cannot be passed to a proved helper yet")),
    })}
    pub(super) fn imported_facts(&mut self,s:&mut State,facts:Vec<(String,bool)>)->Result<()>{
        for(text,index)in facts{self.count()?;if index{if let Some(rest)=text.strip_prefix("∀ j : Nat, j < "){if let Some((bound,_))=rest.split_once(" → "){self.local_prefixes.insert(text.clone(),bound.into());}}}s.path.push((text,true));}Ok(())
    }
    pub fn body_called(&mut self,s:State,call:&InstanceCall,args:Vec<Symbol>)->Result<Vec<(State,Symbol)>>{
        let Some(functions)=self.functions else{self.check(&s,"False".into(),false)?;return Ok(vec![]);};
        let callee=&functions[call.definition.0 as usize];
        if self.function==Some(call.definition)||callee.contract.is_none(){self.check(&s,"False".into(),false)?;return Ok(vec![]);}
        if let Some(outcome)=self.scalar_called(&s,callee,call,&args)?{return Ok(outcome);}
        let own=self.function.ok_or_else(||error("owned call missing body identity"))?;
        let element=self.element().unwrap_or(Ty::Param{owner:own,index:0});
        // A generic callee is instantiated at this body's element type; a non-generic one must already use it.
        let fixed=callee.contract.as_ref().and_then(|c|c.record.as_ref().map(|c|c.element).or_else(||c.memory.as_ref().map(|c|c.element))).filter(|e|!matches!(e,Ty::Param{..}));
        if !(call.substitutions==[element]||(call.substitutions.is_empty()&&fixed==Some(element))){return Err(error("owned helper actual generic substitution mismatch"));}
        let site=call.contract_site.ok_or_else(||error("owned helper missing typed site"))?;
        if let Some(contract)=callee.contract.as_ref().and_then(|c|c.memory.as_ref()){
            // What the helper requires is shown here against the actual arguments, in its own
            // vocabulary. So is the invariant of each sealed value it borrows, when this is the
            // declaring file: code outside it cannot hold a broken one.
            let own=self.contract.map(|c|c.module);
            let due=contract.requires.iter().chain(contract.assumes.iter().filter(|(module,_)|Some(*module)==own).map(|(_,p)|p)).cloned().collect::<Vec<_>>();
            if !due.is_empty(){
                let actual=args.iter().cloned().enumerate().map(|(i,v)|(i as u32,v)).collect::<BTreeMap<_,_>>();
                for p in &due{for(text,index)in self.memory_flat(p,&actual,&s,&s,&Symbol::Unit)?{self.check(&s,text,index)?;}}
            }
            // A helper that only reads through shared borrows changes no cells.
            // Its checked postconditions describe the returned Bool, number or sum.
            let answers:Option<Vec<Ty>>=match self.types.result_sum(callee.result){Some(sum)=>Some(sum.arms.to_vec()),None=>Some(vec![callee.result])}
                .filter(|arms|arms.iter().all(|t|matches!(t,Ty::Bool|Ty::Unit|Ty::Unsigned(_))||*t==contract.element));
            if let(false,Some(arms),true)=(contract.mutable,answers,callee.params.iter().all(|t|*t==Ty::U64||matches!(t,Ty::Borrow{mutable:false,..}))){
                let mut owners=vec![];for v in &args{super::record_state::owner_ids(v,&mut owners);}
                for id in owners{let live=self.focus(&s,id)?.live.clone();self.check(&s,live,false)?;}
                self.imports.insert((call.definition.0,site));
                let plain=self.types.result_sum(callee.result).is_none();let choice=if plain{String::new()}else{self.ghost("Prop")?};let mut out=vec![];
                let actual = args.into_iter().enumerate().map(|(i, value)| (i as u32, value)).collect::<BTreeMap<_, _>>();
                for (arm, ty) in arms.into_iter().enumerate() {
                    let mut after = s.clone();
                    let selected = (!plain).then_some(arm as u8);
                    if !plain { after.path.push((choice.clone(), arm == 0)); }
                    let ty = if ty == contract.element { element } else { ty };
                    let answer = self.entry_value(ty, &mut after)?;
                    let mut bindings = actual.clone();
                    bindings.insert(u32::MAX, answer.clone());
                    self.call_before = Some(actual.clone());
                    let checks = (|| {
                        for (wanted, predicate) in &contract.ensures {
                            if wanted.is_some() && *wanted != selected { continue; }
                            // An opaque shared argument cannot contribute facts.
                            if let Ok(facts) = self.memory_flat(predicate, &bindings, &after, &s, &answer) {
                                self.imported_facts(&mut after, facts)?;
                            }
                        }
                        Ok::<_, crate::diagnostic::Diagnostic>(())
                    })();
                    self.call_before = None;
                    checks?;
                    out.push((after, if plain { answer } else { Symbol::Sum(arm as u8, Box::new(answer)) }));
                }
                return Ok(out);
            }
            let snapshot=s.clone();let mut after=s;let mut actual_ids=std::collections::BTreeSet::new();let mut bindings=args.into_iter().enumerate().map(|(i,v)|(i as u32,v)).collect::<BTreeMap<_,_>>();
            // The borrowed value is a Buffer or a record that owns one. The allocation stays
            // where it is: a function proved this way has no allocator and nothing to swap in.
            let before=bindings.clone();let mut written=vec![];
            for(i,ty)in callee.params.iter().enumerate(){if let Ty::Borrow{mutable,..}=ty{
                let actual=bindings.get(&(i as u32)).cloned().ok_or_else(||error("owned memory import missing actual buffer"))?;
                if matches!(ty, Ty::Borrow{mutable:false,pointee:Pointee::Slice(_)})
                    && matches!(actual,Symbol::SliceLength(_))
                {
                    continue;
                }
                // Every allocation the borrowed value owns: each is live and distinct, and each a
                // mutable borrow reaches comes back with cells only the callee's clauses describe.
                let owned=super::record_post::owners(&actual);if owned.is_empty(){return Err(error("owned memory import missing actual buffer"));}
                for(id,_)in owned{if !actual_ids.insert(id){return Err(error("owned memory imports require nonoverlapping actual foci"));}let focus=self.live_focus(&snapshot,id)?;
                    if focus.element!=element{return Err(error("owned memory import actual buffer element mismatch"));}
                    if *mutable{after.foci.get_mut(&id).expect("focus").cells=self.ghost("Nat → Option α")?;}}
                if *mutable{
                    // A borrowed record comes back with numbers the callee chose; its clauses say which.
                    if !matches!(actual,Symbol::Buffer(_)){let changed=self.changed_numbers(&actual)?;bindings.insert(i as u32,changed.clone());written.push((i,changed));}}
            }}
            self.imports.insert((call.definition.0,site));
            // A Bool or number answer is one outcome, its value unknown but for the callee's clauses.
            let answer=matches!(callee.result,Ty::Bool|Ty::Unsigned(_));
            let arms:Vec<Option<(u8,Ty)>>=if callee.result==Ty::Unit||answer{vec![None]}else if let Some(sum)=self.types.result_sum(callee.result){vec![Some((0,sum.arms[0])),Some((1,sum.arms[1]))]}
                else{return Err(error("owned memory import unsupported return shape"));};
            let choice=if arms.len()==2{self.ghost("Prop")?}else{String::new()};
            self.call_before=Some(before);
            let outcomes=(||{let mut out=vec![];for arm in arms{let mut after=after.clone();
                let(result,selected)=match arm{None if answer=>(self.entry_value(callee.result,&mut after)?,None),None=>(Symbol::Unit,None),Some((arm,ty))=>{after.path.push((choice.clone(),arm==0));
                    // The callee's element type is this body's.
                    let ty=if ty==contract.element{element}else{ty};
                    if !(matches!(ty,Ty::Unit|Ty::Bool|Ty::Unsigned(_))||ty==element){return Err(error("owned memory import unsupported return shape"));}
                    (Symbol::Sum(arm,Box::new(self.entry_value(ty,&mut after)?)),Some(arm))}};
                let mut bindings=bindings.clone();if let Symbol::Sum(_,payload)=&result{bindings.insert(u32::MAX,*payload.clone());}
                for(wanted,p)in &contract.ensures{if wanted.is_some()&&*wanted!=selected{continue;}let facts=self.memory_flat(p,&bindings,&after,&snapshot,&result)?;self.imported_facts(&mut after,facts)?;}
                // What the callee keeps true of a sealed record it borrowed holds here afterwards.
                for(_,p)in &contract.maintains{let facts=self.memory_flat(p,&bindings,&after,&snapshot,&result)?;self.imported_facts(&mut after,facts)?;}
                out.push((after,result));
            }Ok(out)})();
            self.call_before=None;self.written=written;return outcomes;
        }
        if let Some(contract)=callee.contract.as_ref().and_then(|c|c.record.as_ref()){
            // What the callee requires is shown here, read against the actual arguments. So is
            // the invariant of each sealed value it receives, when this is the declaring file:
            // code outside it cannot hold a broken one.
            let own=self.contract.map(|c|c.module);
            let due=contract.requires.iter().cloned().chain(contract.assumes.iter().filter(|(module,_)|Some(*module)==own).map(|(_,p)|p.clone())).collect::<Vec<_>>();
            if !due.is_empty(){
                let old_entry=std::mem::replace(&mut self.entry,args.iter().cloned().enumerate().map(|(i,v)|(i as u32,v)).collect());let old_before=self.before.replace(s.clone());
                let parts=(||{let mut parts=vec![];for p in &due{parts.extend(self.flat(p,&s,None)?);}Ok(parts)})();
                self.entry=old_entry;self.before=old_before;
                for(text,index)in parts?{self.check(&s,text,index)?;}
            }
            if let Some(parameter) = contract.mutable_owner {
                let index = parameter as usize;
                let actual = args.get(index).ok_or_else(||error("record proof missing exclusive argument"))?;
                let old = super::record_post::owners(actual);
                if old.len() != 1 {
                    return Err(error("record proof models one allocation per exclusive replacement"));
                }
                let old_id = old[0].0;
                if self.live_focus(&s, old_id)?.element != element {
                    return Err(error("exclusive replacement element mismatch"));
                }
                let mut updated = s.clone();
                let replacement = self.entry_value(call.args[index].ty, &mut updated)?;
                let (next, _) = super::record_post::owner(&replacement)?;
                let said = self.ghost("Prop")?;
                let old_focus = updated.foci.get_mut(&old_id).expect("tracked exclusive argument");
                old_focus.live = "False".into();
                old_focus.moved = Some((said, next));
                updated.foci.get_mut(&next).expect("tracked replacement").element = element;
                let before = args.into_iter().enumerate().map(|(i, v)| (i as u32,v)).collect::<BTreeMap<_,_>>();
                let mut current = before.clone();
                current.insert(parameter, replacement.clone());
                let old_entry = std::mem::replace(&mut self.entry, before);
                let old_before = self.before.replace(s);
                let old_locals = self.locals.replace(current);
                let outcomes = (|| {
                    let arms: Vec<Ty> = self.types.result_sum(callee.result)
                        .map_or_else(|| vec![callee.result], |sum| sum.arms.to_vec());
                    let choice = (arms.len() > 1).then(|| self.ghost("Prop")).transpose()?;
                    let mut outcomes = Vec::new();
                    for (arm, ty) in arms.into_iter().enumerate() {
                        let mut after = updated.clone();
                        let selected = choice.as_ref().map(|name| {
                            after.path.push((name.clone(), arm == 0));
                            arm as u8
                        });
                        let payload = self.entry_value(ty, &mut after)?;
                        let result = selected.map(|arm| (arm, &payload));
                        let mut facts = Vec::new();
                        for (wanted, predicate) in contract.ensures.iter().map(|pair| (&pair.0, &pair.1))
                            .chain(contract.maintains.iter().map(|(a,_,p)| (a,p)))
                        {
                            if wanted.is_none() || *wanted == selected {
                                facts.extend(self.flat(predicate, &after, result)?);
                            }
                        }
                        self.imported_facts(&mut after, facts)?;
                        let answer = match selected {
                            Some(arm) => Symbol::Sum(arm, Box::new(payload)),
                            None => payload,
                        };
                        outcomes.push((after, answer));
                    }
                    Ok(outcomes)
                })();
                self.entry = old_entry;
                self.before = old_before;
                self.locals = old_locals;
                self.written = vec![(index, replacement)];
                self.imports.insert((call.definition.0, site));
                return outcomes;
            }
            // A verified callee that consumes at most one owner and returns a typed Result.
            // Its body proof closes every allocation except the one it returns, so the
            // consumed owner is dead afterwards and each arm's payload is a fresh symbolic
            // value constrained only by the callee's declared posts for that arm.
            if callee.result!=Ty::Unit{
                // A Result has two outcomes chosen by the callee; a plain owner result has one.
                let arms:Vec<Ty>=match self.types.result_sum(callee.result){Some(sum)=>sum.arms.to_vec(),None=>vec![callee.result]};let plain=arms.len()==1;
                // Every allocation the arguments own is consumed: the callee closes or returns it.
                let consumed=super::record_post::owners(&Symbol::Record(args.clone()));
                for(id,_)in &consumed{let focus=self.live_focus(&s,*id)?;if focus.element!=element{return Err(error("owned call actual element mismatch"));}}
                let snapshot=s;let choice=self.ghost("Prop")?;
                let old_entry=std::mem::replace(&mut self.entry,args.into_iter().enumerate().map(|(i,v)|(i as u32,v)).collect());let old_before=self.before.replace(snapshot.clone());
                self.importing=true;
                let outcomes=(||{let mut out=vec![];
                    for arm in 0..arms.len() as u8{
                        let mut after=snapshot.clone();if !plain{after.path.push((choice.clone(),arm==0));}
                        for(id,_)in &consumed{after.foci.get_mut(id).expect("focus").live="False".into();}
                        let first=self.next_focus;let payload=self.entry_value(arms[arm as usize],&mut after)?;
                        // The callee's result type is written over its own generic parameter; here it is the actual one.
                        for(id,focus)in after.foci.iter_mut(){if *id>=first&&matches!(focus.element,Ty::Param{..}){focus.element=element;}}
                        // Handing the owner on is not releasing it. Recorded before the callee's
                        // clauses are read, so its own `released` claim constrains `said`.
                        // With nothing owned in the payload, the callee's closure proof says each
                        // consumed allocation was released. Otherwise any may have come back: each
                        // gets a successor (the same field if the payload has it) and a fresh
                        // claim that only the callee's clauses or an equal key can establish.
                        let returned=super::record_post::owners(&payload);
                        for(id,path)in &consumed{
                            let Some(next)=returned.iter().find(|(_,p)|p==path).or(returned.first()).map(|(next,_)|*next) else{continue};
                            let said=self.ghost("Prop")?;after.foci.get_mut(id).expect("focus").moved=Some((said,next));
                        }
                        let mut facts=vec![];for(selected,p)in &contract.ensures{if selected.is_none()||*selected==Some(arm){facts.extend(self.flat(p,&after,Some((arm,&payload)))?);}}
                        for(selected,_,p)in &contract.maintains{if selected.is_none()||*selected==Some(arm){facts.extend(self.flat(p,&after,Some((arm,&payload)))?);}}
                        self.imported_facts(&mut after,facts)?;out.push((after,if plain{payload}else{Symbol::Sum(arm,Box::new(payload))}));
                    }Ok(out)})();
                self.importing=false;self.entry=old_entry;self.before=old_before;
                self.imports.insert((call.definition.0,site));return outcomes;
            }
            let (id,_)=super::record_post::owner(&Symbol::Record(args.clone()))?;let focus=self.live_focus(&s,id)?;if focus.element!=element{return Err(error("owned cleanup actual element mismatch"));}
            let snapshot=s.clone();let mut after=s;let fresh=self.ghost("Nat → Option α")?;let live=self.ghost("Prop")?;let f=after.foci.get_mut(&id).expect("focus");f.cells=fresh;f.live=live;
            let old_entry=std::mem::replace(&mut self.entry,args.into_iter().enumerate().map(|(i,v)|(i as u32,v)).collect());let old_before=self.before.replace(snapshot);
            let facts=(||{let mut facts=vec![];for(selected,p)in &contract.ensures{if selected.is_some(){return Err(error("owned Unit cleanup cannot use Result posts"));}facts.extend(self.flat(p,&after,None)?);}Ok(facts)})();self.entry=old_entry;self.before=old_before;let facts=facts?;
            self.imports.insert((call.definition.0,site));self.imported_facts(&mut after,facts)?;return Ok(vec![(after,Symbol::Unit)]);
        }
        self.check(&s,"False".into(),false)?;Ok(vec![])
    }
}
