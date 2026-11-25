//! Bounded shared/exclusive memory VCs from primary typed IR; Source Flow remains sole borrow authority.
use super::scalar::Scalar;
use super::{logical_body::*,logical_place::{Place,Projection},memory_contract::{MemoryContract,Predicate,Number,Element,Path},storage_contract::error,proof::{Goal,SourceIdentity}};
use crate::diagnostic::Diagnostic;
use std::collections::{BTreeMap,BTreeSet};
type Result<T>=std::result::Result<T,Diagnostic>;
pub(super) const MODEL:&str="witchy.proof-required-memory.v5";
#[derive(Clone)]
/// `Slice` carries a count and, where modeled, a snapshot of its cells.
enum Symbol{Unit,Nat(String),Prop(String),Slot(String),Buffer(u32),Record(Vec<Self>),Sum(u8,Box<Self>),Slice(String, Option<String>)}
impl Symbol{fn field(&self,fields:&[usize])->Option<&Self>{let mut v=self;for i in fields{let Self::Record(fs)=v else{return None;};v=fs.get(*i)?;}Some(v)}fn store(&mut self,fields:&[usize],value:Self)->Option<()>{if let Some((i,rest))=fields.split_first(){let Self::Record(fs)=self else{return None;};fs.get_mut(*i)?.store(rest,value)}else{*self=value;Some(())}}}
#[derive(Clone)]
struct State{locals:BTreeMap<u32,Symbol>,path:Vec<String>,cells:BTreeMap<u32,String>}
#[derive(Clone)]
/// `tactic`: a step that shows this check by itself (a lemma of the prelude), for a fact the
/// general closing steps cannot reach; it is still a check, proved like any other.
struct Check{path:Vec<String>,text:String,index:bool,tactic:Option<String>}
#[derive(Default)]
struct Inventory{unsafe_nodes:BTreeSet<usize>,loop_nodes:BTreeSet<usize>,calls:BTreeMap<u32,FunctionId>,
    /// Every unsigned `+ - *` of the body, and whether a statement does arithmetic as it
    /// stores (`x += n`): what `prepare_proved` holds against the nodes the model showed.
    arithmetic:BTreeSet<usize>,stores_arithmetic:bool}
impl Inventory{
    fn place(&mut self,p:&Place,program:&Program){for p in &p.projections{if let Projection::Index{value,..}=p{self.value(value,program);}}}
    fn value(&mut self,e:&Expr,program:&Program){match &e.kind{
        Value::Call(c)=>{for a in &c.args{self.value(a,program);}if program.functions[c.definition.0 as usize].contract.as_ref().is_some_and(|c|c.memory.is_some()){self.calls.insert(c.contract_site.expect("primary contracted site"),c.definition);}},
        Value::Storage{op,receiver,index,value,..}=>{self.value(receiver,program);self.value(index,program);if let Some(v)=value{self.value(v,program);}if op.proof_admitted(){self.unsafe_nodes.insert(e as *const Expr as usize);}},
        Value::ReadPlace(p)|Value::Address(p)|Value::Move{place:p,..}|Value::Extract{place:p,..}=>self.place(p,program),
        Value::Stored(p,v)=>{self.value(v,program);self.place(p,program)},
        Value::TryAlloc{allocator,capacity,..}=>{self.value(allocator,program);self.value(capacity,program);},
        Value::Binary(op,a,b) if matches!(op,Binary::Add|Binary::Sub|Binary::Mul)=>{self.arithmetic.insert(e as *const Expr as usize);self.value(a,program);self.value(b,program);},
        Value::Binary(_,a,b)|Value::Index(a,b,_)|Value::SliceAt(a,b,_)|Value::SliceIndex(a,b,_)|Value::SliceCopy(a,b)|Value::SliceReadLe64(a,b)=>{self.value(a,program);self.value(b,program);},
        Value::SliceOf(p,_)=>self.place(p,program),Value::SliceLen(v)=>self.value(v,program),
        Value::SlicePart(a,b,c)|Value::SliceWithin(a,b,c)=>{self.value(a,program);self.value(b,program);self.value(c,program);},
        Value::AllocationKey(v,_)|Value::Capacity(v,_)|Value::TryRelease(v,_)|Value::Discard(v)|Value::Sum{payload:v,..}|Value::Repeat(v,_)|Value::Retag(v,_)|Value::Project(v,_)|Value::Widen(v)|Value::ProofUtf8(v)|Value::Not(v)=>self.value(v,program),
        Value::Aggregate(fs)|Value::Signed(_,fs)|Value::Float(_,fs) | Value::Bits(_, fs) | Value::BoxOp(_, _, fs)=>for v in fs{self.value(v,program);},Value::Print(p,v,_)=>{self.place(p,program);self.value(v,program);},Value::Read(p,_)=>self.place(p,program),Value::Host(_,p,vs)=>{if let Some(p)=p{self.place(p,program);}for v in vs{self.value(v,program);}},Value::Exchange(a,b)=>{self.place(a,program);self.place(b,program);},
        Value::TaskSpawn{allocator,bytes,entry}=>{self.value(allocator,program);self.value(bytes,program);self.value(entry,program);},
        Value::TaskStep(v,w)=>{self.value(v,program);self.value(w,program);},
        Value::TaskFinish(v)|Value::TaskStatus(v)|Value::Pause(v)=>self.value(v,program),
        Value::StaticStr(_) | Value::Unit|Value::U64(_)|Value::Bool(_)|Value::EntryConsole|Value::EntryInput|Value::EntryAllocator|Value::TaskWord|Value::InTask|Value::EntryHost(..)=>{},
    }}
    fn body(&mut self,body:&[Stmt],program:&Program){for s in body{match s{
        Stmt::Set(_,v)|Stmt::Eval(v)|Stmt::Return(v, _, _)=>self.value(v,program),Stmt::StorePlace(p,op,v)=>{self.stores_arithmetic|=op.is_some();self.place(p,program);self.value(v,program);},Stmt::Replace(_,p,v)=>{self.place(p,program);self.value(v,program);},
        Stmt::If(c,a,b)=>{self.value(c,program);self.body(a,program);self.body(b,program);},Stmt::While(c,a,invariants)=>{if !invariants.memory.is_empty(){self.loop_nodes.insert(s as *const Stmt as usize);}self.value(c,program);self.body(a,program);},
        Stmt::Match{value,arms}=>{self.value(value,program);for a in arms{self.body(&a.body,program);}},Stmt::Expire(_)|Stmt::Break|Stmt::Trap(_)|Stmt::Continue=>{},
    }}}
}
#[derive(Clone)]
struct Focus {key:String,cap:String,entry:String,mutable:bool}
/// What reduces an occupancy clause once a cell's contents and the written index are decided.
/// Only equations: a lemma stated with `↔` makes `simp` add an auxiliary theorem, which the
/// proof audit refuses.
const OCCUPANCY_SIMP:&str="ite_true, ite_false, ne_eq, not_true_eq_false, not_false_eq_true, reduceCtorEq, and_true, and_false, true_and, false_and, iff_true, iff_false, true_iff, false_iff";
/// The same, for the record model, whose instances also become disjunctions.
pub(super) const RECORD_OCCUPANCY_SIMP:&str="eq_self, ite_true, ite_false, ne_eq, not_true_eq_false, not_false_eq_true, reduceCtorEq, and_true, and_false, true_and, false_and, or_true, or_false, true_or, false_or, iff_true, iff_false, true_iff, false_iff";
struct Model<'a>{program:&'a Program,function:&'a Function,contract:&'a MemoryContract,ghosts:Vec<(String,&'static str)>,ranges:Vec<String>,terms:Vec<(String,&'static str,String,usize)>,checks:Vec<Check>,calls:BTreeMap<u32,Vec<Check>>,seen:BTreeSet<usize>,seen_loops:BTreeSet<usize>,nodes:usize,bytes:usize,expanded:usize,requires:Vec<String>,entry:BTreeMap<u32,Symbol>,foci:Vec<Focus>,indices:BTreeSet<String>,quantified_bounds:BTreeMap<String,String>,empty_quantifiers:BTreeSet<String>,uses_keys:bool,
    /// Quantified clauses about which cells hold a value, with the cells each one reads.
    occupancy:BTreeMap<String,BTreeSet<String>>,in_loop:bool,
    /// The `+` and `-` nodes whose result a check of this proof shows in range.
    shown:BTreeSet<usize>}
impl Model<'_>{
    fn count(&mut self)->Result<()>{self.nodes+=1;if self.nodes>4096{return Err(error("memory proof node/state complexity limit"));}Ok(())}
    fn ghost(&mut self,ty:&'static str)->Result<String>{self.count()?;let n=format!("g{}",self.ghosts.len());self.ghosts.push((n.clone(),ty));Ok(n)}
    fn bind(&mut self,ty:&'static str,text:String)->Result<String>{self.count()?;let mut cost=1usize;for token in text.split(|c:char|!c.is_alphanumeric()&&c!='_'){if let Some((_,_,_,n))=self.terms.iter().find(|(n,_,_,_)|n==token){cost=cost.saturating_add(*n);}}self.expanded=self.expanded.saturating_add(cost);self.bytes=self.bytes.saturating_add(text.len());if self.expanded>4096||self.bytes>super::MAX_SOURCE_BYTES{return Err(error("memory proof shared term complexity limit"));}let n=format!("v{}",self.terms.len());self.terms.push((n.clone(),ty,text,cost));Ok(n)}
    fn entry(&mut self,t:Ty,mutable:bool)->Result<Symbol>{self.count()?;Ok(match t{
        Ty::Borrow{pointee,mutable}=>self.entry(pointee.ty(),mutable)?,
        Ty::Slice(id) => {
            let length = self.ghost("Nat")?;
            self.ranges.push(length.clone());
            let contents = if !mutable && self.program.types.slice_element(id) == Ty::Unsigned(Scalar::U8) {
                let values = self.ghost("Nat → Nat")?;
                let cells = format!("(fun k : Nat => some ({values} k))");
                if self.program.types.is_str(id) {
                    self.requires.push(format!("Witchy.Utf8.ValidCells {cells} 0 {length}"));
                }
                Some(cells)
            } else {
                None
            };
            Symbol::Slice(length, contents)
        },
        Ty::Buffer(_)=>{let id=self.foci.len() as u32;let suffix=if id==0{String::new()}else{id.to_string()};self.foci.push(Focus{key:format!("key{suffix}"),cap:format!("cap{suffix}"),entry:format!("cells{suffix}"),mutable});Symbol::Buffer(id)},
        Ty::U64=>{let n=self.ghost("Nat")?;self.ranges.push(n.clone());Symbol::Nat(n)},Ty::Bool=>Symbol::Prop(self.ghost("Prop")?),
        Ty::Param{..}=>Symbol::Slot(format!("some {}",self.ghost("α")?)),
        // A fixed element type: some stored number.
        t if t==self.contract.element=>Symbol::Nat(self.ghost("Nat")?),
        Ty::Aggregate(_)=>{let fields=self.program.types.get(t).ok_or_else(||error("memory proof missing record type"))?.fields.iter().map(|f|f.ty).collect::<Vec<_>>();let mut vs=vec![];for t in fields{vs.push(self.entry(t,mutable)?);}Symbol::Record(vs)},
        _=>return Err(error("memory proof unsupported entry type")),
    })}
    fn entry_cells(&self)->BTreeMap<u32,String>{self.foci.iter().enumerate().map(|(i,f)|(i as u32,f.entry.clone())).collect()}
    fn path<'s>(&self,p:&Path,locals:&'s BTreeMap<u32,Symbol>,before:&'s BTreeMap<u32,Symbol>)->Result<&'s Symbol>{let locals=if p.before{before}else{locals};locals.get(&p.root).and_then(|v|v.field(&p.fields)).ok_or_else(||error("memory proof missing actual typed path"))}
    fn number(&mut self,n:&Number,locals:&BTreeMap<u32,Symbol>,before:&BTreeMap<u32,Symbol>)->Result<String>{self.count()?;Ok(match n{Number::Literal(n)=>format!("({n} : Nat)"),Number::Path(p)=>match self.path(p,locals,before)?{Symbol::Slice(length,_)=>length.clone(),value=>nat(value)?.into()},Number::AllocationKey(p)=>self.key_word(self.buffer(p,locals,before)?),Number::Capacity(p)=>self.foci[self.buffer(p,locals,before)? as usize].cap.clone(),Number::Add(a,b)=>format!("({} + {})",self.number(a,locals,before)?,self.number(b,locals,before)?),Number::Bound=>"j".into()})}
    fn buffer(&self,p:&Path,locals:&BTreeMap<u32,Symbol>,before:&BTreeMap<u32,Symbol>)->Result<u32>{if let Symbol::Buffer(id)=self.path(p,locals,before)?{Ok(*id)}else{Err(error("memory proof expected borrowed Buffer focus"))}}
    fn cells(&self,p:&Path,locals:&BTreeMap<u32,Symbol>,cells:&BTreeMap<u32,String>,before:&BTreeMap<u32,Symbol>,old_cells:&BTreeMap<u32,String>)->Result<String>{let id=self.buffer(p,locals,before)?;(if p.before{old_cells}else{cells}).get(&id).cloned().ok_or_else(||error("memory proof missing focused cells"))}
    fn flat(&mut self,p:&Predicate,locals:&BTreeMap<u32,Symbol>,cells:&BTreeMap<u32,String>,before:&BTreeMap<u32,Symbol>,old_cells:&BTreeMap<u32,String>,result:Option<&Symbol>)->Result<Vec<(String,bool)>>{
        if let Predicate::Binary("∧",a,b)=p{let mut out=self.flat(a,locals,cells,before,old_cells,result)?;out.extend(self.flat(b,locals,cells,before,old_cells,result)?);return Ok(out);}
        if let Predicate::SamePrefix(a,b,n)=p{let ca=self.cells(a,locals,cells,before,old_cells)?;let cb=self.cells(b,locals,cells,before,old_cells)?;let i=self.number(n,locals,before)?;let cap_a=self.foci[self.buffer(a,locals,before)? as usize].cap.clone();let cap_b=self.foci[self.buffer(b,locals,before)? as usize].cap.clone();let text=format!("∀ j : Nat, j < {i} → {ca} j = {cb} j ∧ {cb} j ≠ none");self.quantified_bounds.insert(text.clone(),i.clone());return Ok(vec![(format!("{i} ≤ {cap_a}"),false),(format!("{i} ≤ {cap_b}"),false),(text,true)]);}
        if let Predicate::SliceContents(slice, buffer, start) = p {
            let Symbol::Slice(length, Some(contents)) = self.path(slice, locals, before)? else {
                return Err(error("memory.slice_contents requires a slice with modeled cells"));
            };
            let length = length.clone();
            let contents = contents.clone();
            let buffer = self.cells(buffer, locals, cells, before, old_cells)?;
            let start = self.number(start, locals, before)?;
            let text = format!("∀ j : Nat, j < {length} → {contents} j = {buffer} ({start} + j)");
            self.quantified_bounds.insert(text.clone(), length);
            return Ok(vec![(text, true)]);
        }
        let empty=match p{
            Predicate::EmptyRange(b,lo,hi)=>{let c=self.cells(b,locals,cells,before,old_cells)?;let lo=self.number(lo,locals,before)?;let hi=self.number(hi,locals,before)?;let text=format!("∀ j : Nat, ({lo} ≤ j ∧ j < {hi}) → {c} j = none");self.quantified_bounds.insert(text.clone(),hi);Some(text)},
            Predicate::EmptyFrom(b,lo)=>{let c=self.cells(b,locals,cells,before,old_cells)?;let lo=self.number(lo,locals,before)?;let text=format!("∀ j : Nat, {lo} ≤ j → {c} j = none");self.quantified_bounds.insert(text.clone(),lo);Some(text)},
            Predicate::EmptyAll(b)=>{let c=self.cells(b,locals,cells,before,old_cells)?;Some(format!("∀ j : Nat, {c} j = none"))},_=>None,
        };
        if let Some(text)=empty{self.empty_quantifiers.insert(text.clone());return Ok(vec![(text,true)]);}
        if let Predicate::Forall(lo,hi,body)=p{let lo=self.number(lo,locals,before)?;let hi=self.number(hi,locals,before)?;let body=self.predicate(body,locals,cells,before,old_cells,result)?;
            let text=format!("∀ j : Nat, ({lo} ≤ j ∧ j < {hi}) → {body}");self.quantified_bounds.insert(text.clone(),hi);
            // The cells whose occupancy the body states: a proof splits on whether each holds a value.
            let read=body.match_indices(" j ≠ none").chain(body.match_indices(" j = none")).filter_map(|(at,_)|body[..at].rsplit(|c:char|!c.is_alphanumeric()&&c!='_').next().filter(|t|!t.is_empty()).map(str::to_string)).collect();
            self.occupancy.insert(text.clone(),read);return Ok(vec![(text,true)]);}
        if let Predicate::FullRange(b,lo,hi)=p{let c=self.cells(b,locals,cells,before,old_cells)?;let lo=self.number(lo,locals,before)?;let hi=self.number(hi,locals,before)?;let text=format!("∀ j : Nat, ({lo} ≤ j ∧ j < {hi}) → {c} j ≠ none");self.quantified_bounds.insert(text.clone(),hi);return Ok(vec![(text,true)]);}
        let pair=match p{Predicate::Except(a,b,_)|Predicate::Unchanged(a,b)|Predicate::From(a,b,_)=>Some((a,b)),_=>None};
        if let Some((a,b))=pair{let ia=self.buffer(a,locals,before)?;let ib=self.buffer(b,locals,before)?;let ca=self.cells(a,locals,cells,before,old_cells)?;let cb=self.cells(b,locals,cells,before,old_cells)?;let text=match p{Predicate::Except(_,_,n)=>format!("∀ j : Nat, j ≠ {} → {ca} j = {cb} j",self.number(n,locals,before)?),Predicate::From(_,_,n)=>format!("∀ j : Nat, {} ≤ j → {ca} j = {cb} j",self.number(n,locals,before)?),_=>format!("∀ j : Nat, {ca} j = {cb} j")};if let Predicate::From(_,_,n)=p{let n=self.number(n,locals,before)?;self.quantified_bounds.insert(text.clone(),n);}return Ok(vec![(format!("({} = {})",self.foci[ia as usize].key,self.foci[ib as usize].key),false),(format!("({} = {})",self.foci[ia as usize].cap,self.foci[ib as usize].cap),false),(text,true)]);}
        // A cell clause is two facts: the value is one, and the cell holds it. Apart, each is
        // usually a premise word for word.
        if let Predicate::Cell(..)=p{let text=self.predicate(p,locals,cells,before,old_cells,result)?;
            if let Some((holds,stored))=text.strip_prefix('(').and_then(|t|t.strip_suffix(')')).and_then(|t|t.split_once(" ≠ none ∧ ")){return Ok(vec![(format!("{holds} ≠ none"),false),(stored.to_string(),false)]);}
            return Ok(vec![(text,false)]);}
        Ok(vec![(self.predicate(p,locals,cells,before,old_cells,result)?,false)])
    }
    fn predicate(&mut self,p:&Predicate,locals:&BTreeMap<u32,Symbol>,cells:&BTreeMap<u32,String>,before:&BTreeMap<u32,Symbol>,old_cells:&BTreeMap<u32,String>,result:Option<&Symbol>)->Result<String>{self.count()?;Ok(match p{
        Predicate::Bool(b)=>if *b{"True"}else{"False"}.into(),Predicate::Compare(op,a,b)=>format!("({} {op} {})",self.number(a,locals,before)?,self.number(b,locals,before)?),Predicate::Not(p)=>format!("(¬ {})",self.predicate(p,locals,cells,before,old_cells,result)?),Predicate::Binary(op,a,b)=>format!("({} {op} {})",self.predicate(a,locals,cells,before,old_cells,result)?,self.predicate(b,locals,cells,before,old_cells,result)?),
        Predicate::Initialized(p,i)=>{let cap=self.foci[self.buffer(p,locals,before)? as usize].cap.clone();let i=self.number(i,locals,before)?;if i!="j"{self.indices.insert(i.clone());}let c=self.cells(p,locals,cells,before,old_cells)?;format!("({i} < {cap} ∧ {c} {i} ≠ none)")},Predicate::Empty(p,i)=>{let c=self.cells(p,locals,cells,before,old_cells)?;let i=self.number(i,locals,before)?;
            // A cell a clause names is one the quantified premises are put to use at.
            if i!="j"{self.indices.insert(i.clone());}format!("({c} {i} = none)")},
        Predicate::Cell(p,i,v)=>{let c=self.cells(p,locals,cells,before,old_cells)?;let i=self.number(i,locals,before)?;let v=match v{Element::Result=>result.ok_or_else(||error("memory proof result unavailable in requires"))?,Element::Path(p)=>self.path(p,locals,before)?};let slot=match v{Symbol::Slot(slot)=>slot.clone(),
            // A fixed element type is a number; its stored form is `some` of it.
            Symbol::Nat(n) if !matches!(self.contract.element,Ty::Param{..})=>format!("some {n}"),_=>return Err(error("memory proof cell payload must be actual generic slot"))};format!("(({slot}) ≠ none ∧ {c} {i} = ({slot}))")},
        Predicate::Utf8Boundary(p,index)=>{
            let cells=self.cells(p,locals,cells,before,old_cells)?;
            let index=self.number(index,locals,before)?;
            format!("(¬ Witchy.Utf8.Continuation (({cells} {index}).getD 256))")
        },
        Predicate::Utf8(p,start,count)=>{
            let c = if matches!(p.ty, Ty::Slice(_)) {
                let Symbol::Slice(_, Some(contents)) = self.path(p, locals, before)? else {
                    return Err(error("UTF-8 proof needs the source slice's modeled contents"));
                };
                contents.clone()
            } else {
                self.cells(p,locals,cells,before,old_cells)?
            };
            let start=self.number(start,locals,before)?;
            let count=self.number(count,locals,before)?;
            format!("(Witchy.Utf8.ValidCells {c} {start} {count})")
        },
        Predicate::Same(a,b)=>{let a=self.buffer(a,locals,before)?;let b=self.buffer(b,locals,before)?;format!("({} = {})",self.foci[a as usize].key,self.foci[b as usize].key)},
        Predicate::Result=>prop(result.ok_or_else(||error("memory proof result unavailable in requires"))?)?.to_string(),
        Predicate::SliceContents(..)|Predicate::FullRange(..)|Predicate::Forall(..)|Predicate::Except(..)|Predicate::Unchanged(..)|Predicate::From(..)|Predicate::SamePrefix(..)|Predicate::EmptyRange(..)|Predicate::EmptyFrom(..)|Predicate::EmptyAll(..)=>conjunction(self.flat(p,locals,cells,before,old_cells,result)?.into_iter().map(|(t,_)|t).collect()),
    })}
    fn check(&mut self,s:&State,text:String)->Result<()>{self.check_index(s,text,false)}
    fn check_by(&mut self,s:&State,text:String,tactic:&str)->Result<()>{self.count()?;for _ in &s.path{self.count()?;}self.checks.push(Check{path:s.path.clone(),text,index:false,tactic:Some(tactic.into())});Ok(())}
    fn check_index(&mut self,s:&State,text:String,index:bool)->Result<()>{self.count()?;for _ in &s.path{self.count()?;}self.checks.push(Check{path:s.path.clone(),text,index,tactic:None});Ok(())}
    fn key_word(&mut self,id:u32)->String {
        self.uses_keys=true;
        format!("(encode {})",self.foci[id as usize].key)
    }
    fn cell_index(&mut self,s:&mut State,op:StorageOp,id:u32,operand:&Expr)->Result<String>{
        let word=nat(&self.value(s,operand)?)?.to_owned();
        if matches!(op, StorageOp::RefLink | StorageOp::MutLink) {
            let key=self.key_word(id);
            self.check(s,format!("{word} = {key}"))?;
            Ok("(0 : Nat)".into())
        } else { Ok(word) }
    }
    fn value(&mut self,s:&mut State,e:&Expr)->Result<Symbol>{self.count()?;Ok(match &e.kind{
        Value::Unit=>Symbol::Unit,Value::U64(n)=>Symbol::Nat(format!("({n} : Nat)")),Value::Bool(b)=>Symbol::Prop(if *b{"True"}else{"False"}.into()),
        // Moving an element names the same value a read would.
        Value::ReadPlace(p)|Value::Address(p)|Value::Move{place:p,..}=>{let fields=p.projections.iter().map(|p|if let Projection::Field{ordinal}=p{Ok(*ordinal as usize)}else{Err(error("memory proof indexed reference context unsupported"))}).collect::<Result<Vec<_>>>()?;s.locals.get(&p.root).and_then(|v|v.field(&fields)).cloned().ok_or_else(||error("memory proof missing place"))?},
        Value::AllocationKey(v,_)=>{
            let Symbol::Buffer(id)=self.value(s,v)? else{return Err(error("allocation key needs a live buffer loan"));};
            Symbol::Nat(self.key_word(id))
        },
        Value::SliceLen(v) => {
            let Symbol::Slice(length, _) = self.value(s, v)? else { return Err(error("memory proof slice length requires a slice")); };
            Symbol::Nat(length)
        },
        Value::ProofUtf8(source) => {
            let slice = self.value(s, source)?;
            let Symbol::Slice(length, Some(cells)) = &slice else {
                return Err(error("memory proof UTF-8 refinement requires modeled slice contents"));
            };
            self.check(s, format!("Witchy.Utf8.ValidCells {cells} 0 {length}"))?;
            slice
        }
        Value::Widen(source) if e.ty == Ty::U64 && source.ty == self.contract.element
            && matches!(source.ty, Ty::Unsigned(Scalar::U8 | Scalar::U16 | Scalar::U32)) =>
        {
            match self.value(s, source)? {
                Symbol::Slot(slot) => Symbol::Nat(self.bind("Nat", format!("(({slot}).getD 256)"))?),
                Symbol::Nat(value) => Symbol::Nat(value),
                _ => return Err(error("memory proof widening requires a numeric cell")),
            }
        }
        // Forgetting the UTF-8 refinement keeps the address, length and cells.
        Value::Widen(source)
            if matches!(source.ty, Ty::Borrow { mutable: false, pointee: Pointee::Slice(id) }
                if self.program.types.is_str(id))
            && matches!(e.ty, Ty::Borrow { mutable: false, pointee: Pointee::Slice(id) }
                if !self.program.types.is_str(id)
                    && self.program.types.slice_element(id) == Ty::Unsigned(Scalar::U8)) =>
        {
            self.value(s, source)?
        }
        Value::SliceWithin(..) => return Err(error("`part_proved` is shown by a proof over numbers; this function's proof is about a Buffer")),
        Value::SlicePart(source, from, to) => {
            let utf8 = matches!(source.ty, Ty::Borrow { mutable: false, pointee: Pointee::Slice(id) }
                if self.program.types.is_str(id));
            let Symbol::Slice(source_length, contents) = self.value(s, source)? else {
                return Err(error("memory proof slice range requires a shared slice"));
            };
            let from = nat(&self.value(s, from)?)?.to_owned();
            let to = nat(&self.value(s, to)?)?.to_owned();
            self.check(s, format!("{from} ≤ {to}"))?;
            self.check(s, format!("{to} ≤ {source_length}"))?;
            let length = self.bind("Nat", format!("({to} - {from})"))?;
            let selected = contents.as_ref().map(|cells| format!("(fun k : Nat => {cells} ({from} + k))"));
            if utf8 {
                let (Some(cells), Some(part)) = (contents, selected.as_ref()) else {
                    return Err(error("memory proof text range requires modeled UTF-8 contents"));
                };
                // Both backends trap at an interior continuation byte. Only the normal
                // continuation can use those boundary checks as premises.
                for endpoint in [&from, &to] {
                    s.path.push(format!(
                        "{endpoint} < {source_length} → ¬ Witchy.Utf8.Continuation (({cells} {endpoint}).getD 256)"
                    ));
                }
                let valid = format!("Witchy.Utf8.ValidCells {part} 0 {length}");
                let tactic = format!(
                    "exact Witchy.Utf8.ValidCells.slice (cell := {cells}) (count := {source_length}) (lo := {from}) (hi := {to}) (by assumption) (by assumption) (by assumption) (by assumption)"
                );
                self.check_by(s, valid.clone(), &tactic)?;
                s.path.push(valid);
            }
            Symbol::Slice(length, selected)
        }
        Value::Capacity(v,_)=>{let Symbol::Buffer(id)=self.value(s,v)? else{return Err(error("memory proof capacity requires buffer"));};Symbol::Nat(self.foci[id as usize].cap.clone())},
        Value::Not(v)=>{let v=prop(&self.value(s,v)?)?.to_string();Symbol::Prop(self.bind("Prop",format!("(¬ {v})"))?)},
        // Both sides are evaluated. That matches short-circuit evaluation only when the
        // right side cannot trap or write, which holds for comparisons of locals.
        Value::Binary(op @ (Binary::And|Binary::Or),a,b)=>{if !pure_guard(b){return Err(error("memory proof use nested If for short-circuit runtime guards"));}
            let a=prop(&self.value(s,a)?)?.to_string();let b=prop(&self.value(s,b)?)?.to_string();Symbol::Prop(self.bind("Prop",format!("({a} {} {b})",if matches!(op,Binary::And){"∧"}else{"∨"}))?)},
        Value::Binary(op,a,b)=>{let a=self.value(s,a)?;let b=self.value(s,b)?;let(a,b)=(nat(&a)?,nat(&b)?);let token=match op{Binary::Add=>"+",Binary::Sub=>"-",Binary::Eq=>"=",Binary::Ne=>"≠",Binary::Lt=>"<",Binary::Le=>"≤",Binary::Gt=>">",Binary::Ge=>"≥",_=>return Err(error("memory proof supports checked Add, Sub and numeric comparisons"))};let add=matches!(op,Binary::Add|Binary::Sub);
            // Subtraction is shown not to go below zero before the truncating difference is named.
            if matches!(op,Binary::Sub){self.check(s,format!("{b} ≤ {a}"))?;}
            let v=self.bind(if add{"Nat"}else{"Prop"},format!("({a} {token} {b})"))?;// The sum is shown to fit the type it is written at (a U8 sum below 256), not only a
            // U64: it is emitted with no test and no narrowing once shown.
            if matches!(op,Binary::Add){self.check(s,format!("{v} ≤ {}",match e.ty{Ty::Unsigned(scalar)=>scalar.layout().maximum(),_=>u64::MAX}))?;}if add{self.shown.insert(e as *const Expr as usize);Symbol::Nat(v)}else{Symbol::Prop(v)}},
        Value::Storage{op,receiver,index,value:None,..} if matches!(op,StorageOp::GetUnchecked|StorageOp::RefUnchecked|StorageOp::RefLink|StorageOp::Initialized)=>{
            let Symbol::Buffer(id)=self.value(s,receiver)? else{return Err(error("memory proof operation requires buffer"));};let i=self.cell_index(s,*op,id,index)?;self.indices.insert(i.clone());let cap=self.foci[id as usize].cap.clone();let cells=s.cells[&id].clone();
            if *op==StorageOp::Initialized{Symbol::Prop(self.bind("Prop",format!("({i} < {cap} ∧ {cells} {i} ≠ none)"))?)}else{self.seen.insert(e as *const Expr as usize);self.check(s,format!("{i} < {cap}"))?;self.check(s,format!("{cells} {i} ≠ none"))?;Symbol::Slot(format!("{cells} {i}"))}
        },
        // A slice of cells `from..to`: in order, within the allocation, and each holding a value
        // (docs/design-slices.md). The slice itself is not followed further by this model.
        Value::Storage{op:StorageOp::SliceUnchecked,receiver,index,value:Some(to),..}=>{
            let Symbol::Buffer(id)=self.value(s,receiver)? else{return Err(error("memory proof slice requires buffer"));};
            let from=nat(&self.value(s,index)?)?.to_string();let to=nat(&self.value(s,to)?)?.to_string();
            let cap=self.foci[id as usize].cap.clone();let cells=s.cells[&id].clone();
            self.seen.insert(e as *const Expr as usize);self.indices.insert(from.clone());self.indices.insert(to.clone());
            self.check(s,format!("{from} ≤ {to}"))?;self.check(s,format!("{to} ≤ {cap}"))?;
            self.check(s,format!("∀ j, {from} ≤ j → j < {to} → {cells} j ≠ none"))?;
            // The loan sees the cell values at the point where the slice was taken.
            let length = self.bind("Nat", format!("({to} - {from})"))?;
            let contents = if from == "(0 : Nat)" {
                cells
            } else {
                format!("(fun k : Nat => {cells} ({from} + k))")
            };
            Symbol::Slice(length, Some(contents))
        },
        // A `&mut` reference to a cell: in bounds and holding a value. Whoever holds the reference
        // may store any value there, so from here on the cell holds an arbitrary one. What is
        // proved after this point, the type's invariant included, cannot depend on it.
        Value::Storage{op:op @ (StorageOp::MutUnchecked|StorageOp::MutLink),receiver,index,value:None,..}=>{
            let Symbol::Buffer(id)=self.value(s,receiver)? else{return Err(error("memory proof unchecked reference requires buffer"));};if !self.foci[id as usize].mutable{return Err(error("memory proof unchecked `&mut` reference requires exclusive borrowed focus"));}
            if self.in_loop{return Err(error("memory proof takes a `&mut` cell reference outside loops only"));}
            let i=self.cell_index(s,*op,id,index)?;let old=s.cells[&id].clone();let cap=self.foci[id as usize].cap.clone();
            self.seen.insert(e as *const Expr as usize);self.indices.insert(i.clone());self.check(s,format!("{i} < {cap}"))?;self.check(s,format!("{old} {i} ≠ none"))?;
            let any=self.ghost("α")?;
            let updated=self.bind("Nat → Option α",format!("(fun j : Nat => if j = {i} then some {any} else {old} j)"))?;s.cells.insert(id,updated);Symbol::Slot(format!("some {any}"))
        },
        // The proved range move: cells `0..n` of the source all hold values and those of the
        // target are all empty; afterwards the target's first `n` cells are the source's old
        // ones, in order, and the source's are empty. Everything else is as it was.
        Value::Storage{op:StorageOp::MovePrefixUnchecked,receiver,index,value:Some(target),..}=>{
            let Symbol::Buffer(from)=self.value(s,receiver)? else{return Err(error("memory proof range move requires buffers"));};
            let Symbol::Buffer(to)=self.value(s,target)? else{return Err(error("memory proof range move requires buffers"));};
            if from==to{return Err(error("memory proof range move requires two buffers"));}
            if !self.foci[from as usize].mutable||!self.foci[to as usize].mutable{return Err(error("memory proof range move requires exclusive borrowed foci"));}
            let n=nat(&self.value(s,index)?)?.to_string();
            let (source,dest)=(s.cells[&from].clone(),s.cells[&to].clone());
            let (cap_from,cap_to)=(self.foci[from as usize].cap.clone(),self.foci[to as usize].cap.clone());
            self.seen.insert(e as *const Expr as usize);self.indices.insert(n.clone());
            self.check(s,format!("{n} ≤ {cap_from}"))?;self.check(s,format!("{n} ≤ {cap_to}"))?;
            self.check(s,format!("∀ j : Nat, (0 ≤ j ∧ j < {n}) → {source} j ≠ none"))?;
            self.check(s,format!("∀ j : Nat, (0 ≤ j ∧ j < {n}) → {dest} j = none"))?;
            let moved=self.bind("Nat → Option α",format!("(fun j : Nat => if j < {n} then {source} j else {dest} j)"))?;
            let emptied=self.bind("Nat → Option α",format!("(fun j : Nat => if j < {n} then none else {source} j)"))?;
            s.cells.insert(to,moved);s.cells.insert(from,emptied);Symbol::Unit
        },
        // The proved range take of Copy elements: cells `0..n` hold values and `n` fits; afterwards
        // they are empty and everything else is as it was.
        Value::Storage{op:StorageOp::TakePrefixUnchecked,receiver,index,value:None,..}=>{
            let Symbol::Buffer(b)=self.value(s,receiver)? else{return Err(error("memory proof range take requires a buffer"));};
            if !self.foci[b as usize].mutable{return Err(error("memory proof range take requires an exclusive borrowed focus"));}
            let n=nat(&self.value(s,index)?)?.to_string();
            let cells=s.cells[&b].clone();let cap=self.foci[b as usize].cap.clone();
            self.seen.insert(e as *const Expr as usize);self.indices.insert(n.clone());
            self.check(s,format!("{n} ≤ {cap}"))?;
            self.check(s,format!("∀ j : Nat, (0 ≤ j ∧ j < {n}) → {cells} j ≠ none"))?;
            let emptied=self.bind("Nat → Option α",format!("(fun j : Nat => if j < {n} then none else {cells} j)"))?;
            s.cells.insert(b,emptied);Symbol::Unit
        },
        // The proved word read of a byte buffer: `count <= 8`, cells `at..at+count` within it and
        // holding values. The word is a number nothing here knows; nothing changes.
        Value::Storage{op:StorageOp::ReadWordUnchecked,receiver,index,value:Some(count),..}=>{
            let Symbol::Buffer(b)=self.value(s,receiver)? else{return Err(error("memory proof word read requires a buffer"));};
            if matches!(self.contract.element,Ty::Param{..}){return Err(error("memory proof word read requires a byte buffer"));}
            let a=nat(&self.value(s,index)?)?.to_string();let n=nat(&self.value(s,count)?)?.to_string();
            let cells=s.cells[&b].clone();let cap=self.foci[b as usize].cap.clone();
            self.seen.insert(e as *const Expr as usize);self.indices.insert(a.clone());self.indices.insert(n.clone());
            self.check(s,format!("{n} ≤ 8"))?;
            self.check(s,format!("{a} + {n} ≤ {cap}"))?;
            self.check_index(s,format!("∀ j : Nat, ({a} ≤ j ∧ j < {a} + {n}) → {cells} j ≠ none"),true)?;
            let word=self.ghost("Nat")?;self.ranges.push(word.clone());Symbol::Nat(word)
        },
        // The proved word write of a byte buffer: `count <= 8`, cells `at..at+count` empty and
        // within it; afterwards they hold values nothing here knows (a fresh function of the
        // cell), so no proof can lean on which bytes they are. Everything else is as it was.
        Value::Storage{op:StorageOp::InitWordUnchecked,receiver,index,value:Some(pair),..}=>{
            let Value::Aggregate(parts)=&pair.kind else{return Err(error("memory proof word write requires word and count"));};
            let [word,count]=parts.as_slice() else{return Err(error("memory proof word write requires word and count"));};
            let Symbol::Buffer(b)=self.value(s,receiver)? else{return Err(error("memory proof word write requires a buffer"));};
            if !self.foci[b as usize].mutable{return Err(error("memory proof word write requires an exclusive borrowed focus"));}
            if matches!(self.contract.element,Ty::Param{..}){return Err(error("memory proof word write requires a byte buffer"));}
            let a=nat(&self.value(s,index)?)?.to_string();self.value(s,word)?;let n=nat(&self.value(s,count)?)?.to_string();
            let cells=s.cells[&b].clone();let cap=self.foci[b as usize].cap.clone();
            self.seen.insert(e as *const Expr as usize);self.indices.insert(a.clone());self.indices.insert(n.clone());
            self.check(s,format!("{n} ≤ 8"))?;
            self.check(s,format!("{a} + {n} ≤ {cap}"))?;
            self.check_index(s,format!("∀ j : Nat, ({a} ≤ j ∧ j < {a} + {n}) → {cells} j = none"),true)?;
            let bytes=self.ghost("Nat → Nat")?;
            let written=self.bind("Nat → Option α",format!("(fun j : Nat => if j < {a} then {cells} j else if j < {a} + {n} then some ({bytes} j) else {cells} j)"))?;
            s.cells.insert(b,written);Symbol::Unit
        },
        // Preserve the source values when the slice's backing cells are visible.
        // An opaque source still guarantees that each copied cell is initialized.
        Value::Storage { op: StorageOp::InitSliceUnchecked, receiver, index, value: Some(source), .. } => {
            let Symbol::Buffer(target) = self.value(s, receiver)? else { return Err(error("memory proof slice copy requires a target buffer")); };
            if !self.foci[target as usize].mutable { return Err(error("memory proof slice copy requires an exclusive target")); }
            let at = nat(&self.value(s, index)?)?.to_owned();
            let Symbol::Slice(count, contents) = self.value(s, source)? else { return Err(error("memory proof slice copy requires a shared slice")); };
            let cells = s.cells[&target].clone();
            let capacity = self.foci[target as usize].cap.clone();
            self.seen.insert(e as *const Expr as usize);
            self.indices.insert(at.clone());
            self.indices.insert(count.clone());
            self.check(s, format!("{at} + {count} ≤ {capacity}"))?;
            self.check_index(s, format!("∀ j : Nat, ({at} ≤ j ∧ j < {at} + {count}) → {cells} j = none"), true)?;
            let contents = match contents {
                Some(contents) => contents,
                None => {
                    let values = self.ghost("Nat → Nat")?;
                    format!("(fun k : Nat => some ({values} k))")
                }
            };
            let written = self.bind("Nat → Option α", format!("(fun j : Nat => if j < {at} then {cells} j else if j < {at} + {count} then {contents} (j - {at}) else {cells} j)"))?;
            s.cells.insert(target, written);
            Symbol::Unit
        },
        // The proved range fill of a Copy element: cells `0..n` are empty and `n` fits; afterwards
        // each holds the item and everything else is as it was.
        Value::Storage{op:StorageOp::FillUnchecked,receiver,index,value:Some(payload),..}=>{
            let Symbol::Buffer(b)=self.value(s,receiver)? else{return Err(error("memory proof range fill requires a buffer"));};
            if !self.foci[b as usize].mutable{return Err(error("memory proof range fill requires an exclusive borrowed focus"));}
            let n=nat(&self.value(s,index)?)?.to_string();
            let slot = match self.value(s, payload)? {
                Symbol::Slot(slot) => slot,
                Symbol::Nat(value) if !matches!(self.contract.element, Ty::Param { .. }) => format!("some {value}"),
                _ => return Err(error("memory proof range fill requires a modeled element")),
            };
            let cells=s.cells[&b].clone();let cap=self.foci[b as usize].cap.clone();
            self.seen.insert(e as *const Expr as usize);self.indices.insert(n.clone());
            self.check(s,format!("{n} ≤ {cap}"))?;
            self.check_index(s,format!("∀ j : Nat, (0 ≤ j ∧ j < {n}) → {cells} j = none"),true)?;
            self.check(s,format!("({slot}) ≠ none"))?;
            let filled=self.bind("Nat → Option α",format!("(fun j : Nat => if j < {n} then ({slot}) else {cells} j)"))?;
            s.cells.insert(b,filled);Symbol::Unit
        },
        // The proved range copy: the source's cells `0..n` hold values, the target's `at..at+n`
        // are empty and within it; afterwards those target cells are the source's, in order.
        // The source is unchanged (its elements are Copy).
        Value::Storage{op:StorageOp::CopyIntoUnchecked,receiver,index,value:Some(pair),..}=>{
            let Value::Aggregate(parts)=&pair.kind else{return Err(error("memory proof range copy requires target and offset"));};
            let [target,at]=parts.as_slice() else{return Err(error("memory proof range copy requires target and offset"));};
            let Symbol::Buffer(from)=self.value(s,receiver)? else{return Err(error("memory proof range copy requires buffers"));};
            let Symbol::Buffer(to)=self.value(s,target)? else{return Err(error("memory proof range copy requires buffers"));};
            if from==to{return Err(error("memory proof range copy requires two buffers"));}
            if !self.foci[to as usize].mutable{return Err(error("memory proof range copy requires an exclusive borrowed target"));}
            let n=nat(&self.value(s,index)?)?.to_string();let a=nat(&self.value(s,at)?)?.to_string();
            let (source,dest)=(s.cells[&from].clone(),s.cells[&to].clone());
            let (cap_from,cap_to)=(self.foci[from as usize].cap.clone(),self.foci[to as usize].cap.clone());
            self.seen.insert(e as *const Expr as usize);self.indices.insert(n.clone());
            self.check(s,format!("{n} ≤ {cap_from}"))?;self.check(s,format!("{a} + {n} ≤ {cap_to}"))?;
            self.check(s,format!("∀ j : Nat, ((0 : Nat) ≤ j ∧ j < {n}) → {source} j ≠ none"))?;
            self.check(s,format!("∀ j : Nat, ({a} ≤ j ∧ j < {a} + {n}) → {dest} j = none"))?;
            // The source's cells seen from the target's positions, shown from the check above by
            // instantiating it at `j - at`; then known on this path.
            let shifted=format!("∀ j : Nat, ({a} ≤ j ∧ j < {a} + {n}) → {source} (j - {a}) ≠ none");
            // Tactic steps only, and the inline `omega`: an auxiliary theorem (a `by` inside a term,
            // or plain `omega`'s abstracted proof) is refused by the proof audit.
            let by=format!("intro j hj\n    apply_assumption\n    refine ⟨Nat.zero_le _, ?_⟩\n    witchy_inline_omega");
            self.check_by(s,shifted.clone(),&by)?;s.path.push(shifted);
            let copied=self.bind("Nat → Option α",format!("(fun j : Nat => if {a} ≤ j ∧ j < {a} + {n} then {source} (j - {a}) else {dest} j)"))?;
            // What the copy keeps of a prefix-shaped target (a Vec's), each shown by a fixed case
            // split on `j < at` and then known on this path: full cells below `at` stay full and
            // the run joins them; empty cells from `at` stay empty past the run. Both hold for any
            // target, as implications.
            let full=format!("(∀ j : Nat, ((0 : Nat) ≤ j ∧ j < {a}) → {dest} j ≠ none) → ∀ j : Nat, ((0 : Nat) ≤ j ∧ j < {a} + {n}) → {copied} j ≠ none");
            let full_by=format!("intro hp j hj\n    by_cases hlt : j < {a}\n    · rw [if_neg]\n      · exact hp j ⟨hj.1, hlt⟩\n      · intro hc\n        witchy_inline_omega\n    · rw [if_pos]\n      · apply_assumption\n        constructor <;> witchy_inline_omega\n      · constructor <;> witchy_inline_omega");
            self.check_by(s,full.clone(),&full_by)?;s.path.push(full);
            let empty=format!("(∀ j : Nat, {a} ≤ j → {dest} j = none) → ∀ j : Nat, {a} + {n} ≤ j → {copied} j = none");
            let empty_by=format!("intro hp j hj\n    rw [if_neg]\n    · apply hp\n      witchy_inline_omega\n    · intro hc\n      witchy_inline_omega");
            self.check_by(s,empty.clone(),&empty_by)?;s.path.push(empty);
            s.cells.insert(to,copied);Symbol::Unit
        },
        // The proved take: in bounds and holding a value, which moves out and leaves the cell empty.
        Value::Storage{op:StorageOp::TakeUnchecked | StorageOp::ReplaceUnchecked,receiver,index,value:incoming,..}=>{
            let Symbol::Buffer(id)=self.value(s,receiver)? else{return Err(error("memory proof unchecked take or replace requires buffer"));};
            if !self.foci[id as usize].mutable{return Err(error("memory proof unchecked take or replace requires exclusive borrowed focus"));}
            let i=nat(&self.value(s,index)?)?.to_string();
            let replacement = if let Some(incoming) = incoming {
                let Symbol::Slot(slot) = self.value(s, incoming)? else { return Err(error("memory proof replacement requires actual generic item")); };
                self.check(s, format!("({slot}) ≠ none"))?;
                format!("({slot})")
            } else { "none".into() };
            let old=s.cells[&id].clone();let cap=self.foci[id as usize].cap.clone();
            self.seen.insert(e as *const Expr as usize);self.indices.insert(i.clone());self.check(s,format!("{i} < {cap}"))?;self.check(s,format!("{old} {i} ≠ none"))?;
            let updated=self.bind("Nat → Option α",format!("(fun j : Nat => if j = {i} then {replacement} else {old} j)"))?;s.cells.insert(id,updated);Symbol::Slot(format!("{old} {i}"))
        },
        Value::Storage{op:StorageOp::InitUnchecked,receiver,index,value:Some(payload),..}=>{
            let Symbol::Buffer(id)=self.value(s,receiver)? else{return Err(error("memory proof initialization requires buffer"));};if !self.foci[id as usize].mutable{return Err(error("memory proof unchecked write requires exclusive borrowed focus"));}
            let i=nat(&self.value(s,index)?)?.to_string();let payload=self.value(s,payload)?;let slot=match payload {
                Symbol::Slot(slot) => slot,
                Symbol::Nat(value) if !matches!(self.contract.element,Ty::Param{..}) => format!("some {value}"),
                _ => return Err(error("memory proof initialization requires a modeled element")),
            };let old=s.cells[&id].clone();let cap=self.foci[id as usize].cap.clone();
            self.seen.insert(e as *const Expr as usize);self.indices.insert(i.clone());self.check(s,format!("{i} < {cap}"))?;self.check(s,format!("{old} {i} = none"))?;self.check(s,format!("({slot}) ≠ none"))?;
            let updated=self.bind("Nat → Option α",format!("(fun j : Nat => if j = {i} then ({slot}) else {old} j)"))?;s.cells.insert(id,updated);Symbol::Unit
        },
        Value::Call(c)=>{let mut outcomes=self.call(s,c)?;if outcomes.len()!=1{return Err(error("memory proof nested Result-return helper unsupported"));}let(facts,v)=outcomes.pop().expect("one outcome");s.path.extend(facts);v},
        Value::Sum{arm,payload}=>Symbol::Sum(*arm,Box::new(self.value(s,payload)?)),
        _=>return Err(error("memory proof unsupported expression/effect")),
    })}
    /// A discarded checked Take needs no invented T/result witness. The exact
    /// normal cell effect is None when in bounds, otherwise unchanged; taking an
    /// already-empty cell has the same logical state on its Err arm.
    fn discarded(&mut self,s:&mut State,e:&Expr)->Result<()>{
        if let Value::Storage{op:StorageOp::Take,receiver,index,value:None,..}=&e.kind{
            self.count()?;let Symbol::Buffer(id)=self.value(s,receiver)? else{return Err(error("memory proof Take receiver requires buffer"));};
            if !self.foci[id as usize].mutable{return Err(error("memory proof Take requires exclusive borrowed focus"));}
            let i=nat(&self.value(s,index)?)?.to_string();let cap=self.foci[id as usize].cap.clone();let old=s.cells[&id].clone();self.indices.insert(i.clone());
            let updated=self.bind("Nat → Option α",format!("(fun j : Nat => if j = {i} ∧ {i} < {cap} then none else {old} j)"))?;s.cells.insert(id,updated);Ok(())
        }else{self.value(s,e)?;Ok(())}
    }
    fn store(&mut self,s:&mut State,p:&Place,op:Option<Binary>,e:&Expr)->Result<()>{
        let local=self.function.locals.get(p.root as usize).ok_or_else(||error("memory proof missing local"))?;
        if let(false,true,true,Ty::Unsigned(width))=(p.pointer,p.projections.is_empty(),local.mutable,p.ty){
            // The destination is read before the right side runs, as the emitted code reads it.
            let old=if op.is_some(){Some(s.locals.get(&p.root).cloned().ok_or_else(||error("memory proof missing mutable local"))?)}else{None};
            let value=self.value(s,e)?;let v=if let Some(Binary::Add)=op{let a=nat(old.as_ref().expect("read above"))?;let b=nat(&value)?;Symbol::Nat(self.bind("Nat",format!("({a} + {b})"))?)}
                else if let Some(Binary::Sub)=op{let a=nat(old.as_ref().expect("read above"))?.to_string();let b=nat(&value)?.to_string();self.check(s,format!("{b} ≤ {a}"))?;Symbol::Nat(self.bind("Nat",format!("({a} - {b})"))?)}
                else if op.is_none(){value}else{return Err(error("memory proof mutable U64 operation unsupported"));};
            self.check(s,format!("{} ≤ {}",nat(&v)?,width.layout().maximum()))?;s.locals.insert(p.root,v);return Ok(());
        }
        if self.in_loop||op.is_some()||!self.contract.mutable||self.foci.is_empty()||!p.pointer||p.root!=self.contract.borrow||p.ty!=Ty::U64||p.projections.is_empty(){return Err(error("memory proof stores require supported exclusive record field or mutable U64 local"));}
        let fields=p.projections.iter().map(|p|if let Projection::Field{ordinal}=p{Ok(*ordinal as usize)}else{Err(error("memory proof indexed store refused"))}).collect::<Result<Vec<_>>>()?;let v=self.value(s,e)?;self.check(s,format!("{} ≤ {}",nat(&v)?,u64::MAX))?;s.locals.get_mut(&p.root).and_then(|old|old.store(&fields,v)).ok_or_else(||error("memory proof missing typed store path"))?;Ok(())
    }
    fn loop_invariants(&mut self,s:&State,invariants:&[Predicate])->Result<Vec<(String,bool)>>{let before=self.entry.clone();let cells=self.entry_cells();let mut out=vec![];for p in invariants{out.extend(self.flat(p,&s.locals,&s.cells,&before,&cells,None)?);}Ok(out)}
    fn loop_state(&mut self,mut s:State)->Result<State>{
        // A record behind a shared borrow cannot change during the loop; one behind an exclusive borrow could.
        if self.function.params.iter().any(|t|matches!(t,Ty::Borrow{pointee,mutable} if *mutable&&!matches!(pointee.ty(),Ty::Buffer(_)))){return Err(error("memory proof loop requires direct borrowed Buffer foci"));}
        for (id,v) in &mut s.locals{let local=&self.function.locals[*id as usize];if local.mutable{if !matches!(local.ty,Ty::Unsigned(_))||!matches!(v,Symbol::Nat(_)){return Err(error("memory proof loop mutable head locals must be U64"));}let n=self.ghost("Nat")?;self.ranges.push(n.clone());*v=Symbol::Nat(n);}}
        // The cells may have changed, but not past the capacity: every write in the body is
        // shown to be below it.
        for id in 0..self.foci.len(){if self.foci[id].mutable{let n=self.ghost("Nat → Option α")?;s.path.push(format!("∀ j : Nat, {} ≤ j → {n} j = none",self.foci[id].cap));s.cells.insert(id as u32,n);}}
        Ok(s)
    }
    fn loop_body(&mut self,condition:&Expr,body:&[Stmt],invariants:&[Predicate],s:State)->Result<State>{
        if invariants.is_empty()||self.in_loop||contains_loop(body)||!pure_guard(condition){return Err(error("memory proof loops require invariants, pure guard and unnested supported body"));}
        for(text,index)in self.loop_invariants(&s,invariants)?{self.check_index(&s,text,index)?;}
        let mut head=self.loop_state(s)?;for(text,_)in self.loop_invariants(&head,invariants)?{head.path.push(text);}
        let guard=prop(&self.value(&mut head,condition)?)?.to_string();let mut entered=head.clone();entered.path.push(guard.clone());
        self.in_loop=true;let backedges=self.run(body,vec![entered]);self.in_loop=false;
        for back in backedges?{for(text,index)in self.loop_invariants(&back,invariants)?{self.check_index(&back,text,index)?;}}
        head.path.push(format!("¬ {guard}"));Ok(head)
    }
    fn forget_mutable_record_fields(&mut self, s: &mut State, value: &Symbol) -> Result<Symbol> {
        self.count()?;
        Ok(match value {
            Symbol::Record(fields) => Symbol::Record(fields.iter().map(|field| {
                self.forget_mutable_record_fields(s, field)
            }).collect::<Result<_>>()?),
            Symbol::Buffer(id) => {
                if !self.foci[*id as usize].mutable {
                    return Err(error("memory proof mutable record contains a shared focus"));
                }
                let cells = self.ghost("Nat → Option α")?;
                s.cells.insert(*id, cells);
                Symbol::Buffer(*id)
            }
            Symbol::Nat(_) => {
                let value = self.ghost("Nat")?;
                self.ranges.push(value.clone());
                Symbol::Nat(value)
            }
            Symbol::Prop(_) => Symbol::Prop(self.ghost("Prop")?),
            Symbol::Unit => Symbol::Unit,
            _ => return Err(error("memory proof cannot summarize this mutable record field")),
        })
    }
    /// A call to a proved helper: its precondition is recorded for the site, and each
    /// way it can return yields the facts its contract states for that outcome.
    fn call(&mut self,s:&mut State,c:&InstanceCall)->Result<Vec<(Vec<String>,Symbol)>>{
        let mut args = Vec::new();
        for arg in &c.args {
            args.push(self.value(s, arg)?);
        }
        let callee = &self.program.functions[c.definition.0 as usize];
        if callee.name == "#str_from_bytes" {
            let [slice @ Symbol::Slice(length, Some(cells))] = args.as_slice() else {
                return Err(error("checked UTF-8 validation needs modeled byte contents"));
            };
            // The generated validator can return this very slice as &str only
            // after inspecting its bytes. Failure supplies no content fact.
            return Ok(vec![
                (vec![format!("Witchy.Utf8.ValidCells {cells} 0 {length}")],
                    Symbol::Sum(0, Box::new(slice.clone()))),
                (vec![], Symbol::Sum(1, Box::new(Symbol::Nat("(1 : Nat)".into())))),
            ]);
        }
        let contract = callee.contract.as_ref().and_then(|c|c.memory.as_ref())
            .ok_or_else(||error("memory proof unverified helper/effect context"))?.clone();
        if !(c.substitutions==[self.contract.element]||(c.substitutions.is_empty()&&contract.element==self.contract.element)){return Err(error("memory proof actual generic substitution mismatch"));}
        // Inside a loop only helpers that cannot write are allowed; the head state stays valid.
        if self.in_loop&&contract.mutable{return Err(error("memory proof loop body helper effects unsupported"));}
        if contract.mutable&&(!self.contract.mutable||contract.result!=Ty::Unit){return Err(error("memory proof modular mutable calls require an exclusive focus and Unit result"));}
        let mut bindings=args.into_iter().enumerate().map(|(i,v)|(i as u32,v)).collect::<BTreeMap<_,_>>();let old_bindings=bindings.clone();let old_cells=s.cells.clone();let mut pre=vec![];for p in contract.requires.iter().chain(contract.assumes.iter().filter(|(module,_)|*module==self.contract.module).map(|(_,p)|p)){pre.push(self.predicate(p,&bindings,&s.cells,&old_bindings,&old_cells,None)?);}let text=conjunction(pre);let site=c.contract_site.ok_or_else(||error("memory proof missing primary call site"))?;self.count()?;self.calls.get_mut(&site).ok_or_else(||error("memory proof call absent from typed inventory"))?.push(Check{path:s.path.clone(),text,index:false,tactic:None});
        if contract.mutable {
            let actual = bindings.get(&contract.borrow).cloned()
                .ok_or_else(|| error("memory proof missing mutable actual"))?;
            let updated = self.forget_mutable_record_fields(s, &actual)?;
            if matches!(contract.borrowed, Ty::Aggregate(_)) {
                let Value::Address(place) = &c.args[contract.borrow as usize].kind else {
                    return Err(error("memory proof mutable record call needs a direct borrowed place"));
                };
                let fields = place.projections.iter().map(|projection| match projection {
                    Projection::Field { ordinal } => Ok(*ordinal as usize),
                    _ => Err(error("memory proof mutable record call requires field projections")),
                }).collect::<Result<Vec<_>>>()?;
                s.locals.get_mut(&place.root).and_then(|root| root.store(&fields, updated.clone()))
                    .ok_or_else(|| error("memory proof mutable record call could not update its place"))?;
            }
            bindings.insert(contract.borrow, updated);
        }

        // Every buffer the callee may write gets fresh cells, not only its contract's focus
        // (review: a helper writing two buffers left the other's old cells beside the callee's
        // ensures about them, and what followed was proved from the contradiction).
        let written:Vec<u32>=self.program.functions[c.definition.0 as usize].params.iter().enumerate().filter(|(_,t)|matches!(t,Ty::Borrow{mutable:true,..})).filter_map(|(i,_)|match bindings.get(&(i as u32)){Some(Symbol::Buffer(id))=>Some(*id),_=>None}).collect();
        for id in written{if !self.foci[id as usize].mutable{return Err(error("memory proof callee writes a buffer this function holds only shared"));}if contract.mutable&&matches!(bindings.get(&contract.borrow),Some(Symbol::Buffer(focus)) if *focus==id){continue;}let fresh=self.ghost("Nat → Option α")?;s.cells.insert(id,fresh);}
        let arms:Vec<(Option<u8>,Ty)>=match self.program.types.result_sum(contract.result){Some(sum)=>sum.arms.iter().enumerate().map(|(i,t)|(Some(i as u8),*t)).collect(),None=>vec![(None,contract.result)]};
        let mut out=vec![];
        for(arm,ty)in arms{self.count()?;let mut facts=vec![];
            let payload=if ty==contract.element&&!matches!(self.contract.element,Ty::Param{..}){Symbol::Nat(self.ghost("Nat")?)}
                else if ty==contract.element{let slot=self.ghost("Option α")?;facts.push(format!("{slot} ≠ none"));Symbol::Slot(slot)}else if ty==Ty::Unit{Symbol::Unit}
                else if ty==Ty::U64{let n=self.ghost("Nat")?;self.ranges.push(n.clone());Symbol::Nat(n)}else if ty==Ty::Bool{Symbol::Prop(self.ghost("Prop")?)}
                else if let Ty::Slice(id) = ty {
                    let length = self.ghost("Nat")?;
                    self.ranges.push(length.clone());
                    let contents = if self.contract.element == Ty::Unsigned(Scalar::U8)
                        && self.program.types.slice_element(id) == self.contract.element
                    {
                        let bytes = self.ghost("Nat → Nat")?;
                        Some(format!("(fun k : Nat => some ({bytes} k))"))
                    } else {
                        None
                    };
                    Symbol::Slice(length, contents)
                }
                else{return Err(error("memory proof helper returns an unsupported shape"));};
            let result=match arm{Some(arm)=>Symbol::Sum(arm,Box::new(payload.clone())),None=>payload.clone()};
            let mut named=bindings.clone();if arm.is_some()||ty==Ty::U64||matches!(ty,Ty::Slice(_)){named.insert(u32::MAX,payload.clone());}
            for(selected,p)in &contract.ensures{if selected.is_some()&&*selected!=arm{continue;}for(text,_)in self.flat(p,&named,&s.cells,&old_bindings,&old_cells,Some(&payload))?{facts.push(text);}}
            for(_,p)in &contract.maintains{for(text,_)in self.flat(p,&named,&s.cells,&old_bindings,&old_cells,Some(&payload))?{facts.push(text);}}
            out.push((facts,result));
        }Ok(out)
    }
    fn run(&mut self,body:&[Stmt],mut states:Vec<State>)->Result<Vec<State>>{for stmt in body{let mut next=vec![];for mut s in states{self.count()?;match stmt{
        Stmt::Set(id,e)=>{let v=self.value(&mut s,e)?;s.locals.insert(*id,v);next.push(s)},Stmt::Eval(e)=>{self.discarded(&mut s,e)?;next.push(s)},
        Stmt::Return(e, _, _)=>if let Value::Call(c)=&e.kind{for(facts,v)in self.call(&mut s,c)?{let mut after=s.clone();after.path.extend(facts);self.returned(&after,&v)?;}}else{let v=self.value(&mut s,e)?;self.returned(&s,&v)?;},
        // A checked take either moves the value out and leaves the cell empty, or changes nothing.
        Stmt::Match{value,arms} if matches!(&value.kind,Value::Storage{op:StorageOp::Take,value:None,..})&&arms.len()==2=>{
            let Value::Storage{receiver,index,..}=&value.kind else{unreachable!("guarded")};
            let Symbol::Buffer(id)=self.value(&mut s,receiver)? else{return Err(error("memory proof Take receiver requires buffer"));};
            if !self.foci[id as usize].mutable{return Err(error("memory proof Take requires exclusive borrowed focus"));}
            let i=nat(&self.value(&mut s,index)?)?.to_string();let cap=self.foci[id as usize].cap.clone();let old=s.cells[&id].clone();self.indices.insert(i.clone());
            let present=format!("({i} < {cap} ∧ {old} {i} ≠ none)");
            let mut taken=s.clone();taken.path.push(present.clone());
            let updated=self.bind("Nat → Option α",format!("(fun j : Nat => if j = {i} then none else {old} j)"))?;taken.cells.insert(id,updated);
            if let Some(binding)=arms[0].binding{taken.locals.insert(binding,Symbol::Slot(format!("{old} {i}")));}
            next.extend(self.run(&arms[0].body,vec![taken])?);
            s.path.push(format!("¬ {present}"));let code=self.ghost("Nat")?;self.ranges.push(code.clone());
            if let Some(binding)=arms[1].binding{s.locals.insert(binding,Symbol::Nat(code));}
            next.extend(self.run(&arms[1].body,vec![s])?);
        },
        Stmt::Match{value,arms}=>{let outcomes=if let Value::Call(c)=&value.kind{self.call(&mut s,c)?}else{vec![(vec![],self.value(&mut s,value)?)]};
            for(facts,v)in outcomes{let Symbol::Sum(arm,payload)=v else{return Err(error("memory proof match requires an actual Result"));};let mut after=s.clone();after.path.extend(facts);
                let chosen=&arms[arm as usize];if let Some(id)=chosen.binding{after.locals.insert(id,*payload);}next.extend(self.run(&chosen.body,vec![after])?);}},
        Stmt::StorePlace(p,op,e)=>{self.store(&mut s,p,*op,e)?;next.push(s);},
        Stmt::If(e,a,b)=>{let guard=prop(&self.value(&mut s,e)?)?.to_string();if guard!="False"{let mut yes=s.clone();if guard!="True"{yes.path.push(guard.clone());}next.extend(self.run(a,vec![yes])?);}if guard!="True"{if guard!="False"{s.path.push(format!("¬ {guard}"));}next.extend(self.run(b,vec![s])?);}},
        Stmt::While(condition,body,invariants)=>{self.seen_loops.insert(stmt as *const Stmt as usize);next.push(self.loop_body(condition,body,&invariants.memory,s)?);},
        Stmt::Expire(_)=>next.push(s),_=>return Err(error("memory proof unsupported statement context")),
    }}if next.len()>4096{return Err(error("memory proof pending state limit"));}states=next;}Ok(states)}
    fn returned(&mut self,s:&State,v:&Symbol)->Result<()>{
        let fixed=!matches!(self.contract.element,Ty::Param{..});
        // A slice result is opaque here: what made it was proved where it was made.
        let valid=if matches!(self.contract.result,Ty::Slice(_)){matches!(v,Symbol::Slice(..))}else if self.contract.result==self.contract.element{matches!(v,Symbol::Slot(_))||(fixed&&matches!(v,Symbol::Nat(_)))}else if self.contract.result==Ty::Unit{matches!(v,Symbol::Unit)}else if self.contract.result==Ty::Bool{matches!(v,Symbol::Prop(_))}else if self.contract.result==Ty::U64{matches!(v,Symbol::Nat(_))}else{let arms=self.program.types.result_sum(self.contract.result).ok_or_else(||error("memory proof missing Result type"))?.arms;match v{Symbol::Sum(a,p) if (*a as usize)<arms.len()=>match arms[*a as usize]{Ty::Unit=>matches!(p.as_ref(),Symbol::Unit),Ty::U64=>matches!(p.as_ref(),Symbol::Nat(_)),t if t==self.contract.element=>matches!(p.as_ref(),Symbol::Slot(_))||(fixed&&matches!(p.as_ref(),Symbol::Nat(_))),_=>false},_=>false}};
        if !valid{return Err(error("memory proof return must match actual generic/Unit/Result arm"));}let mut bindings=self.entry_bindings(s);let before=self.entry.clone();let old_cells=self.entry_cells();if let Symbol::Sum(_,payload)=v{bindings.insert(u32::MAX,*payload.clone());}
        if self.contract.result==Ty::U64{bindings.insert(u32::MAX,v.clone());}
        if let Symbol::Slice(..) = v {
            bindings.insert(u32::MAX, v.clone());
        }
        for(selected,p)in self.contract.ensures.clone(){if selected.is_some_and(|a|!matches!(v,Symbol::Sum(b,_) if a==*b)){continue;}
            // `!result || P` at a literal return: nothing to show for false, and P's own
            // leaves for true, so quantified parts keep their per-index reasoning.
            let p=match(&p,v){(Predicate::Binary("∨",guard,rest),Symbol::Prop(value)) if matches!(guard.as_ref(),Predicate::Not(inner) if matches!(inner.as_ref(),Predicate::Result))=>match value.as_str(){"False"=>continue,"True"=>rest.as_ref().clone(),_=>p},_=>p};
            for(text,index)in self.flat(&p,&bindings,&s.cells,&before,&old_cells,Some(v))?{self.check_index(s,text,index)?;}}
        // In the file that declares a sealed type, an exclusive borrow is handed back satisfying the invariant.
        for(module,p)in self.contract.maintains.clone(){if module!=self.contract.module{continue;}
            for(text,index)in self.flat(&p,&bindings,&s.cells,&before,&old_cells,Some(v))?{self.check_index(s,text,index)?;}}Ok(())
    }
    fn entry_bindings(&self,s:&State)->BTreeMap<u32,Symbol>{s.locals.iter().filter(|(id,_)|(**id as usize)<self.function.params.len()).map(|(i,v)|(*i,v.clone())).collect()}
    fn render(&mut self,checks:&[Check],identity:SourceIdentity)->Result<Goal>{
        let mut statement="∀ (α Key : Type) ".to_string();let mut intros="α Key".to_string();
        if self.uses_keys { statement.push_str("(encode : Key → Nat) "); intros.push_str(" encode"); }
        for f in &self.foci{append(&mut statement,&format!("({} : Key) ({} : Nat) ({} : Nat → Option α) ",f.key,f.cap,f.entry))?;intros.push_str(&format!(" {} {} {}",f.key,f.cap,f.entry));}for(n,t)in &self.ghosts{append(&mut statement,&format!("({n} : {t}) "))?;intros.push_str(&format!(" {n}"));}
        append(&mut statement,", ")?;for(i,f)in self.foci.iter().enumerate(){append(&mut statement,&format!("{} ≤ {} → (∀ j : Nat, {} ≤ j → {} j = none) → ",f.cap,u64::MAX,f.cap,f.entry))?;intros.push_str(&format!(" hcap{i} hout{i}"));}for(i,n)in self.ranges.iter().enumerate(){append(&mut statement,&format!("{n} ≤ {} → ",u64::MAX))?;intros.push_str(&format!(" hb{i}"));}for(i,p)in self.requires.iter().enumerate(){append(&mut statement,&format!("({p}) → "))?;intros.push_str(&format!(" hr{i}"));}for(n,t,v,_)in &self.terms{append(&mut statement,&format!("let {n} : {t} := {v}; "))?;intros.push_str(&format!(" {n}"));}
        // Factor only equal complete paths. Every original leaf remains a
        // separate conjunct; no premise crosses from one path into another.
        let mut groups:Vec<(Vec<String>,Vec<&Check>)>=vec![];
        for c in checks{if let Some((_,leaves))=groups.iter_mut().find(|(path,_)|path==&c.path){leaves.push(c);}else{groups.push((c.path.clone(),vec![c]));}}
        for(path,leaves)in &groups{append(&mut statement,"(")?;for p in path{append(&mut statement,&format!("({p}) → "))?;}
            if leaves.len()==1{append(&mut statement,&leaves[0].text)?;}else{for c in leaves{append(&mut statement,&format!("({}) ∧ ",c.text))?;}append(&mut statement,"True")?;}append(&mut statement,") ∧ ")?;
        }append(&mut statement,"True")?;
        let mut prover=format!("  intros {intros}\n");if !self.terms.is_empty(){append(&mut prover,&format!("  try dsimp only [{}] at *\n",self.terms.iter().map(|(n,_,_,_)|n.as_str()).collect::<Vec<_>>().join(", ")))?;}append(&mut prover,"  refine ⟨")?;for _ in &groups{append(&mut prover,"?_, ")?;}append(&mut prover,"True.intro⟩\n")?;
        for(path,leaves)in &groups{append(&mut prover,"  · ")?;if !path.is_empty(){append(&mut prover,&format!("intros {}\n    ",(0..path.len()).map(|i|format!("h{i}")).collect::<Vec<_>>().join(" ")))?;}
            // These checked arithmetic facts depend only on the shared path,
            // not on a particular check or its quantified cell index.
            // An index this path never mentions cannot bear on its leaves.
            let mut text=String::new();for p in path{text.push_str(p);text.push('\n');}for c in leaves{text.push_str(&c.text);text.push('\n');}
            for(name,_,term,_)in self.terms.iter().rev(){if super::record_model::mentions(&text,name){text.push_str(term);text.push('\n');}}
            // Splitting on `j = n` only helps where a cell is read or written at `n`, which
            // appears in the text as an application `cells n` or as the update `if j = n` of
            // the cells after a write.
            let applied=|n:&str|text.match_indices(n).any(|(at,_)|{let before=&text[..at];let after=&text[at+n.len()..];
                (before.ends_with("if j = ")||before.ends_with(' ')&&before[..before.len()-1].chars().next_back().is_some_and(|c|c.is_alphanumeric()||c=='_'))&&!after.chars().next().is_some_and(|c|c.is_alphanumeric()||c=='_')});
            let relevant=self.indices.iter().map(|n|applied(n)).collect::<Vec<_>>();
            for(i,n)in self.indices.iter().enumerate(){if !relevant[i]{continue;}for(f,focus)in self.foci.iter().enumerate(){
                self.nodes=self.nodes.saturating_add(1);if self.nodes>4096{return Err(error("memory proof bound tactic complexity limit"));}
                append(&mut prover,&format!("try have hbound{i}_{f} : {n} < {} := by witchy_omega\n    ",focus.cap))?;
            }}
            // A quantified premise of the function is put to use at each cell the path touches.
            let quantified=self.requires.iter().enumerate().filter(|(_,p)|p.starts_with("∀ j : Nat,")).map(|(i,p)|(i,p.clone())).collect::<Vec<_>>();
            // Clauses that say exactly which cells hold a value are settled cell by cell: split on
            // whether the entry cell holds one, then read the shared terms through so that the
            // instances of the clause and the goal name the same cells.
            let shared=self.terms.iter().map(|(n,_,_,_)|n.clone()).collect::<BTreeSet<_>>();
            let stated=self.occupancy.clone();let occupancy=|texts:&mut dyn Iterator<Item=&String>|{let mut read=BTreeSet::new();for t in texts{if let Some(r)=stated.get(t){read.extend(r.iter().filter(|c|!shared.contains(*c)).cloned());}}read};
            let premised=occupancy(&mut quantified.iter().map(|(_,p)|p));
            // The cells a leaf is about: those it names, directly or through the shared terms it uses.
            let definitions=self.terms.iter().map(|(n,_,t,_)|(n.clone(),t.clone())).collect::<Vec<_>>();
            let about=|text:&str,cells:&str|{let mut seen=vec![text.to_string()];let mut i=0;while i<seen.len()&&i<64{
                if super::record_model::mentions(&seen[i],cells){return true;}
                for(n,t)in &definitions{if super::record_model::mentions(&seen[i],n)&&!seen.contains(t){seen.push(t.clone());}}i+=1;}false};
            let unfold=if self.terms.is_empty(){String::new()}else{format!("(try dsimp only [{}] at *) <;> ",self.terms.iter().map(|(n,_,_,_)|n.as_str()).collect::<Vec<_>>().join(", "))};
            for(r,_)in &quantified{for(i,n)in self.indices.iter().enumerate(){if !relevant[i]{continue;}
                self.nodes=self.nodes.saturating_add(1);if self.nodes>4096{return Err(error("memory proof bound tactic complexity limit"));}
                // The premise's own bound is arithmetic; with it shown, the cell fact stands alone.
                append(&mut prover,&format!("have hr{r}_{i} := hr{r} {n}\n    try have hrd{r}_{i} := hr{r}_{i} (by witchy_omega)\n    "))?;}}
            for (j, fact) in path.iter().enumerate() {
                if !fact.starts_with("∀ j : Nat,") { continue; }
                for (i, index) in self.indices.iter().enumerate().filter(|(i, _)| relevant[*i]) {
                    self.nodes = self.nodes.saturating_add(1);
                    if self.nodes > 4096 { return Err(error("memory proof bound tactic complexity limit")); }
                    append(&mut prover, &format!("try have hcell{j}_{i} := h{j} {index} (by witchy_omega)\n    "))?;
                }
            }
            if !premised.is_empty()&&!unfold.is_empty(){append(&mut prover,&format!("{}\n    ",unfold.trim_end_matches(" <;> ")))?;}
            if leaves.len()>1{append(&mut prover,"refine ⟨")?;for _ in leaves{append(&mut prover,"?_, ")?;}append(&mut prover,"True.intro⟩\n")?;}
            for c in leaves{let mut step=String::new();
                if let Some(tactic)=&c.tactic{append(&mut step,&format!("{tactic}\n"))?;if leaves.len()==1{append(&mut prover,&step)?;}else{append(&mut prover,"    · ")?;append(&mut prover,&step.replace("\n    ","\n      "))?;}continue;}
                // What is to be shown is often a premise word for word: a type invariant kept as it was.
                if self.requires.contains(&c.text){append(&mut step,"assumption\n")?;if leaves.len()==1{append(&mut prover,&step)?;}else{append(&mut prover,"    · ")?;append(&mut prover,&step)?;}continue;}
                if c.index{append(&mut step,"intro j\n    try intro hjbound\n    ")?;for(i,_)in self.foci.iter().enumerate(){self.nodes=self.nodes.saturating_add(1);if self.nodes>4096{return Err(error("memory proof index case complexity limit"));}append(&mut step,&format!("have houtj{i} := hout{i} j\n    "))?;}
                    for(i,p)in c.path.iter().enumerate(){if p.starts_with("∀ j : Nat,"){append(&mut step,&format!("have hp{i} := h{i} j\n    "))?;}}
                    for(r,_)in &quantified{append(&mut step,&format!("have hrj{r} := hr{r} j\n    "))?;}
                    let mut cases=self.indices.iter().enumerate().filter(|(i,_)|relevant[*i]).map(|(i,n)|format!("by_cases hj{i} : j = {n}")).collect::<Vec<_>>();
                    let occupied=occupancy(&mut std::iter::once(&c.text).chain(&c.path).chain(quantified.iter().map(|(_,p)|p))).into_iter().filter(|cells|about(&c.text,cells)).collect::<BTreeSet<_>>();
                    let bounds=c.path.iter().chain(quantified.iter().map(|(_,p)|p)).filter_map(|p|self.quantified_bounds.get(p)).collect::<BTreeSet<_>>();// An occupancy clause carries its own bound, which `hjbound` supplies.
                    if occupied.is_empty(){for(i,n)in bounds.into_iter().enumerate(){cases.push(format!("by_cases hlt{i} : j < {n}"));}}
                    // Whether the cell lies inside each allocation matters only when a fact about the cells
                    // beyond a capacity is needed. That is rare, so those splits are a second attempt.
                    let mut beyond=vec![];
                    if self.empty_quantifiers.contains(&c.text)||c.path.iter().any(|p|self.empty_quantifiers.contains(p)){for(i,f)in self.foci.iter().enumerate(){beyond.push(format!("by_cases hcapj{i} : j < {}",f.cap));}}
                    // A clause that says exactly which cells hold a value is settled cell by cell.
                    let names=cases.iter().filter_map(|c|c.strip_prefix("by_cases ").and_then(|c|c.split_once(" : ")).map(|(n,_)|n.to_string())).collect::<Vec<_>>();
                    for(i,cells)in occupied.iter().enumerate(){cases.push(format!("by_cases hoc{i} : {cells} j = none"));}
                    // With the cell's occupancy and the written index decided, the goal and the clause's
                    // instance at `j` reduce to arithmetic.
                    let close=if occupied.is_empty(){"first | (constructor <;> witchy_close) | witchy_close".to_string()}else{
                        let mut facts=names;facts.extend((0..occupied.len()).map(|i|format!("hoc{i}")));
                        // The premises' instances at `j`, and the path's (a loop invariant, say).
                        let instances=quantified.iter().map(|(r,_)|format!("hrj{r}")).chain(c.path.iter().enumerate().filter(|(_,p)|p.starts_with("∀ j : Nat,")).map(|(i,_)|format!("hp{i}"))).collect::<Vec<_>>().join(" ");
                        format!("{unfold}first | ((try simp only [{}, {OCCUPANCY_SIMP}] at {instances} ⊢) <;> witchy_omega) | witchy_close",facts.join(", "))};
                    let all=cases.len()+beyond.len();
                    let branches=1usize.checked_shl(all as u32).unwrap_or(usize::MAX);if branches>4096{return Err(error("memory proof index case complexity limit"));}self.nodes=self.nodes.saturating_add(branches);if self.nodes>4096{return Err(error("memory proof index case complexity limit"));}
                    if beyond.is_empty(){
                        if !cases.is_empty(){append(&mut step,&format!("{}\n    all_goals\n      ",cases.join(" <;> ")))?;}
                        append(&mut step,&format!("{close}\n"))?;
                    }else{
                        let inner=cases.iter().map(|c|format!("{c} <;> ")).collect::<String>();let outer=cases.iter().chain(&beyond).map(|c|format!("{c} <;> ")).collect::<String>();
                        append(&mut step,&format!("first | ({inner}{close}) | ({outer}{close})\n"))?;
                    }
                }else{
                    // Occupancy clauses this path assumes (a loop invariant, say), by position.
                    let guards=c.path.iter().enumerate().filter_map(|(i,p)|stated.get(p).map(|r|(i,r.iter().filter(|c|!shared.contains(*c)).cloned().collect::<Vec<_>>()))).collect::<Vec<_>>();
                    if c.text.contains("Witchy.Utf8.ValidCells") {
                        let premises = self.requires.iter().enumerate()
                            .filter(|(_, fact)| fact.contains("Witchy.Utf8.ValidCells"))
                            .map(|(i, _)| format!("hr{i}"))
                            .chain(c.path.iter().enumerate()
                                .filter(|(_, fact)| fact.starts_with("Witchy.Utf8.ValidCells "))
                                .map(|(i, _)| format!("h{i}")))
                            .take(8).collect::<Vec<_>>();
                        append(&mut step, "first | assumption")?;
                        append(&mut step, &format!(" | (apply Witchy.Utf8.ValidCells.asciiRange; intro byte_index byte_bound; {unfold}(simp only [Nat.zero_add]) <;> (first | assumption | (split <;> simp_all <;> witchy_inline_omega)))"))?;
                        let appends = premises.iter().flat_map(|left| {
                            premises.iter().map(move |right| {
                                format!(" | exact Witchy.Utf8.ValidCells.appendRange {left} {right}")
                            })
                        }).collect::<String>();
                        append(&mut step, &appends)?;
                        if premises.iter().any(|name| name.starts_with('h') && !name.starts_with("hr")) {
                            // A checked subrange names its length by subtraction. Normalize
                            // caller-supplied equalities before comparing the append result.
                            append(&mut step, &format!(
                                " | ({unfold}subst_vars; (try simp only [Nat.add_sub_cancel_left] at *); first {appends})"
                            ))?;
                        }
                        let slice_equalities = self.requires.iter().enumerate()
                            .map(|(i, fact)| (format!("hr{i}"), fact))
                            .chain(c.path.iter().enumerate().map(|(i, fact)| (format!("h{i}"), fact)))
                            .filter(|(_, fact)| fact.starts_with("∀ j : Nat, j < ")
                                && fact.contains(" = ") && !fact.contains("∧"))
                            .take(8);
                        for (equality, _) in slice_equalities {
                            for valid in &premises {
                                append(&mut step, &format!(
                                    " | (subst_vars; exact Witchy.Utf8.ValidCells.ofSliceContents {equality} {valid})"
                                ))?;
                            }
                        }
                        for (index, fact) in c.path.iter().enumerate() {
                            if !fact.starts_with("∀ j : Nat,") || !fact.contains("∧") {
                                continue;
                            }
                            for premise in &premises {
                                append(&mut step, &format!(
                                    " | (apply Witchy.Utf8.ValidCells.takeCopied h{index} {premise} <;> (first | (intro h; simp_all) | (simp_all <;> witchy_inline_omega)))"
                                ))?;
                            }
                        }
                        for premise in &premises {
                            append(&mut step, &format!(" | (apply Witchy.Utf8.ValidCells.copyPrefix (Witchy.Utf8.ValidCells.takeAtBoundary {premise} (by witchy_inline_omega) (by intro h; simpa using (show _ from by assumption))); intro j hj; first | solve_by_elim {} )", c.path.iter().enumerate().filter(|(_, text)| text.starts_with("∀ j : Nat,") && text.contains("∧")).map(|(i, _)| format!("| exact (h{i} j (by witchy_inline_omega)).1")).collect::<Vec<_>>().join(" ")))?;
                        }
                        append(&mut step, " | (apply Witchy.Utf8.ValidCells.takeAtBoundary <;> (first | assumption | (intro h; simpa using (show _ from by assumption)) | witchy_inline_omega)) | (simp [Witchy.Utf8.ValidCells, Witchy.Utf8.bytesAt]; exact (Witchy.Utf8.Scalar.ascii (by witchy_inline_omega)).valid) | witchy_close\n")?;
                    } else if c.text.contains("Witchy.Utf8.Continuation") {
                        append(&mut step, &format!("{unfold}(unfold Witchy.Utf8.Continuation; witchy_inline_omega)\n"))?;
                    } else if premised.is_empty()&&guards.is_empty(){append(&mut step,"witchy_close\n")?;}else{
                    // A fact about one cell follows from a clause once that cell's occupancy is known.
                    let mut cells=premised.iter().filter(|cells|about(&c.text,cells)).cloned().collect::<Vec<_>>();for(_,g)in &guards{for cell in g{if !cells.contains(cell)&&about(&c.text,cell){cells.push(cell.clone());}}}
                    let splits=if cells.is_empty(){String::new()}else{self.indices.iter().enumerate().filter(|(i,n)|relevant[*i]&&about(&c.text,n)).flat_map(|(_,n)|{
                        let at=guards.iter().map(|(i,_)|format!("(try have hpn{i} := h{i} {n} (by witchy_omega)) <;> ")).collect::<String>();
                        std::iter::once(at).chain(cells.iter().map(move |cells|format!("by_cases hoc : {cells} {n} = none <;> ")).collect::<Vec<_>>())
                    }).collect::<String>()};
                    // With the cell's occupancy decided, its clause instances read as arithmetic.
                    let decided=if splits.is_empty(){String::new()}else{format!("first | ((try simp only [hoc, {RECORD_OCCUPANCY_SIMP}] at *) <;> witchy_omega) | ")};
                    append(&mut step,&format!("first | exact True.intro | assumption | (with_reducible rfl) | witchy_omega | ({splits}{unfold}{decided}witchy_close) | witchy_close\n"))?;
                }}
                if leaves.len()==1{append(&mut prover,&step)?;}else{append(&mut prover,"    · ")?;append(&mut prover,&step.replace("\n    ","\n      "))?;}
            }
        }
        // With a fixed element type the cells hold numbers; there is no type to abstract over.
        let(statement,prover)=if matches!(self.contract.element,Ty::Param{..}){(statement,prover)}else{(statement.replacen("∀ (α Key : Type)","∀ (Key : Type)",1).replace('α',"Nat"),prover.replacen("intros α ","intros ",1))};
        Ok(Goal{source_name:self.function.name.clone(),statement,nodes:self.nodes,source:Some(identity),prover:Some(prover)})
    }
}
fn pure_guard(e:&Expr)->bool{match &e.kind{Value::StaticStr(_) | Value::Unit|Value::U64(_)|Value::Bool(_)=>true,Value::ReadPlace(p)|Value::Address(p)=>p.projections.iter().all(|p|matches!(p,Projection::Field{..})),Value::AllocationKey(v,_)|Value::Capacity(v,_)|Value::Not(v)=>pure_guard(v),Value::Binary(_,a,b)=>pure_guard(a)&&pure_guard(b),Value::Storage{op:StorageOp::Initialized,receiver,index,value:None,..}=>pure_guard(receiver)&&pure_guard(index),_=>false}}
fn contains_loop(body:&[Stmt])->bool{body.iter().any(|s|match s{Stmt::While(..)=>true,Stmt::If(_,a,b)=>contains_loop(a)||contains_loop(b),Stmt::Match{arms,..}=>arms.iter().any(|a|contains_loop(&a.body)),_=>false})}
fn nat(s:&Symbol)->Result<&str>{if let Symbol::Nat(n)=s{Ok(n)}else{Err(error("memory proof expected U64 symbolic value"))}}
fn prop(s:&Symbol)->Result<&str>{if let Symbol::Prop(n)=s{Ok(n)}else{Err(error("memory proof expected Bool symbolic value"))}}
fn conjunction(v:Vec<String>)->String{if v.is_empty(){"True".into()}else{format!("({})",v.into_iter().map(|s|format!("({s})")).collect::<Vec<_>>().join(" ∧ "))}}
fn append(out:&mut String,s:&str)->Result<()>{if out.len().saturating_add(s.len())>super::MAX_SOURCE_BYTES{return Err(error("memory proof statement/prover byte limit"));}out.push_str(s);Ok(())}
fn identity(f:&Function,kind:&'static str,callee:Option<String>,site:Option<u32>)->SourceIdentity{SourceIdentity{kind,function:f.name.clone(),callee,site,storage:false,record:false,memory:true}}
#[cfg(test)]
pub(super) fn prepare(program:&Program)->Result<Vec<Goal>>{Ok(prepare_proved(program)?.0)}
/// The goals, and the functions whose every unsigned `+` and `-` the proof shows in range (each
/// such node of the body is one the model walked and checked, there is no `*`, and no statement
/// does arithmetic as it stores): their checks are not emitted and they cannot overflow.
pub(super) fn prepare_proved(program:&Program)->Result<(Vec<Goal>,BTreeSet<u32>)>{
    let mut proved=BTreeSet::new();
    let selected=program.functions.iter().map(|f|f.contract.as_ref().and_then(|c|c.memory.as_ref())).collect::<Vec<_>>();
    fn visit(i:usize,program:&Program,selected:&[Option<&MemoryContract>],marks:&mut[u8],depth:usize)->Result<()>{if depth>64||marks[i]==1{return Err(error("memory proof recursive or too-deep contract graph"));}if marks[i]==2{return Ok(());}marks[i]=1;for c in &program.functions[i].calls{let j=c.definition.0 as usize;if selected[j].is_some(){visit(j,program,selected,marks,depth+1)?;}}marks[i]=2;Ok(())}
    let mut marks=vec![0;program.functions.len()];for(i,c)in selected.iter().enumerate(){if c.is_some(){visit(i,program,&selected,&mut marks,0).map_err(|e|super::proof_advice::at_function(e,&program.functions[i]))?;}}
    let mut goals=vec![];for(i,f)in program.functions.iter().enumerate(){
        // A refusal with no place of its own is placed at the function (`proof_advice::at_function`).
        (||->Result<()>{let mut inventory=Inventory::default();inventory.body(&f.body,program);
        let Some(contract)=selected[i] else{if !inventory.unsafe_nodes.is_empty(){
            let record=f.contract.as_ref().and_then(|c|c.record.as_ref()).ok_or_else(||error("unchecked memory operation requires an explicit supported contract and proof auto, even in unused/dead code"))?;
            super::record_model::body(f,record,&program.types)?;
        }let proved=f.contract.as_ref().is_some_and(|c|c.record.is_some());for(site,callee)in inventory.calls{
            // A body proved over owned records shows the requirement inside its own proof.
            if !proved&&!selected[callee.0 as usize].expect("memory callee").requires.is_empty(){return Err(error("memory proof nontrivial caller requires explicit supported proof context"));}goals.push(Goal{source_name:f.name.clone(),statement:"True".into(),nodes:1,source:Some(identity(f,"call_precondition",Some(program.functions[callee.0 as usize].name.clone()),Some(site))),prover:None});}return Ok(());};
        let mut m=Model{program,function:f,contract,ghosts:vec![],ranges:vec![],terms:vec![],checks:vec![],calls:inventory.calls.keys().map(|s|(*s,vec![])).collect(),seen:BTreeSet::new(),seen_loops:BTreeSet::new(),nodes:contract.nodes,bytes:0,expanded:0,requires:vec![],entry:BTreeMap::new(),foci:vec![],indices:BTreeSet::new(),quantified_bounds:BTreeMap::new(),empty_quantifiers:BTreeSet::new(),uses_keys:false,occupancy:BTreeMap::new(),in_loop:false,shown:BTreeSet::new()};
        let mut locals=BTreeMap::new();for(id,t)in f.params.iter().enumerate(){locals.insert(id as u32,m.entry(*t,false)?);}m.entry=locals.clone();let cells=m.entry_cells();
        for p in &contract.requires {
            // Keep quantified premises separate so they can be instantiated at each checked index.
            for (text, _) in m.flat(p, &locals, &cells, &locals, &cells, None)? {
                m.requires.push(text);
            }
        }
        // A sealed value that arrives satisfies its type's invariant; each part is its own premise so the quantified ones can be used per index.
        for(_,p)in &contract.assumes{for(text,_)in m.flat(p,&locals,&cells,&locals,&cells,None)?{m.requires.push(text);}}let remaining=m.run(&f.body,vec![State{locals,path:vec![],cells}])?;if contract.result==Ty::Unit{for s in remaining{m.returned(&s,&Symbol::Unit)?;}}else if !remaining.is_empty(){return Err(error("memory proof requires every finite path to return"));}
        let dead=inventory.unsafe_nodes.difference(&m.seen).count()+inventory.loop_nodes.difference(&m.seen_loops).count();for _ in 0..dead{m.count()?;m.checks.push(Check{path:vec!["False".into()],text:"False".into(),index:false,tactic:None});}
        if !inventory.stores_arithmetic&&inventory.arithmetic.is_subset(&m.shown){proved.insert(f.definition.0);}
        let checks=m.checks.clone();let body=m.render(&checks,identity(f,"function_body",None,None))?;let mut total=body.statement.len();goals.push(body);
        for(site,callee)in inventory.calls{let mut checks=m.calls.remove(&site).expect("inventoried call");if checks.is_empty(){checks.push(Check{path:vec!["False".into()],text:"False".into(),index:false,tactic:None});}let goal=m.render(&checks,identity(f,"call_precondition",Some(program.functions[callee.0 as usize].name.clone()),Some(site)))?;total=total.saturating_add(goal.statement.len());if total>super::MAX_SOURCE_BYTES{return Err(error("memory proof total caller statement limit"));}goals.push(goal);}
        Ok(())})().map_err(|e|super::proof_advice::at_function(e,f))?;
    }Ok((goals,proved))
}
