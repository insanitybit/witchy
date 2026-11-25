//! One generic record-body proposition, with shared terms and guarded obligations.
use super::{logical_body::*,semantic_types::Types,record_contract::{RecordContract,Path,Root,Term,Predicate},record_state::State,record_value::Symbol,record_focus::Focus,record_caller::Caller,proof::{Goal,SourceIdentity},storage_contract::error};
use crate::diagnostic::Diagnostic;
use std::collections::{BTreeMap,BTreeSet};
type Result<T> = std::result::Result<T,Diagnostic>;
pub(super) const MODEL:&str="witchy.checked-storage-record.v5";
pub(super) const OPTION_MODEL:&str="witchy.checked-record-option-values.v1";
#[derive(Clone)]
pub(super) struct Check {pub path:Vec<(String,bool)>,pub text:String,pub index:bool}
struct Prefix {length:String,full:String,empty:String}
pub(super) struct Model<'a> {
    pub(super) types:&'a Types,pub(super) contract:Option<&'a RecordContract>,pub(super) entry:BTreeMap<u32,Symbol>,
    key_encoding:Option<String>,
    ghosts:Vec<(String,String)>,ranges:Vec<String>,validity:Vec<String>,pub caller:Option<Caller<'a>>,pub(super) next_focus:u32,requires:Vec<String>,prefixes:Vec<Prefix>,
    terms:Vec<(String,String,String,usize)>,checks:Vec<Check>,indexes:BTreeSet<String>,writes:BTreeSet<String>,nodes:usize,bytes:usize,expanded:usize,
    pub(super) checked_traps:BTreeSet<usize>,
    /// The `+` and `-` nodes (and `x += n` statements) a check of this body's proof shows in range.
    pub(super) shown_arithmetic:BTreeSet<usize>,
    seen_unchecked:BTreeSet<usize>,pub(super) before:Option<State>,pub sealed:super::record_post::OwnedPost,pub(super) local_prefixes:BTreeMap<String,String>,owned_paths:Vec<Vec<(String,bool)>>,pub(super) functions:Option<&'a[Function]>,pub(super) function:Option<FunctionId>,pub(super) imports:BTreeSet<(u32,u32)>,pub(super) modular:bool,pub(super) importing:bool,
    /// Quantified clauses about which cells hold a value, with the cells each one reads.
    pub(super) occupancy:BTreeMap<String,BTreeSet<String>>,
    /// While a callee's clauses are read: the arguments as they were before the call.
    pub(super) call_before:Option<BTreeMap<u32,Symbol>>,
    /// After a call: each borrowed record argument (by position) as the callee left it.
    pub(super) written:Vec<(usize,Symbol)>,
    /// The locals a loop invariant is being read against.
    pub(super) locals:Option<BTreeMap<u32,Symbol>>,result_type:Option<Ty>,
}
impl Model<'_> {
    pub fn count(&mut self)->Result<()> {self.nodes+=1;if self.nodes>4096{return Err(error("record contracts generated state/node complexity limit"));}Ok(())}
    fn bounded(&self,s:String)->Result<String>{if s.len()>super::MAX_SOURCE_BYTES{return Err(error("record contracts generated statement budget exceeded"));}Ok(s)}
    pub fn bind(&mut self,ty:&str,text:String)->Result<String> {
        self.count()?;let mut cost=1usize;
        for token in text.split(|c:char|!c.is_alphanumeric()&&c!='_') {if let Some((_,_,_,n))=self.terms.iter().find(|(name,_,_,_)|name==token){cost=cost.saturating_add(*n);}}
        self.expanded=self.expanded.saturating_add(cost);self.bytes=self.bytes.saturating_add(text.len());
        if self.expanded>4096||self.bytes>super::MAX_SOURCE_BYTES{return Err(error("record contracts shared term expansion limit"));}
        let name=format!("v{}",self.terms.len());self.terms.push((name.clone(),ty.into(),text,cost));Ok(name)
    }
    pub fn ghost(&mut self,ty:&str)->Result<String>{self.count()?;let name=format!("g{}",self.ghosts.len());self.ghosts.push((name.clone(),ty.into()));Ok(name)}
    pub fn key_word(&mut self,key:&str)->Result<String>{
        let encode=match &self.key_encoding {
            Some(encode)=>encode.clone(),
            None=>{let name=self.ghost("Key → Nat")?;self.key_encoding=Some(name.clone());name}
        };
        Ok(format!("({encode} {key})"))
    }
    pub fn interest(&mut self,i:&str)->Result<()> {self.count()?;self.indexes.insert(i.into());Ok(())}
    pub fn write_index(&mut self,i:&str)->Result<()> {self.count()?;self.writes.insert(i.into());Ok(())}
    pub fn check(&mut self,s:&State,text:String,index:bool)->Result<()> {
        for _ in 0..=s.path.len(){self.count()?;}let text=self.bounded(text)?;self.checks.push(Check{path:s.path.clone(),text,index});Ok(())
    }
    pub fn is_caller(&self)->bool{self.caller.is_some()}
    pub(super) fn element(&self)->Option<Ty>{self.contract.map(|c|c.element)}
    fn is_element(&self,ty:Ty)->bool{matches!(ty,Ty::Param{..})||self.contract.is_some_and(|c|c.element==ty)}
    pub fn seen_unchecked(&mut self,e:&Expr){self.seen_unchecked.insert(e as *const Expr as usize);}
    pub fn ranged_ghost(&mut self)->Result<String>{let n=self.ghost("Nat")?;self.ranges.push(n.clone());Ok(n)}
    pub fn focus<'s>(&self,s:&'s State,id:u32)->Result<&'s Focus>{let f=s.foci.get(&id).ok_or_else(||error("record caller missing allocation focus"))?;if !f.known||f.key.is_empty()||f.provenance=="unknown"||f.live=="False"{return Err(error("record caller requires known allocation history"));}Ok(f)}
    pub fn live_focus(&mut self,s:&State,id:u32)->Result<Focus>{let f=self.focus(s,id)?.clone();self.check(s,f.live.clone(),false)?;Ok(f)}
    pub fn needs(&self,body:&[Stmt])->bool{self.caller.as_ref().is_some_and(|c|!super::contract_sites::record_sites(body,c.functions).is_empty())}
    pub fn needs_value(&self,e:&Expr)->bool{self.caller.as_ref().is_some_and(|c|super::contract_sites::has_record(e,c.functions))}
    pub fn record_call(&self,call:&InstanceCall)->bool{self.caller.as_ref().is_some_and(|c|c.functions[call.definition.0 as usize].contract.as_ref().is_some_and(|c|c.record.is_some()))}
    pub fn allocation(&mut self,s:&mut State,buffer:u32,capacity:String)->Result<u32>{
        self.count()?;if !self.is_caller()&&self.contract.is_some_and(|c|self.types.buffers()[buffer as usize].element!=c.element){return Err(error("record allocation requires actual generic element"));}let id=self.next_focus;self.next_focus+=1;let key=self.ghost("Key")?;
        for other in s.foci.values(){s.path.push((format!("{} → {key} ≠ {}",other.live,other.key),true));}
        let cells=self.bind("Nat → Option α","(fun _ : Nat => (none : Option α))".into())?;
        s.foci.insert(id,Focus{element:self.types.buffers()[buffer as usize].element,key,capacity,cells,provenance:"allocation",known:true,live:"True".into(),moved:None});Ok(id)
    }
    pub fn element_option(&self,s:&State,value:&Symbol)->Result<String>{
        match value {
            Symbol::Element(v)=>Ok(format!("some {v}")),
            // A fixed element type is a bounded number; its stored form is the same option.
            Symbol::Nat(v) if self.contract.is_some_and(|c|!matches!(c.element,Ty::Param{..}))=>Ok(format!("some {v}")),
            Symbol::Stored(slot) if s.path.iter().any(|(p,b)|*b&&*p==format!("{slot} ≠ none"))=>Ok(slot.clone()),
            _=>Err(error("record element requires known nonempty optional-value history")),
        }
    }
    pub fn payload(&mut self,s:&State,e:&Expr,element:Ty)->Result<(String,Option<String>)>{
        if e.ty!=element{return Err(error("record Init payload element type mismatch"));}
        let value=match &e.kind{Value::ReadPlace(p)|Value::Move{place:p,..}=>super::record_state::read(s,p)?,Value::U64(_)|Value::Bool(_) if self.is_caller()=>Symbol::Unit,_=>return Err(error(if self.is_caller(){"record Init payload requires a typed nontrapping Copy read or caller literal"}else{"record contracts Init payload requires a nontrapping Copy parameter or field"}))};
        if matches!(value, Symbol::Element(_) | Symbol::Stored(_))
            || matches!(value, Symbol::Nat(_)) && self.contract.is_some_and(|c| !matches!(c.element, Ty::Param { .. }))
        {
            return Ok((self.element_option(s, &value)?, None));
        }
        if !self.is_caller()||matches!(value,Symbol::Unknown){return Err(error("record Init payload requires the generic element or known caller value"));}
        let slot=self.ghost("Option α")?;Ok((slot.clone(),Some(format!("{slot} ≠ none"))))
    }
    pub fn called(&mut self,mut s:State,call:&InstanceCall,args:Vec<Symbol>,needed_after:bool)->Result<Vec<(State,Symbol)>>{
        let functions=self.caller.as_ref().expect("caller context").functions;let callee=&functions[call.definition.0 as usize];
        if let Some(contract)=callee.contract.as_ref().and_then(|c|c.record.as_ref()){
            if call.substitutions.len()!=1{return Err(error("record caller requires one actual CopyStorage substitution"));}
            fn elements(v:&Symbol,s:&State,out:&mut Vec<Ty>){match v{Symbol::Buffer(id)=>if let Some(f)=s.foci.get(id){out.push(f.element);},Symbol::Record(fs)=>for v in fs{elements(v,s,out);},_=>{}}}
            // Every allocation the arguments own holds the call's element type, and is live.
            let mut actual=vec![];for v in &args{elements(v,&s,&mut actual);}if actual.is_empty()||actual.iter().any(|t|*t!=call.substitutions[0]){return Err(error("record caller actual allocation element/substitution mismatch"));}
            for (focus,_) in super::record_post::owners(&Symbol::Record(args.clone())){self.focus(&s,focus)?;}
            for p in &contract.requires{super::record_caller::supported(p)?;}
            let old=std::mem::replace(&mut self.entry,args.iter().cloned().enumerate().map(|(i,v)|(i as u32,v)).collect());
            // Requires refer to this call's entry, after all actual arguments,
            // including allocations that did not exist at caller entry.
            let old_before=self.before.replace(s.clone());
            let rendered:Result<Vec<Check>>=(||{let mut parts=vec![];for p in &contract.requires{for(text,index)in self.flat(p,&s,None)?{for _ in 0..=s.path.len(){self.count()?;}parts.push(Check{path:s.path.clone(),text,index});}}Ok(parts)})();self.entry=old;self.before=old_before;
            let parts=rendered?;let site=call.contract_site.ok_or_else(||error("record caller missing primary site"))?;
            let order=self.caller.as_ref().expect("caller").sites.values().filter_map(|s|s.order).max().map_or(0,|n|n+1);
            let entry=self.caller.as_mut().expect("caller").sites.get_mut(&site).ok_or_else(||error("record caller site absent from typed inventory"))?;
            if s.dependencies.contains(&site){return Err(error("owned post self or repeated-site prerequisite"));}
            entry.order.get_or_insert(order);
            entry.dependencies.extend(s.dependencies.iter().copied());
            entry.parts.extend(parts);entry.reached=true;
            if needed_after {
                let summary=self.caller.as_ref().expect("caller").summaries.get(&call.definition.0).cloned().ok_or_else(||error("owned post missing sealed body descriptor"))?;
                return self.outcomes(s,call,args,summary,site);
            }
        }
        else if needed_after{return Err(error("record caller cannot model unverified helper effects before a required site"));}
        for focus in s.foci.values_mut(){focus.known=false;focus.provenance="unknown";}Ok(vec![(s,Symbol::Unknown)])
    }
    fn fresh_owned(&mut self,ty:Ty,id:u32,s:&mut State)->Result<Symbol> {
        self.count()?;
        Ok(match ty {
            Ty::U64=>{let n=self.ghost("Nat")?;s.path.push((format!("{n} ≤ {}",u64::MAX),true));Symbol::Nat(n)},
            Ty::Param{..}=>{let slot=self.ghost("Option α")?;s.path.push((format!("{slot} ≠ none"),true));Symbol::Stored(slot)},
            Ty::Buffer(_)=>Symbol::Buffer(id),
            Ty::Aggregate(_)=>{
                let fields=self.types.get(ty).ok_or_else(||error("owned post missing typed record"))?.fields.iter().map(|f|f.ty).collect::<Vec<_>>();
                let mut values=vec![];for ty in fields{values.push(self.fresh_owned(ty,id,s)?);}Symbol::Record(values)
            },
            _=>return Err(error("owned post unsupported public result shape")),
        })
    }
    fn outcomes(&mut self,s:State,call:&InstanceCall,args:Vec<Symbol>,summary:super::record_post::OwnedPost,site:u32)->Result<Vec<(State,Symbol)>> {
        // The immutable snapshot is taken here, after every actual argument was
        // evaluated and after this site's complete precondition was recorded.
        let snapshot=s.clone();let (id,_)=super::record_post::owner(&Symbol::Record(args.clone()))?;
        self.focus(&snapshot,id)?;
        let functions=self.caller.as_ref().expect("caller").functions;
        let callee=&functions[call.definition.0 as usize];
        let arms=self.types.result_sum(callee.result).ok_or_else(||error("owned post requires typed Result"))?.arms;
        let contract=callee.contract.as_ref().and_then(|c|c.record.as_ref()).expect("record contract");
        let choice=self.ghost("Prop")?;let mut out=vec![];
        for arm in 0..2u8 {
            let mut after=s.clone();after.path.push((choice.clone(),arm==0));
            let payload=self.fresh_owned(arms[arm as usize],id,&mut after)?;
            let (_,path)=super::record_post::owner(&payload)?;
            if summary.arms.get(&arm)!=Some(&path){
                return Err(error("owned post arm lacks sealed actual return path"));
            }
            after.foci.get_mut(&id).expect("focus").cells=self.ghost("Nat → Option α")?;
            if arm==0||summary.returned_prefix(contract,arm) {
                let old_entry=std::mem::replace(&mut self.entry,args.iter().cloned().enumerate().map(|(i,v)|(i as u32,v)).collect());
                let old_before=self.before.replace(snapshot.clone());
                let rendered:Result<()>=(||{
                    for(selected,p)in &contract.ensures {
                        if selected.is_some_and(|a|a!=arm){continue;}
                        let mut projected=vec![];super::record_post::projection(p,&mut projected);
                        for p in projected {
                            let parts=self.flat(&p,&after,Some((arm,&payload)))?;
                            let length=if let Predicate::Prefix(_,n,_)=&p{Some(self.term(n,&after,Some((arm,&payload)))?)}else{None};
                            for(text,index)in parts {
                                if index{if let Some(n)=&length{self.local_prefixes.insert(text.clone(),n.clone());}}
                                after.path.push((text,true));self.count()?;
                            }
                        }
                    }Ok(())
                })();self.entry=old_entry;self.before=old_before;rendered?;
                self.owned_paths.push(after.path.clone());after.dependencies.insert(site);
            }else{
                let focus=after.foci.get_mut(&id).expect("focus");focus.known=false;focus.provenance="unknown";
            }
            out.push((after,Symbol::Sum(arm,Box::new(payload))));
        }
        Ok(out)
    }
    pub(super) fn entry_value(&mut self,t:Ty,s:&mut State)->Result<Symbol> {self.count()?;Ok(match t {
        Ty::Unsigned(width)=>{let name=self.ghost("Nat")?;if t==Ty::U64{self.ranges.push(name.clone());}else{self.validity.push(format!("{name} ≤ {}",width.layout().maximum()));}Symbol::Nat(name)},
        Ty::Bool=>Symbol::Prop(self.ghost("Prop")?),Ty::Unit=>Symbol::Unit,
        Ty::Param{..}=>Symbol::Element(self.ghost("α")?),
        Ty::Borrow{mutable:false,pointee:Pointee::Allocator}=>Symbol::Permission,
        Ty::Borrow{mutable:true,pointee} if matches!(pointee.ty(),Ty::Buffer(_) | Ty::Aggregate(_))=>self.entry_value(pointee.ty(),s)?,
        Ty::Borrow{mutable:false,pointee:Pointee::Slice(_)}=>Symbol::SliceLength(self.ranged_ghost()?),
        // Opaque: the body can pass it on to read-only helpers, and no clause can name what is behind it.
        Ty::Borrow{mutable:false,..} if !self.is_caller()=>Symbol::Permission,
        Ty::Allocator|Ty::Console|Ty::Input|Ty::Borrow{pointee:Pointee::Console|Pointee::Input,..} if self.is_caller()=>Symbol::Permission,
        Ty::Buffer(i)=>{let id=self.next_focus;self.next_focus+=1;let element=self.types.buffers()[i as usize].element;
            // Every additional owned buffer has its own entry state. Sharing the
            // first key and contents would identify distinct live allocations.
            let(key,capacity,cells)=if self.is_caller()||self.importing||id != 0{let k=self.ghost("Key")?;let c=self.ranged_ghost()?;let v=self.ghost("Nat → Option α")?;self.validity.push(format!("∀ j : Nat, {c} ≤ j → {v} j = none"));(k,c,v)}else{("key".into(),"cap".into(),"cells".into())};
            s.foci.insert(id,Focus{element,key,capacity,cells,provenance:"entry",known:true,live:"True".into(),moved:None});Symbol::Buffer(id)},
        Ty::Aggregate(_)=>{let fields=self.types.get(t).expect("record").fields.iter().map(|f|f.ty).collect::<Vec<_>>();let mut values=vec![];for t in fields{values.push(self.entry_value(t,s)?);}Symbol::Record(values)},
        _=>return Err(error("record caller unsupported entry type")),
    })}
    pub(super) fn path<'a>(&'a self,p:&Path,result:Option<(u8,&'a Symbol)>)->Result<&'a Symbol> {
        let value=match p.root {
            Root::Parameter(id)=>self.entry.get(&id),
            Root::Arm(arm)=>result.and_then(|(a,v)|(a==arm).then_some(v)),
            Root::Local(id)=>self.locals.as_ref().and_then(|locals|locals.get(&id)),
        }.and_then(|v|v.field(&p.fields)).ok_or_else(||error("record contracts missing actual ghost path"))?;Ok(value)
    }
    pub(super) fn focus_path(&self,p:&Path,s:&State,result:Option<(u8,&Symbol)>)->Result<Focus>{let Symbol::Buffer(id)=self.path(p,result)? else{return Err(error("record contracts ghost view is not an allocation focus"));};let view=if p.before{self.before.as_ref().unwrap_or(s)}else{s};Ok(self.focus(view,*id)?.clone())}
    pub(super) fn cells(&self,p:&Path,s:&State,result:Option<(u8,&Symbol)>)->Result<String>{let focus=self.focus_path(p,s,result)?;Ok(focus.cells)}
    pub(super) fn term(&mut self,t:&Term,s:&State,result:Option<(u8,&Symbol)>)->Result<String> {
        self.count()?;let text=match t {
            Term::Literal(n)=>format!("({n} : Nat)"),
            Term::Path(p)=>match self.path(p,result)?{Symbol::Nat(v) if p.ty==Ty::U64||matches!(p.ty,Ty::Unsigned(_))=>v.clone(),_=>return Err(error("record contracts typed U64 ghost path required"))},
            Term::Add(a,b)=>format!("({} + {})",self.term(a,s,result)?,self.term(b,s,result)?),
            Term::Capacity(p)=>self.focus_path(p,s,result)?.capacity,
            Term::Bound=>"j".into(),
        };self.bounded(text)
    }
    fn element_term(&mut self,t:&Term,s:&State,result:Option<(u8,&Symbol)>)->Result<String>{
        self.count()?;let Term::Path(p)=t else{return Err(error("record generic element requires a typed path"));};
        if !self.is_element(p.ty){return Err(error("record generic element path type mismatch"));}
        self.element_option(s,self.path(p,result)?)
    }
    pub(super) fn flat(&mut self,p:&Predicate,s:&State,result:Option<(u8,&Symbol)>)->Result<Vec<(String,bool)>> {
        self.count()?;
        Ok(match p {
            Predicate::Binary("∧",a,b)=>{let mut v=self.flat(a,s,result)?;v.extend(self.flat(b,s,result)?);v},
            Predicate::Forall(lo,hi,body)=>{let lo=self.term(lo,s,result)?;let hi=self.term(hi,s,result)?;let body=self.predicate(body,s,result)?;
                let text=format!("∀ j : Nat, ({lo} ≤ j ∧ j < {hi}) → {body}");
                // The cells whose occupancy the body states: a proof splits on whether each holds a value.
                let read=body.match_indices(" j ≠ none").chain(body.match_indices(" j = none")).filter_map(|(at,_)|body[..at].rsplit(|c:char|!c.is_alphanumeric()&&c!='_').next().filter(|t|!t.is_empty()).map(str::to_string)).collect();
                self.occupancy.insert(text.clone(),read);vec![(text,true)]
            },
            Predicate::Prefix(p,n,c)=>{
                let cells=self.cells(p,s,result)?;let cap=self.focus_path(p,s,result)?.capacity;let n=self.term(n,s,result)?;let c=self.term(c,s,result)?;
                vec![(format!("{cap} = {c}"),false),(format!("{n} ≤ {c}"),false),
                    (format!("∀ j : Nat, j < {n} → {cells} j ≠ none"),true),(format!("∀ j : Nat, {n} ≤ j → {cells} j = none"),true)]
            },
            Predicate::Unchanged(a,b)=>{let fa=self.focus_path(a,s,result)?;let fb=self.focus_path(b,s,result)?;vec![(format!("{} = {}",fa.key,fb.key),false),(format!("{} = {}",fa.capacity,fb.capacity),false),(format!("∀ j : Nat, {} j = {} j",fa.cells,fb.cells),true)]},
            Predicate::Except(a,b,i)=>{let i=self.term(i,s,result)?;self.interest(&i)?;let fa=self.focus_path(a,s,result)?;let fb=self.focus_path(b,s,result)?;
                vec![(format!("{} = {}",fa.key,fb.key),false),(format!("{} = {}",fa.capacity,fb.capacity),false),(format!("∀ j : Nat, j ≠ {i} → {} j = {} j",fa.cells,fb.cells),true)]},
            Predicate::EqualPrefix(a,b,n)=>{let fa=self.focus_path(a,s,result)?;let fb=self.focus_path(b,s,result)?;let n=self.term(n,s,result)?;vec![(format!("{n} ≤ {}",fa.capacity),false),(format!("{n} ≤ {}",fb.capacity),false),(format!("∀ j : Nat, j < {n} → {} j = {} j",fa.cells,fb.cells),true)]},
            _=>vec![(self.predicate(p,s,result)?,false)],
        })
    }
    pub(super) fn predicate(&mut self,p:&Predicate,s:&State,result:Option<(u8,&Symbol)>)->Result<String> {
        self.count()?;let text=match p {
            Predicate::Bool(b)=>if *b{"True"}else{"False"}.into(),
            Predicate::Compare(op,a,b)=>{
                if matches!(a,Term::Path(p) if self.is_element(p.ty)){
                    if !matches!(*op,"="|"≠"){return Err(error("record generic element comparison requires equality"));}
                    format!("({} {op} {})",self.element_term(a,s,result)?,self.element_term(b,s,result)?)
                }else{format!("({} {op} {})",self.term(a,s,result)?,self.term(b,s,result)?)}
            },
            Predicate::Not(p)=>format!("(¬ {})",self.predicate(p,s,result)?),
            Predicate::Binary(op,a,b)=>format!("({} {op} {})",self.predicate(a,s,result)?,self.predicate(b,s,result)?),
            Predicate::Empty(p,i)=>{let i=self.term(i,s,result)?;if i!="j"{self.interest(&i)?;}format!("({} {i} = none)",self.cells(p,s,result)?)},
            Predicate::Cell(p,i,item)=>{let i=self.term(i,s,result)?;if i!="j"{self.interest(&i)?;}let slot=self.element_term(item,s,result)?;format!("({slot} ≠ none ∧ {} {i} = {slot})",self.cells(p,s,result)?)},
            Predicate::Utf8Boundary(p,index)=>{
                let cells=self.cells(p,s,result)?;
                let index=self.term(index,s,result)?;
                format!("(¬ Witchy.Utf8.Continuation (({cells} {index}).getD 256))")
            },
            Predicate::Utf8(p,start,count)=>{
                let cells=self.cells(p,s,result)?;
                let start=self.term(start,s,result)?;
                let count=self.term(count,s,result)?;
                format!("(Witchy.Utf8.ValidCells {cells} {start} {count})")
            },
            Predicate::Same(a,b)=>{let a=self.focus_path(a,s,result)?;let b=self.focus_path(b,s,result)?;format!("({} = {})",a.key,b.key)},
            Predicate::Released(p)=>{let Symbol::Buffer(id)=self.path(p,result)? else{return Err(error("released requires allocation path"));};released(s,*id,0)?},
            _=>{let parts=self.flat(p,s,result)?;format!("({})",parts.into_iter().map(|(s,_)|format!("({s})")).collect::<Vec<_>>().join(" ∧ "))},
        };self.bounded(text)
    }
    fn requirements(&mut self,p:&Predicate,s:&State)->Result<()> {
        if let Predicate::Binary("∧",a,b)=p {self.requirements(a,s)?;return self.requirements(b,s);}
        let first=self.requires.len();let parts=self.flat(p,s,None)?;
        for(text,_)in parts{self.requires.push(self.bounded(text)?);}
        if let Predicate::Prefix(_,n,_)=p {
            let length=self.term(n,s,None)?;self.prefixes.push(Prefix{length,full:format!("hr{}",first+2),empty:format!("hr{}",first+3)});
        }Ok(())
    }
    /// The invariant clauses read against the locals of `s`.
    pub(super) fn invariant_parts(&mut self,s:&State,invariants:&[Predicate])->Result<Vec<(String,bool)>>{
        let previous=self.locals.replace(s.locals.clone());
        let parts=(||{let mut parts=vec![];for p in invariants{parts.extend(self.flat(p,s,None)?);}Ok(parts)})();
        self.locals=previous;parts
    }
    /// An arbitrary later value of the same shape. An owner keeps its place in the
    /// accounting but nothing is known of its allocation beyond the invariants.
    pub(super) fn arbitrary(&mut self,old:&Symbol,s:&mut State)->Result<Symbol>{self.count()?;Ok(match old{
        Symbol::Unit=>Symbol::Unit,Symbol::Permission=>Symbol::Permission,
        Symbol::Nat(_)=>Symbol::Nat(self.ranged_ghost()?),Symbol::Prop(_)=>Symbol::Prop(self.ghost("Prop")?),Symbol::Element(_)=>Symbol::Element(self.ghost("α")?),
        Symbol::Record(fields)=>{let mut values=vec![];for field in fields{values.push(self.arbitrary(field,s)?);}Symbol::Record(values)},
        Symbol::Buffer(id)=>{let key=self.ghost("Key")?;let capacity=self.ranged_ghost()?;let cells=self.ghost("Nat → Option α")?;self.validity.push(format!("∀ j : Nat, {capacity} ≤ j → {cells} j = none"));
            let focus=s.foci.get_mut(id).ok_or_else(||error("record loop owner missing allocation focus"))?;focus.key=key;focus.capacity=capacity;focus.cells=cells;focus.live="True".into();focus.moved=None;Symbol::Buffer(*id)},
        _=>return Err(error("record loops cannot reassign this kind of value")),
    })}
    pub fn returned(&mut self,s:&State,value:&Symbol)->Result<()> {
        if self.is_caller(){return Ok(());}
        let plain=self.result_type.is_some_and(|ty|ty!=Ty::Unit&&self.types.result_sum(ty).is_none());
        let result=match value{Symbol::Unit=>None,Symbol::Sum(arm,payload)=>Some((*arm,payload.as_ref())),owner if plain=>Some((0,owner)),_=>return Err(error("record contracts require actual Unit/owned Result return"))};
        // The allocations handed back: each must be live, and every other one released.
        let mut returned:Vec<u32>=if plain{let ids:Vec<u32>=super::record_post::owners(value).into_iter().map(|(id,_)|id).collect();if ids.is_empty(){return Err(error("record contracts require actual Unit/owned Result return"));}for id in &ids{self.focus(s,*id)?;}ids}else if let Some((arm,payload))=result{
            let arms=self.result_type.and_then(|ty|self.types.result_sum(ty)).ok_or_else(||error("record return requires typed Result"))?.arms;
            let ty=*arms.get(arm as usize).ok_or_else(||error("record return has invalid typed arm"))?;
            if ty==Ty::U64 {
                if !matches!(payload,Symbol::Nat(_)){return Err(error("record code arm requires actual U64 payload without owner"));}vec![]
            }else if ty==Ty::Unit {
                if !matches!(payload,Symbol::Unit){return Err(error("record unit arm requires an actual unit payload"));}vec![]
            }else{
                // Each actual returned owner must name a tracked allocation; the
                // closure checks below require precisely them to remain live.
                let ids:Vec<u32>=super::record_post::owners(payload).into_iter().map(|(id,_)|id).collect();
                if ids.is_empty(){return Err(error("record owned arm requires an actual allocation owner"));}
                for id in &ids{self.focus(s,*id)?;}ids
            }
        }else{vec![]};
        if let Some(id)=self.contract.and_then(|c|c.mutable_owner) {
            let actual=s.locals.get(&id).ok_or_else(||error("record proof lost its exclusive argument"))?;
            returned.extend(super::record_post::owners(actual).into_iter().map(|(focus,_)|focus));
            self.locals=Some(s.locals.clone());
        }
        for(id,focus)in &s.foci{self.check(s,if returned.contains(id){focus.live.clone()}else{format!("¬ {}",focus.live)},false)?;}
        if !self.modular {let Some((arm,payload))=result else{return Err(error("legacy owned post requires Result"));};let entry=Symbol::Record(self.entry.values().cloned().collect());let(original,_)=super::record_post::owner(&entry)?;if s.foci.len()!=1||!s.foci.contains_key(&original){return Err(error("owned post body has unmodeled allocation effects"));}self.sealed.returned(arm,payload,original)?;}
        for(selected,p)in self.contract.expect("body contract").ensures.clone(){if selected.is_none()||result.is_some_and(|(arm,_)|selected==Some(arm)){for(text,index)in self.flat(&p,s,result)?{self.check(s,text,index)?;}}}
        // In the file that declares a sealed type, every value handed back must satisfy its invariant.
        let c=self.contract.expect("body contract");
        for(selected,module,p)in c.maintains.clone(){if module==c.module&&(selected.is_none()||result.is_some_and(|(arm,_)|selected==Some(arm))){for(text,index)in self.flat(&p,s,result)?{self.check(s,text,index)?;}}}Ok(())
    }

}
/// The proposition that the allocation behind focus `id` no longer exists.
fn released(s:&State,id:u32,depth:usize)->Result<String>{
    let f=s.foci.get(&id).ok_or_else(||error("released missing tracked allocation"))?;
    let Some((said,next))=&f.moved else{return Ok(format!("(¬ {})",f.live));};
    if depth>64{return Err(error("record contracts ownership chain complexity limit"));}
    let successor=s.foci.get(next).ok_or_else(||error("released missing successor allocation"))?;
    Ok(format!("({said} ∨ (({} = {}) ∧ {}))",successor.key,f.key,released(s,*next,depth+1)?))
}
/// A character of an identifier, for `mentions` and `identifiers`.
fn word(c:char)->bool{c.is_alphanumeric()||c=='_'||c=='\''}
/// Whether `text` contains `name` as a whole identifier.
pub(super) fn mentions(text:&str,name:&str)->bool{
    text.match_indices(name).any(|(at,_)|!text[..at].chars().next_back().is_some_and(word)&&!text[at+name.len()..].chars().next().is_some_and(word))
}
/// The whole identifiers in `text` that are among `names`: for a name that is one identifier,
/// `mentions(text, name)` is whether it is among them.
fn named<'t>(text:&'t str,names:&'t BTreeSet<&str>)->impl Iterator<Item=&'t str>{text.split(|c:char|!word(c)).filter(|w|names.contains(w))}
fn append(out:&mut String,text:&str)->Result<()> {if out.len().saturating_add(text.len())>super::MAX_SOURCE_BYTES{return Err(error("record contracts generated statement/proof budget exceeded"));}out.push_str(text);Ok(())}
fn build_body<'a>(f:&Function,c:&'a RecordContract,types:&'a Types,functions:Option<&'a[Function]>)->Result<(Goal,super::record_post::OwnedPost,BTreeSet<(u32,u32)>,(bool,bool))> {
    let mut m=Model::new(types,Some(c),None,c.nodes);m.functions=functions;m.function=Some(f.definition);m.result_type=Some(f.result);
    // Legacy sealed reuse requires one entry owner and an owner in both arms.
    // Constructors and code-valued failure arms need resource closure only.
    m.modular=(f.result!=Ty::Unit&&types.result_sum(f.result).is_none())||!f.params.iter().any(|p|matches!(p,Ty::Buffer(_)|Ty::Aggregate(_)))||types.result_sum(f.result).is_some_and(|s|s.arms.contains(&Ty::U64)||s.arms.contains(&Ty::Unit))||f.result==Ty::Unit||!f.calls.is_empty()||f.params.iter().any(|p|matches!(p,Ty::Borrow{pointee:Pointee::Allocator,..}));
    let mut state=State{locals:BTreeMap::new(),foci:BTreeMap::new(),path:vec![],dependencies:BTreeSet::new()};m.enter(f,&mut state)?;
    for p in &c.requires{m.requirements(p,&state)?;}
    // A sealed value that arrives satisfies its type's invariant.
    for(_,p)in &c.assumes{m.requirements(p,&state)?;}
    let remaining=super::record_state::run(&mut m,&f.body,vec![state])?;
    if f.result==Ty::Unit {for s in remaining{m.returned(&s,&Symbol::Unit)?;}}else if !remaining.is_empty(){return Err(error("record contracts require every finite path to return"));}
    let dead=super::record_state::unchecked_nodes(&f.body).difference(&m.seen_unchecked).count();
    for _ in 0..dead{m.count()?;m.checks.push(Check{path:vec![("False".into(),true)],text:"False".into(),index:false});}
    if !traps_covered(&f.body, &m.checked_traps) {
        return Err(error("record contract has an explicit trap without a path obligation"));
    }
    let proved_traps = !m.checked_traps.is_empty();
    // Every unsigned `+ - *` of the body is one the proof walked and showed in range.
    let proved_arithmetic = super::record_state::arithmetic_nodes(&f.body).is_subset(&m.shown_arithmetic);
    let sealed=m.sealed.clone();let imports=m.imports.clone();let goal=m.finish(f,SourceIdentity{kind:"function_body",function:f.name.clone(),callee:None,site:None,storage:false,record:true,memory:false})?;Ok((goal,sealed,imports,(proved_traps,proved_arithmetic)))
}
pub(super) fn body(f:&Function,c:&RecordContract,types:&Types)->Result<(Goal,super::record_post::OwnedPost)>{let(goal,sealed,_,_)=build_body(f,c,types,None)?;Ok((goal,sealed))}
impl<'a> Model<'a>{
    pub fn new(types:&'a Types,contract:Option<&'a RecordContract>,caller:Option<Caller<'a>>,nodes:usize)->Self{Self{types,contract,entry:BTreeMap::new(),key_encoding:None,ghosts:vec![],ranges:vec![],validity:vec![],caller,next_focus:0,requires:vec![],prefixes:vec![],terms:vec![],checks:vec![],indexes:BTreeSet::new(),writes:BTreeSet::new(),nodes,bytes:0,expanded:0,checked_traps:BTreeSet::new(),shown_arithmetic:BTreeSet::new(),seen_unchecked:BTreeSet::new(),before:None,sealed:Default::default(),local_prefixes:BTreeMap::new(),occupancy:BTreeMap::new(),owned_paths:vec![],functions:None,function:None,imports:BTreeSet::new(),modular:false,importing:false,call_before:None,written:vec![],locals:None,result_type:None}}
    pub fn enter(&mut self,f:&Function,state:&mut State)->Result<()>{for(id,t)in f.params.iter().enumerate(){let v=self.entry_value(*t,state)?;self.entry.insert(id as u32,v);}state.locals=self.entry.clone();self.before=Some(state.clone());Ok(())}
    pub fn take_checks(&mut self,checks:Vec<Check>){self.checks=checks;}
    pub fn finish(&mut self,f:&Function,identity:SourceIdentity)->Result<Goal>{
    let m=self;
    let mut statement="∀ (α Key : Type) (key : Key) (cap : Nat) (cells : Nat → Option α) ".to_string();
    let mut intros="α Key key cap cells".to_string();
    // A ghost that no requirement, term, path or leaf mentions cannot matter to
    // the proof. Leaving it (and the facts about it alone) out states a stronger
    // theorem with a smaller context for every leaf.
    // Checks with one path share it: it is stated, and read, once.
    let mut groups:Vec<(&[(String,bool)],Vec<&Check>)>=vec![];
    for check in &m.checks{if let Some((_,leaves))=groups.iter_mut().find(|(path,_)|*path==check.path.as_slice()){leaves.push(check);}else{groups.push((&check.path,vec![check]));}}
    // Ghost and term names (`gN`, `vN`) are single identifiers, so a text mentions one when it
    // is among the text's identifiers: each text is read once, not once per name.
    let ghosts=m.ghosts.iter().map(|(n,_)|n.as_str()).collect::<BTreeSet<_>>();
    // The ghosts each validity fact names, read once for the statement and every path.
    let validity_ghosts=m.validity.iter().map(|p|named(p,&ghosts).collect::<Vec<_>>()).collect::<Vec<_>>();
    let used={
        let mut used=BTreeSet::new();
        for p in &m.requires{used.extend(named(p,&ghosts));}
        for(_,_,t,_)in &m.terms{used.extend(named(t,&ghosts));}
        for(path,leaves)in &groups{for(g,_)in *path{used.extend(named(g,&ghosts));}for c in leaves{used.extend(named(&c.text,&ghosts));}}
        loop{let before=used.len();
            for named_ghosts in &validity_ghosts{if named_ghosts.iter().any(|n|used.contains(n)){used.extend(named_ghosts);}}
            if used.len()==before{break;}}
        used
    };
    for(name,ty)in &m.ghosts{if !used.contains(name.as_str()){continue;}append(&mut statement,&format!("({name} : {ty}) "))?;intros.push_str(&format!(" {name}"));}
    append(&mut statement,&format!(", cap ≤ {} → (∀ j : Nat, cap ≤ j → cells j = none) → ",u64::MAX))?;intros.push_str(" hcap hout");
    for(i,n)in m.ranges.iter().enumerate(){if !used.contains(n.as_str()){continue;}append(&mut statement,&format!("{n} ≤ {} → ",u64::MAX))?;intros.push_str(&format!(" hb{i}"));}
    for(i,p)in m.validity.iter().enumerate(){if validity_ghosts[i].iter().all(|n|used.contains(n)){append(&mut statement,&format!("({p}) → "))?;intros.push_str(&format!(" hv{i}"));}}
    for(i,p)in m.requires.iter().enumerate(){append(&mut statement,&format!("({p}) → "))?;intros.push_str(&format!(" hr{i}"));}
    for(name,ty,text,_)in &m.terms{append(&mut statement,&format!("let {name} : {ty} := {text}; "))?;intros.push_str(&format!(" {name}"));}
    // Two facts can state one equality in both directions (a contract clause and a type
    // invariant, say). Rewriting with both never ends, and the second adds nothing, so it is
    // stated as True: the theorem proved is the stronger one.
    let sides=|guard:&str|{let t=guard.trim().trim_start_matches('(').trim_end_matches(')');let(a,b)=t.split_once(" = ")?;
        let atom=|x:&str|!x.is_empty()&&x.chars().all(|c|c.is_alphanumeric()||c=='_');(atom(a)&&atom(b)).then(||(a.to_string(),b.to_string()))};
    let assumed=m.requires.iter().filter_map(|p|sides(p)).collect::<Vec<_>>();
    let mirrors=|path:&[(String,bool)]|{let mut oriented=assumed.clone();let mut mirrored=BTreeSet::new();
        for(i,(guard,positive))in path.iter().enumerate(){if !*positive{continue;}if let Some((a,b))=sides(guard){if oriented.contains(&(b.clone(),a.clone())){mirrored.insert(i);}else{oriented.push((a,b));}}}mirrored};
    for(path,leaves)in &groups{append(&mut statement,"(")?;let mirrored=mirrors(path);for(i,(guard,positive))in path.iter().enumerate(){if mirrored.contains(&i){append(&mut statement,"(True) → ")?;continue;}append(&mut statement,&format!("({}{guard}) → ",if *positive{""}else{"¬ "}))?;}
        if leaves.len()==1{append(&mut statement,&leaves[0].text)?;}else{for check in leaves{append(&mut statement,&format!("({}) ∧ ",check.text))?;}append(&mut statement,"True")?;}append(&mut statement,") ∧ ")?;
    }append(&mut statement,"True")?;
    let unfold=m.terms.iter().map(|(n,_,_,_)|n.as_str()).collect::<Vec<_>>().join(", ");
    let mut prover=format!("  intros {intros}\n");
    if !unfold.is_empty(){append(&mut prover,&format!("  try dsimp only [{unfold}] at *\n"))?;}
    // Clauses that say exactly which cells hold a value, with the cells each reads at entry or
    // in a fresh allocation (not a shared term). A proof splits on whether such a cell holds a
    // value, then reads the shared terms through so the clause's instances and the goal agree.
    let term_names=m.terms.iter().map(|(n,_,_,_)|n.clone()).collect::<BTreeSet<_>>();
    let ghosts_and_terms=ghosts.iter().copied().chain(m.terms.iter().map(|(n,_,_,_)|n.as_str())).collect::<BTreeSet<_>>();
    let occupancy_requires=m.requires.iter().enumerate().filter_map(|(i,p)|m.occupancy.get(p).map(|c|(i,c.iter().filter(|c|!term_names.contains(*c)).cloned().collect::<BTreeSet<_>>()))).collect::<Vec<_>>();
    let unfold_all=if unfold.is_empty(){String::new()}else{format!("(try dsimp only [{unfold}] at *) <;> ")};
    append(&mut prover,"  refine ⟨")?;for _ in &groups{append(&mut prover,"?_, ")?;}append(&mut prover,"True.intro⟩\n")?;
    for(path,leaves)in &groups{
        append(&mut prover,"  · ")?;let names=(0..path.len()).map(|i|format!("h{i}")).collect::<Vec<_>>();
        if !names.is_empty(){append(&mut prover,&format!("intros {}\n    ",names.join(" ")))?;}
        // Each path uses only the ghosts of the calls it passed through. Facts
        // about the others would be scanned by every leaf's arithmetic search.
        let mut mentioned=BTreeSet::new();
        for(guard,_)in *path{mentioned.extend(named(guard,&ghosts_and_terms));}for check in leaves{mentioned.extend(named(&check.text,&ghosts_and_terms));}
        for(name,_,term,_)in m.terms.iter().rev(){if mentioned.contains(name.as_str()){mentioned.extend(named(term,&ghosts_and_terms));}}
        let idle_ghost=|n:&str|used.contains(n)&&!mentioned.contains(n);
        let mut idle=vec![];
        for(i,n)in m.ranges.iter().enumerate(){if idle_ghost(n){idle.push(format!("hb{i}"));}}
        for(i,named_ghosts)in validity_ghosts.iter().enumerate(){if !named_ghosts.is_empty()&&named_ghosts.iter().all(|n|idle_ghost(n)){idle.push(format!("hv{i}"));}}
        if !idle.is_empty(){append(&mut prover,&format!("clear {}\n    ",idle.join(" ")))?;}
        if leaves.len()>1{append(&mut prover,"refine ⟨")?;for _ in leaves{append(&mut prover,"?_, ")?;}append(&mut prover,"True.intro⟩\n")?;}
        let mut steps=vec![];
        let mirrored=mirrors(path);
        let names=names.into_iter().enumerate().filter(|(i,_)|!mirrored.contains(i)).map(|(_,n)|n).collect::<Vec<_>>();
        for check in leaves{let mut step=String::new();let names=names.clone();
        // What is to be shown is often a premise word for word: a type invariant carried through unchanged.
        if m.requires.contains(&check.text)||path.iter().enumerate().any(|(i,(guard,positive))|*positive&&!mirrored.contains(&i)&&*guard==check.text){steps.push("assumption\n".to_string());continue;}
        let prefix_equality = check.text.starts_with("∀ j : Nat, j < ") && check.text.contains(" j = ");
        if check.index{append(&mut step,"intro j\n    ")?;
            // A clause instance at `j` applies once its bound is shown from the goal's.
            if prefix_equality || !occupancy_requires.is_empty(){append(&mut step,"try intro hjbound\n    ")?;}
            for(r,_)in &occupancy_requires{append(&mut step,&format!("have hqj{r} := hr{r} j\n    try replace hqj{r} := hqj{r} (by witchy_inline_omega)\n    "))?;}}
        let mut indexes=m.indexes.clone();if check.index{indexes.insert("j".into());}
        for(p,prefix)in m.prefixes.iter().enumerate(){for(i,index)in indexes.iter().enumerate(){append(&mut step,&format!("have hf{p}_{i} := {} {index}\n    have he{p}_{i} := {} {index}\n    ",prefix.full,prefix.empty))?;}}
        for (p,(guard,_)) in check.path.iter().enumerate(){
            if guard.starts_with("∀ j : Nat,"){for(i,index)in indexes.iter().enumerate(){append(&mut step,&format!("have hp{p}_{i} := h{p} {index}\n    "))?;
                if prefix_equality && index == "j" {
                    append(&mut step,&format!("try have hpeq{p} := hp{p}_{i} (by witchy_inline_omega)\n    "))?;
                }
            }}
        }
        let mut cases=vec![];
        if check.index {
            for(i,index)in m.writes.iter().enumerate(){cases.push(format!("by_cases hj{i} : j = {index}"));}
            for(i,prefix)in m.prefixes.iter().enumerate(){cases.push(format!("by_cases hs{i} : j < {}",prefix.length));}
            for(i,(guard,_))in check.path.iter().enumerate(){if let Some(length)=m.local_prefixes.get(guard){cases.push(format!("by_cases hp{i} : j < {length}"));}}
            let mut occupied=occupancy_requires.iter().flat_map(|(_,c)|c.iter().cloned()).collect::<BTreeSet<_>>();
            if let Some(read)=m.occupancy.get(&check.text){occupied.extend(read.iter().filter(|c|!term_names.contains(*c)).cloned());}
            for(i,cells)in occupied.iter().enumerate(){cases.push(format!("by_cases hoc{i} : {cells} j = none"));}
        }
        // Splitting twice on one proposition only doubles the work.
        let mut split=BTreeSet::new();cases.retain(|case|split.insert(case.split_once(" : ").map(|(_,p)|p.to_string()).unwrap_or_else(||case.clone())));
        let branches=1usize.checked_shl(cases.len() as u32).unwrap_or(usize::MAX);if branches>4096{return Err(error("record contracts proof case complexity limit"));}
        for _ in 0..branches{m.nodes=m.nodes.saturating_add(1);}if m.nodes>4096{return Err(error("record contracts proof case complexity limit"));}
        if !cases.is_empty(){append(&mut step,&format!("{}\n    all_goals\n      ",cases.join(" <;> ")))?;}
        // Explicit path-name rewrites can contain both orientations of the same
        // owned-post equality. Let simp_all orient the local context consistently.
        let owned=m.owned_paths.iter().any(|path|check.path.starts_with(path));
        let mut rewrite=names;rewrite.extend((0..m.writes.len()).filter(|_|check.index).map(|i|format!("hj{i}")));
        let occupancy=!occupancy_requires.is_empty()||m.occupancy.contains_key(&check.text);
        let close=if owned{"(first | assumption | witchy_close)".to_string()}
            else if !rewrite.is_empty(){format!("(try simp only [{}]) <;> witchy_close",rewrite.join(", "))}
            else{"witchy_close".to_string()};
        let close=if !occupancy{close}else if check.index{
            // With the cell's occupancy and the written indexes decided, the goal and the clause's
            // instances at `j` reduce to arithmetic.
            let facts=cases.iter().filter_map(|c|c.strip_prefix("by_cases ").and_then(|c|c.split_once(" : ")).map(|(n,_)|n.to_string())).collect::<Vec<_>>();
            let mut instances=occupancy_requires.iter().map(|(r,_)|format!("hqj{r}")).collect::<Vec<_>>();
            // The path's own cell facts, put to use at `j`.
            if let Some(at)=indexes.iter().position(|n|n=="j"){for(p,(guard,_))in check.path.iter().enumerate(){if guard.starts_with("∀ j : Nat,"){instances.push(format!("hp{p}_{at}"));}}}
            let instances=instances.join(" ");
            let simp=if facts.is_empty(){super::memory_model::RECORD_OCCUPANCY_SIMP.to_string()}else{format!("{}, {}",facts.join(", "),super::memory_model::RECORD_OCCUPANCY_SIMP)};
            {
            // Each place separately: a shared script meets leaves where some instance is absent.
            // An instance whose bound is unknown is an implication, which omega does not read;
            // as a disjunction it does.
            let places=instances.split(' ').filter(|n|!n.is_empty()).map(|n|format!("(try replace {n} := Decidable.not_or_of_imp {n}) <;> (try simp only [{simp}] at {n}) <;> ")).collect::<String>();
            format!("{unfold_all}first | ({places}(try simp only [{simp}]) <;> witchy_inline_omega) | ({close}) | (simp_all <;> witchy_inline_omega)")}
        }else{
            // A fact about one cell follows from a clause instance once that cell's occupancy is known.
            let splits=indexes.iter().filter(|n|mentions(&check.text,n)||check.path.iter().any(|(g,_)|mentions(g,n))).flat_map(|n|{let n=n.clone();occupancy_requires.iter().flat_map(move |(r,c)|{let n=n.clone();c.iter().map(move |cells|format!("(try have hqd{r} := hr{r} {n} (by witchy_inline_omega)) <;> by_cases hoc : {cells} {n} = none <;> "))})}).collect::<String>();
            format!("first | ({close}) | ({splits}{unfold_all}witchy_close)")
        };
        // A fact about one cell of a filled prefix follows from the prefix once its bound is
        // shown by arithmetic. That is tried only when the direct search fails.
        let mut discharge=String::new();
        for(p,_)in m.prefixes.iter().enumerate(){for(i,index)in indexes.iter().enumerate(){
            // In a quantified post, the split on j versus the written cell may
            // establish the prefix bound only here, after introducing j.
            if check.index && index!="j" {continue;}
            discharge.push_str(&format!("(try have hfd{p}_{i} := hf{p}_{i} (by witchy_inline_omega)); (try have hed{p}_{i} := he{p}_{i} (by witchy_inline_omega)); "));}}
        let close = if prefix_equality {
            format!("first | ({close}) | (solve_by_elim [Eq.trans])")
        } else { close };
        let close = if check.text.contains("Witchy.Utf8.ValidCells") {
            let transfer = "subst_vars; apply Witchy.Utf8.ValidCells.copyPrefix <;> (first | assumption | (apply Witchy.Utf8.ValidCells.takeAtBoundary <;> (first | assumption | (intro h; simp_all) | witchy_inline_omega)) | (intro i hi; solve_by_elim))";
            let split = path.iter().enumerate().rfind(|(_, (guard, positive))| *positive && guard.contains(" ∨ "))
                .map(|(i, _)| format!(" | (rcases h{i} with hl | hr <;> (first | witchy_inline_omega | ({transfer})))"))
                .unwrap_or_default();
            let ascii = "apply Witchy.Utf8.ValidCells.appendAscii <;> (first | assumption | witchy_inline_omega)";
            format!("solve | ({close}) | ({ascii}) | ({transfer}){split}")
        } else { close };
        if discharge.is_empty(){append(&mut step,&format!("{close}\n"))?;}else{append(&mut step,&format!("first | ({close}) | ({discharge}witchy_close)\n"))?;}

            steps.push(step);
        }
        let mut distinct=vec![];for step in &steps{if !distinct.contains(&step){distinct.push(step);}}
        // Cheapest first: a premise taken as is, then scripts that begin by introducing the
        // cell index (they stop at once on a leaf that has none), then the general search.
        distinct.sort_by_key(|step|if step.as_str()=="assumption\n"{0}else if step.starts_with("intro j"){1}else{2});
        // Share repeated proof scripts, never propositions or audit entries.
        // `solve` requires a candidate to close its entire leaf; another script
        // is tried only on tactic failure. Every original leaf is kernel checked.
        if steps.len()>1&&distinct.len()<steps.len(){
            append(&mut prover,"    all_goals\n      first\n")?;
            for step in distinct{append(&mut prover,"      | solve\n        | ")?;append(&mut prover,&step.replace("\n    ","\n          "))?;}
        }else{
            for step in steps{if leaves.len()==1{append(&mut prover,&step)?;}else{append(&mut prover,"    · ")?;append(&mut prover,&step.replace("\n    ","\n      "))?;}}
        }
    }
    // With a fixed element type the cells hold numbers; there is no type to abstract over.
    let (statement,prover)=if m.contract.is_some_and(|c|!matches!(c.element,Ty::Param{..})){
        (statement.replacen("∀ (α Key : Type)","∀ (Key : Type)",1).replace('α',"Nat"),prover.replacen("intros α ","intros ",1))
    }else{(statement,prover)};
    Ok(Goal{source_name:f.name.clone(),statement,nodes:m.nodes,source:Some(identity),prover:Some(prover)})
}
}
/// The body goals of the functions with record contracts, and what callers and the check of
/// imported helpers need from them. `None`: there are none.
pub(super) struct Bodies{proved_traps:BTreeSet<u32>,goals:Vec<Goal>,summaries:BTreeMap<u32,super::record_post::OwnedPost>,imports:BTreeMap<u32,BTreeSet<(u32,u32)>>}
pub(super) fn bodies(program:&Program)->Result<Option<Bodies>> {
    if !program.functions.iter().any(|f|f.contract.as_ref().is_some_and(|c|c.record.is_some())){return Ok(None);}
    let mut goals=vec![];let mut summaries=BTreeMap::new();let mut imports=BTreeMap::new();
    // Each body is built from the program alone, independently of the others, so they are
    // built on several threads and then taken in order: the result is the same as one by one.
    let work=program.functions.iter().filter_map(|f|f.contract.as_ref().and_then(|c|c.record.as_ref()).map(|c|(f,c))).collect::<Vec<_>>();
    let mut proved_traps = BTreeSet::new();
    let built=super::parallel::map(&work,|(f,c)|build_body(f,c,&program.types,Some(&program.functions)));
    for((f,_),body)in work.iter().zip(built){
        let(goal,sealed,used,traps)=body.map_err(|e|super::proof_advice::at_function(e,f))?;
        if traps.0 { proved_traps.insert(f.definition.0 | super::scalar_vc::UNREACHABLE_PROVED); }
        if traps.1 { proved_traps.insert(f.definition.0); }
        goals.push(goal);if !sealed.arms.is_empty(){summaries.insert(f.definition.0,sealed);}imports.insert(f.definition.0,used);
    }
    Ok(Some(Bodies{proved_traps,goals,summaries,imports}))
}
/// The record model's goals: the bodies, then each call site's. `memory` is the memory model's
/// result for the same program, which imported helpers are checked against; the caller works it
/// out once.
pub(super) fn finish(program:&Program,bodies:Result<Option<Bodies>>,memory:&Result<Vec<Goal>>)->Result<super::proof_body::Prepared> {
    let Some(Bodies{proved_traps,mut goals,summaries,imports})=bodies? else{return Ok(Default::default())};
    let mut prepared=super::record_caller::prepare(program,&summaries,&goals)?;goals.append(&mut prepared.goals);
    if imports.values().any(|used|!used.is_empty()){
        // Validate current complete dependency membership. source_vc subsequently
        // puts these same current memory goals in the one atomic kernel batch.
        let memory=memory.as_ref().map_err(Clone::clone)?;
        let all=goals.iter().chain(memory).collect::<Vec<_>>();
        for(owner,used)in &imports{for(callee,site)in used{
            let from=&program.functions[*owner as usize];let target=&program.functions[*callee as usize];
            if !all.iter().any(|g|g.source.as_ref().is_some_and(|s|s.kind=="function_body"&&s.function==target.name))||
               !all.iter().any(|g|g.source.as_ref().is_some_and(|s|s.kind=="call_precondition"&&s.function==from.name&&s.callee.as_deref()==Some(target.name.as_str())&&s.site==Some(*site))){return Err(error("owned helper dependency missing current body/site proof"));}
        }}
        fn visit(id:u32,edges:&BTreeMap<u32,BTreeSet<(u32,u32)>>,gray:&mut BTreeSet<u32>,black:&mut BTreeSet<u32>)->Result<()>{
            if black.contains(&id){return Ok(());}if !gray.insert(id){return Err(error("owned helper post import cycle"));}
            if let Some(next)=edges.get(&id){for(callee,_)in next{visit(*callee,edges,gray,black)?;}}gray.remove(&id);black.insert(id);Ok(())
        }
        let mut gray=BTreeSet::new();let mut black=BTreeSet::new();for id in imports.keys(){visit(*id,&imports,&mut gray,&mut black)?;}
    }
    prepared.proved_arithmetic.extend(proved_traps);
    prepared.goals=goals;Ok(prepared)
}
