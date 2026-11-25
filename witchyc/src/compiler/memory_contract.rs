//! Shared and exclusive generic memory clauses over resolved borrowed field ordinals.
use super::{logical_body::{FunctionId,Ty},semantic_types::{Types,Kind},storage_contract::{error,method}};
use crate::{ast as a,diagnostic::Diagnostic};
use std::collections::BTreeMap;
type Result<T>=std::result::Result<T,Diagnostic>;
#[derive(Clone,Debug)]
pub(super) struct Path{pub root:u32,pub fields:Vec<usize>,pub ty:Ty,pub before:bool}
#[derive(Clone,Debug)]
pub(super) enum Number{Literal(u64),Path(Path),Capacity(Path),AllocationKey(Path),Add(Box<Self>,Box<Self>),
    /// The variable of the enclosing `forall`.
    Bound}
#[derive(Clone,Debug)]
pub(super) enum Element{Result,Path(Path)}
#[derive(Clone,Debug)]
pub(super) enum Predicate{Bool(bool),Compare(&'static str,Number,Number),Not(Box<Self>),Binary(&'static str,Box<Self>,Box<Self>),Initialized(Path,Number),Empty(Path,Number),Cell(Path,Number,Element),Same(Path,Path),Except(Path,Path,Number),Unchanged(Path,Path),From(Path,Path,Number),SamePrefix(Path,Path,Number),EmptyRange(Path,Number,Number),EmptyFrom(Path,Number),EmptyAll(Path),
    /// Every cell in `[lo, hi)` holds a value.
    FullRange(Path,Number,Number),
    Utf8(Path,Number,Number),
    SliceContents(Path,Path,Number),
    Utf8Boundary(Path,Number),
    /// `forall j in (lo, hi): body`: the body holds at every cell index in `[lo, hi)`.
    Forall(Number,Number,Box<Self>),
    /// The Bool a function returned.
    Result}
#[derive(Clone,Debug)]
pub(super) struct MemoryContract{pub requires:Vec<Predicate>,pub ensures:Vec<(Option<u8>,Predicate)>,pub nodes:usize,pub element:Ty,pub result:Ty,pub mutable:bool,pub borrow:u32,pub borrowed:Ty,
    /// Type invariants of the sealed values borrowed, with each type's declaring file: assumed on entry.
    pub assumes:Vec<(u32,Predicate)>,
    /// The same invariants over the final state of exclusive borrows: a body in the declaring file shows them.
    pub maintains:Vec<(u32,Predicate)>,
    /// The file the function was written in.
    pub module:u32}
fn shape(t:Ty,element:Ty,types:&Types,depth:usize,visits:&mut usize)->Option<usize>{
    *visits+=1;if depth>16||*visits>4096{return None;}match t{
        Ty::U64|Ty::Bool=>Some(0),t if t==element=>Some(0),Ty::Buffer(i) if types.buffers()[i as usize].element==element=>Some(1),
        Ty::Aggregate(_)=>{let r=types.get(t)?;if !matches!(r.kind,Kind::Record(_)){return None;}let mut owners=0;for f in &r.fields{owners+=shape(f.ty,element,types,depth+1,visits)?;}Some(owners)},_=>None,
    }
}
pub(super) fn eligible(owner:FunctionId,generics:usize,params:&[Ty],result:Ty,types:&Types)->bool{
    let Some(element)=super::record_contract::element(owner,generics,params,result,types) else{return false;};
    let mut borrows=0;let mut direct=true;let mut mutable=0;
    for t in params{match *t{t if t==Ty::U64||t==element=>{},Ty::Borrow{pointee:super::logical_body::Pointee::Slice(id),mutable:false} if types.slice_element(id)==element=>{},Ty::Borrow{pointee,mutable:m}=>{borrows+=1;mutable+=usize::from(m);direct&=matches!(pointee.ty(),Ty::Buffer(_));if !matches!(shape(pointee.ty(),element,types,0,&mut 0),Some(1..)){return false;}},_=>return false,}}
    // A slice of the elements (`Vec.as_slice`) is a result like a reference to one.
    let ordinary=matches!(result,Ty::Slice(i) if types.slice_element(i)==element)||result==element||result==Ty::Unit||result==Ty::Bool||result==Ty::U64||types.result_sum(result).is_some_and(|s|s.arms==[element,Ty::U64]||s.arms==[Ty::Unit,element]||s.arms==[element,element]);
    let unit_code_result=types.result_sum(result).is_some_and(|s|s.arms==[Ty::Unit,Ty::U64])&&!params.contains(&element);
    // Two direct buffers: one read and one written (a copy), or both written (a move). Or two
    // values holding buffers, one written and one read (`Vec.extend_copy_mut`): each borrowed
    // sealed value's invariant is assumed, and the written one's maintained, as for one.
    let unit_or_code=result==Ty::Unit||types.result_sum(result).is_some_and(|s|s.arms==[Ty::Unit,Ty::U64]);
    (borrows==1&&(ordinary||unit_code_result))||(borrows==2&&direct&&mutable>=1&&unit_or_code)||(borrows==2&&mutable==1&&unit_or_code)
}
struct Resolve<'a>{scope:&'a BTreeMap<String,(u32,Ty)>,types:&'a Types,element:Ty,result:Ty,post:bool,arm:Option<(String,Ty)>,nodes:usize,leaves:usize,
    /// Reading a type invariant: bare names are the fields of this value.
    base:Option<Path>,
    /// The variable of the `forall` being read.
    bound:Option<String>}
/// An expression that is true or false rather than a number.
fn truth(e:&a::Expr)->bool{match &e.kind{
    a::Expression::Bool(_)|a::Expression::Unary("!",_)|a::Expression::Forall{..}=>true,
    a::Expression::Binary(op,..)=>matches!(*op,"&&"|"||"|"<"|"<="|">"|">="|"=="|"!="),
    _=>method(e).is_some_and(|(m,_)|!matches!(m,"capacity"|"allocation_key")),
}}
impl Resolve<'_>{
    fn count(&mut self,depth:usize)->Result<()>{self.nodes+=1;if depth>64||self.nodes>4096{return Err(error("memory contracts expression complexity limit"));}Ok(())}
    fn path(&mut self,e:&a::Expr,depth:usize)->Result<Path>{self.count(depth)?;match &e.kind{
        a::Expression::Call(c,args) if matches!(&c.kind,a::Expression::Name(n) if n=="old")&&args.len()==1=>{if !self.post{return Err(error("memory contracts old is ensures-only"));}let mut p=self.path(&args[0],depth+1)?;if p.before||p.root==u32::MAX{return Err(error("memory contracts nested old or result snapshot refused"));}p.before=true;Ok(p)},
        a::Expression::Name(n) if self.base.is_some()=>{
            let mut p=self.base.clone().expect("invariant base");
            let(i,f)=self.types.get(p.ty).and_then(|r|r.fields.iter().enumerate().find(|(_,f)|f.name==super::spec_expand::member_scope(n).1)).ok_or_else(||error("a type invariant names only the type's own fields"))?;
            p.fields.push(i);p.ty=f.ty;Ok(p)
        },
        a::Expression::Name(n)=>{
            // A plain number or slice result is read like a result binder.
            if n == "result" && self.post && self.arm.is_none()
                && matches!(self.result, Ty::U64 | Ty::Slice(_))
            {
                return Ok(Path { root: u32::MAX, fields: vec![], ty: self.result, before: false });
            }
            if let Some((name,ty))=&self.arm {if n==name{return Ok(Path{root:u32::MAX,fields:vec![],ty:*ty,before:false});}}let(id,t)=self.scope.get(n).ok_or_else(||error("memory contracts unknown parameter path"))?;let ty=match t{Ty::Borrow{pointee,..}=>pointee.ty(),t=>*t};Ok(Path{root:*id,fields:vec![],ty,before:false})},
        a::Expression::Unary("*",v)=>{let a::Expression::Name(n)=&v.kind else{return Err(error("memory contracts dereference requires a borrowed parameter"));};if !matches!(self.scope.get(n),Some((_,Ty::Borrow{..}))){return Err(error("memory contracts dereference requires borrowed permission"));}self.path(v,depth+1)},
        a::Expression::Unary("&"|"&mut",v)=>self.path(v,depth+1),
        a::Expression::Field(v,n)=>{let mut p=self.path(v,depth+1)?;let r=self.types.get(p.ty).ok_or_else(||error("memory contracts field requires typed record"))?;let(i,f)=r.fields.iter().enumerate().find(|(_,f)|f.name==*n).ok_or_else(||error("memory contracts unknown field"))?;p.fields.push(i);p.ty=f.ty;Ok(p)},
        _=>Err(error("memory contracts require borrowed parameter/field paths")),
    }}
    fn buffer(&mut self,e:&a::Expr,d:usize)->Result<Path>{let p=self.path(e,d)?;if !matches!(p.ty,Ty::Buffer(i) if self.types.buffers()[i as usize].element==self.element){return Err(error("memory contracts require the generic borrowed Buffer leaf"));}Ok(p)}
    fn number(&mut self,e:&a::Expr,d:usize)->Result<Number>{self.count(d)?;Ok(match &e.kind{
        a::Expression::Integer(n)=>Number::Literal(*n),a::Expression::Name(n) if self.bound.as_deref()==Some(n.as_str())=>Number::Bound,
        // `result.len()` of a returned slice: how many cells it holds (memory_model `Symbol::Slice`).
        a::Expression::Call(c,args) if args.is_empty()&&self.post&&self.arm.is_none()&&matches!(self.result,Ty::Slice(_))
            &&matches!(&c.kind,a::Expression::Field(v,m) if m=="len"&&matches!(&v.kind,a::Expression::Name(n) if n=="result"))=>Number::Path(Path{root:u32::MAX,fields:vec![],ty:Ty::U64,before:false}),a::Expression::Call(c,args) if args.is_empty() && matches!(&c.kind,a::Expression::Field(_,m) if m=="len") => {
            let a::Expression::Field(receiver, _) = &c.kind else { unreachable!() };
            let path = self.path(receiver, d+1)?;
            if !matches!(path.ty, Ty::Slice(_)) { return Err(error("memory contract len() needs a slice")); }
            Number::Path(path)
        },
        a::Expression::Binary("+",a,b)=>Number::Add(Box::new(self.number(a,d+1)?),Box::new(self.number(b,d+1)?)),
        _=>if let Some(("allocation_key",[v]))=method(e){Number::AllocationKey(self.buffer(v,d+1)?)}else if let Some(("capacity",[v]))=method(e){Number::Capacity(self.buffer(v,d+1)?)}else{let p=self.path(e,d+1)?;if !matches!(p.ty, Ty::Unsigned(_)){return Err(error("memory contracts numeric path requires an unsigned integer"));}Number::Path(p)},
    })}
    fn element(&mut self,e:&a::Expr,d:usize)->Result<Element>{self.count(d)?;if matches!(&e.kind,a::Expression::Name(n) if n=="result"){
        if !self.post||self.result!=self.element{return Err(error("memory contracts scalar result is only available for direct T returns"));}return Ok(Element::Result);
    }let p=self.path(e,d+1)?;if p.ty!=self.element{return Err(error("memory contracts cell payload requires the generic element"));}Ok(Element::Path(p))}
    fn predicate(&mut self,e:&a::Expr,d:usize)->Result<Predicate>{self.count(d)?;Ok(match &e.kind{
        a::Expression::Bool(b)=>Predicate::Bool(*b),a::Expression::Unary("!",p)=>Predicate::Not(Box::new(self.predicate(p,d+1)?)),
        a::Expression::Name(n) if n=="result"&&self.post&&self.result==Ty::Bool&&self.arm.is_none()=>Predicate::Result,
        a::Expression::Binary(op@("&&"|"||"),a,b)=>Predicate::Binary(if *op=="&&"{"∧"}else{"∨"},Box::new(self.predicate(a,d+1)?),Box::new(self.predicate(b,d+1)?)),
        // Two truths are equal when each holds exactly when the other does.
        a::Expression::Binary(op@("=="|"!="),a,b) if truth(a)||truth(b)=>{let iff=Predicate::Binary("↔",Box::new(self.predicate(a,d+1)?),Box::new(self.predicate(b,d+1)?));if *op=="=="{iff}else{Predicate::Not(Box::new(iff))}},
        a::Expression::Forall{variable,lower,upper,body}=>{
            if self.bound.is_some(){return Err(error("memory contracts: a forall cannot sit inside another"));}
            if self.scope.contains_key(variable)||variable=="result"||self.arm.as_ref().is_some_and(|(n,_)|n==variable){return Err(error("memory contracts: a forall variable cannot hide another name"));}
            let lo=self.number(lower,d+1)?;let hi=self.number(upper,d+1)?;
            self.bound=Some(variable.clone());let body=self.predicate(body,d+1);self.bound=None;let body=body?;
            // A clause that is itself quantified would reuse the bound variable's name in the
            // statement and capture it; a body speaks of single cells and numbers only.
            fn pointwise(p:&Predicate)->bool{match p{Predicate::Bool(_)|Predicate::Compare(..)|Predicate::Initialized(..)|Predicate::Empty(..)|Predicate::Cell(..)=>true,Predicate::Not(p)=>pointwise(p),Predicate::Binary(_,a,b)=>pointwise(a)&&pointwise(b),_=>false}}
            if !pointwise(&body){return Err(error("memory contracts: a forall body speaks of single cells (`memory.initialized`, `memory.empty`, `memory.cell`) and numbers"));}
            Predicate::Forall(lo,hi,Box::new(body))
        },
        a::Expression::Binary(op@("=="|"!="|"<"|"<="|">"|">="),a,b)=>Predicate::Compare(match *op{"=="=>"=","!="=>"≠","<"=>"<","<="=>"≤",">"=>">",_=>"≥"},self.number(a,d+1)?,self.number(b,d+1)?),
        _=>match method(e){Some(("initialized",[b,i]))=>Predicate::Initialized(self.buffer(b,d+1)?,self.number(i,d+1)?),Some(("empty",[b,i]))=>Predicate::Empty(self.buffer(b,d+1)?,self.number(i,d+1)?),Some(("cell",[b,i,v]))=>Predicate::Cell(self.buffer(b,d+1)?,self.number(i,d+1)?,self.element(v,d+1)?),Some(("same_allocation",[a,b]))=>Predicate::Same(self.buffer(a,d+1)?,self.buffer(b,d+1)?),Some(("unchanged_except",[a,b,i]))=>Predicate::Except(self.buffer(a,d+1)?,self.buffer(b,d+1)?,self.number(i,d+1)?),Some(("unchanged",[a,b]))=>Predicate::Unchanged(self.buffer(a,d+1)?,self.buffer(b,d+1)?),Some(("unchanged_from",[a,b,i]))=>Predicate::From(self.buffer(a,d+1)?,self.buffer(b,d+1)?,self.number(i,d+1)?),Some(("same_prefix",[a,b,i]))=>Predicate::SamePrefix(self.buffer(a,d+1)?,self.buffer(b,d+1)?,self.number(i,d+1)?),Some(("initialized_range",[b,lo,hi]))=>Predicate::FullRange(self.buffer(b,d+1)?,self.number(lo,d+1)?,self.number(hi,d+1)?),Some(("utf8_boundary",[b,index]))=>{
            if self.element != Ty::Unsigned(super::scalar::Scalar::U8) {
                return Err(error("memory.utf8_boundary requires a U8 buffer"));
            }
            Predicate::Utf8Boundary(self.buffer(b,d+1)?,self.number(index,d+1)?)
        },Some(("slice_contents", [slice, buffer, start])) => {
            let slice = self.path(slice, d+1)?;
            if !matches!(slice.ty, Ty::Slice(id) if self.types.slice_element(id) == Ty::Unsigned(super::scalar::Scalar::U8))
                || self.element != Ty::Unsigned(super::scalar::Scalar::U8)
            {
                return Err(error("memory.slice_contents needs a modeled byte slice and U8 buffer"));
            }
            Predicate::SliceContents(slice, self.buffer(buffer, d+1)?, self.number(start, d+1)?)
        },Some(("utf8",[b,start,count]))=>{
            if self.element != Ty::Unsigned(super::scalar::Scalar::U8) {
                return Err(error("memory.utf8 requires a U8 buffer"));
            }
            let path = self.path(b,d+1)?;
            let bytes = match path.ty {
                Ty::Buffer(id) => self.types.buffers()[id as usize].element,
                Ty::Slice(id) => self.types.slice_element(id),
                _ => return Err(error("memory.utf8 needs a byte buffer or shared byte slice")),
            };
            if bytes != self.element {
                return Err(error("memory.utf8 needs a byte buffer or shared byte slice"));
            }
            Predicate::Utf8(path,self.number(start,d+1)?,self.number(count,d+1)?)
        },Some(("empty_range",[b,lo,hi]))=>Predicate::EmptyRange(self.buffer(b,d+1)?,self.number(lo,d+1)?,self.number(hi,d+1)?),Some(("empty_from",[b,lo]))=>Predicate::EmptyFrom(self.buffer(b,d+1)?,self.number(lo,d+1)?),Some(("prefix",[b,length,capacity]))=>{
            // The whole description of a filled prefix: the stated capacity is the allocation's,
            // the length fits, every cell below it holds a value and every cell from it on is empty.
            let(buffer,length,capacity)=(self.buffer(b,d+1)?,self.number(length,d+1)?,self.number(capacity,d+1)?);
            let and=|a,b|Predicate::Binary("∧",Box::new(a),Box::new(b));
            and(and(Predicate::Compare("=",capacity.clone(),Number::Capacity(buffer.clone())),Predicate::Compare("≤",length.clone(),capacity)),
                and(Predicate::FullRange(buffer.clone(),Number::Literal(0),length.clone()),Predicate::EmptyFrom(buffer,length)))
        },Some(("empty_all",[b]))=>Predicate::EmptyAll(self.buffer(b,d+1)?),Some(("frame_unchanged",_))=>return Err(error("`memory.frame_unchanged` was removed: it stated nothing. Say what is unchanged with `memory.unchanged` or `memory.unchanged_except`")),_=>return Err(error("memory contracts unsupported bounded memory predicate"))},
    })}
    fn leaf(&mut self,e:&a::Expr)->Result<Predicate>{self.leaves+=1;if self.leaves>32{return Err(error("memory contracts allow at most 32 clauses"));}self.predicate(e,0)}
}
pub(super) fn resolve(f:&a::Function,scope:&BTreeMap<String,(u32,Ty)>,types:&Types,result:Ty,owner:FunctionId)->Result<super::contract::Contract>{
    if scope.len()>32||scope.contains_key("result")||f.ensures.is_empty(){return Err(error("memory contracts require ensures, no result parameter, and at most 32 parameters"));}
    let parameters=scope.values().map(|(_,t)|*t).collect::<Vec<_>>();
    let element=super::record_contract::element(owner,f.generics.len(),&parameters,result,types).ok_or_else(||error("memory contracts require one element type"))?;
    let mut r=Resolve{scope,types,element,result,post:false,arm:None,nodes:0,leaves:0,base:None,bound:None};let requires=f.requires.iter().map(|e|r.leaf(e)).collect::<Result<Vec<_>>>()?;r.post=true;
    let mut ensures=vec![];
    for p in &f.ensures {match &p.kind {
        a::PostconditionKind::Predicate(e)=>ensures.push((None,r.leaf(e)?)),
        a::PostconditionKind::Match{value,arms}=>{
            if !matches!(&value.kind,a::Expression::Name(n) if n=="result")||arms.len()!=2{return Err(error("memory contracts require exhaustive match result"));}
            let result_arms=types.result_sum(result).ok_or_else(||error("memory contracts post match requires Result"))?.arms;let mut seen=[false;2];
            for arm in arms {let selected=match arm.variant.as_str(){"Ok"=>0,"Err"=>1,_=>return Err(error("memory contracts post match requires Ok/Err"))};
                if seen[selected]||arm.clauses.is_empty(){return Err(error("memory contracts duplicate or empty result arm"));}seen[selected]=true;
                if let Some(name)=&arm.binding {if name=="result"||scope.contains_key(name){return Err(error("memory contracts post arm binder must be fresh"));}r.arm=Some((name.clone(),result_arms[selected]));}
                else if result_arms[selected]!=Ty::Unit{return Err(error("memory contracts nonunit post arm requires binder"));}
                for clause in &arm.clauses {let a::PostconditionKind::Predicate(e)=&clause.kind else{return Err(error("memory contracts nested result matches unsupported"));};ensures.push((Some(selected as u8),r.leaf(e)?));}r.arm=None;
            }
        },
    }}
    // Every sealed value reachable through a borrow, however deeply it sits.
    fn sealed(types:&Types,p:Path,depth:usize,out:&mut Vec<Path>){
        if depth>8{return;}let Some(record)=types.get(p.ty) else{return;};if !matches!(record.kind,Kind::Record(_)){return;}
        if types.invariants(p.ty).is_some(){out.push(p.clone());}
        for(i,f)in record.fields.iter().enumerate(){let mut q=p.clone();q.fields.push(i);q.ty=f.ty;sealed(types,q,depth+1,out);}
    }
    let(mut assumes,mut maintains)=(vec![],vec![]);
    for(id,t)in scope.values(){let Ty::Borrow{pointee,mutable}=t else{continue;};
        let mut found=vec![];sealed(types,Path{root:*id,fields:vec![],ty:pointee.ty(),before:false},0,&mut found);
        for p in found{let(module,invariants)=types.invariants(p.ty).expect("sealed path");r.base=Some(p);
            for e in invariants{r.post=false;assumes.push((module,r.predicate(e,0)?));if *mutable{r.post=true;maintains.push((module,r.predicate(e,0)?));}}
            r.base=None;}
    }
    let nodes=r.nodes;let(borrow,mutable,borrowed)=scope.values().filter_map(|(id,t)|if let Ty::Borrow{mutable,pointee}=t{Some((*id,*mutable,pointee.ty()))}else{None}).max_by_key(|(_,mutable,_)|*mutable).ok_or_else(||error("memory contracts require one borrowed focus"))?;Ok(super::contract::Contract{requires:vec![],ensures:vec![],assumed:vec![],effects:vec![],elements:vec![],nodes,storage:None,record:None,memory:Some(MemoryContract{requires,ensures,nodes,element,result,mutable,borrow,borrowed,assumes,maintains,module:f.module})})
}

/// Invariants resolve at the actual typed head scope, against function-entry old snapshots.
pub(super) fn resolve_loop(expressions:&[a::Expr],scope:&BTreeMap<String,(u32,Ty)>,types:&Types,result:Ty,element:Ty)->Result<Vec<Predicate>>{
    let mut r=Resolve{scope,types,element,result,post:true,arm:None,nodes:0,leaves:0,base:None,bound:None};
    expressions.iter().map(|e|r.leaf(e)).collect()
}
