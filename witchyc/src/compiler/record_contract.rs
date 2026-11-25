//! Typed clauses for generic records with zero or one entry allocation owner. No source body reconstruction.
use super::{logical_body::{FunctionId,Ty},semantic_types::{Types,Kind},storage_contract::error};
use crate::{ast as a,diagnostic::Diagnostic};
use std::collections::BTreeMap;
type Result<T> = std::result::Result<T,Diagnostic>;
#[derive(Clone,Debug)]
pub(super) enum Root {Parameter(u32),Arm(u8),
    /// The current value of a local, inside a loop invariant.
    Local(u32)}
#[derive(Clone,Debug)]
pub(super) struct Path {pub root:Root,pub fields:Vec<usize>,pub ty:Ty,pub before:bool}
#[derive(Clone,Debug)]
pub(super) enum Term {Literal(u64),Path(Path),Add(Box<Self>,Box<Self>),Capacity(Path),
    /// The variable of the enclosing `forall`.
    Bound}
#[derive(Clone,Debug)]
pub(super) enum Predicate {
    Bool(bool),Compare(&'static str,Term,Term),Not(Box<Self>),Binary(&'static str,Box<Self>,Box<Self>),
    Prefix(Path,Term,Term),Empty(Path,Term),Cell(Path,Term,Term),Same(Path,Path),
    Unchanged(Path,Path),Except(Path,Path,Term),Released(Path),EqualPrefix(Path,Path,Term),
    /// `forall j in (lo, hi): body`: the body holds at every cell index in `[lo, hi)`.
    Forall(Term,Term,Box<Self>),
    Utf8(Path,Term,Term),
    Utf8Boundary(Path,Term),
}
#[derive(Clone,Debug)]
pub(super) struct RecordContract {pub requires:Vec<Predicate>,pub ensures:Vec<(Option<u8>,Predicate)>,pub nodes:usize,
    /// Type invariants of sealed values received, with the file that declares each type.
    /// Assumed on entry; a caller in that file must show them.
    pub assumes:Vec<(u32,Predicate)>,
    /// Type invariants of sealed values handed back, per result arm. A body in the declaring
    /// file must show them; every caller may rely on them.
    pub maintains:Vec<(Option<u8>,u32,Predicate)>,
    /// The file the function was written in.
    pub module:u32,
    /// What the one allocation holds: the generic parameter, or a fixed narrow unsigned type.
    pub element:Ty,
    pub mutable_owner: Option<u32>}
fn shape(t:Ty,element:Ty,types:&Types,depth:usize,visits:&mut usize)->Option<usize> {
    *visits+=1;if depth>16||*visits>4096{return None;}
    match t {
        Ty::U64=>Some(0),t if t==element=>Some(0),
        Ty::Buffer(i) if types.buffers()[i as usize].element==element=>Some(1),
        Ty::Aggregate(_)=>{let r=types.get(t)?;if !matches!(r.kind,Kind::Record(_)){return None;}
            // A record of any number of allocations is one owner here: it is moved whole.
            let mut owners=0;for f in &r.fields{owners+=shape(f.ty,element,types,depth+1,visits)?;}if owners>=1{Some(1)}else{None}},
        _=>None,
    }
}
/// The element type of a function's allocations. A generic function stores its
/// parameter. A non-generic one must use a single unsigned type narrower than U64,
/// so stored values can never be confused with lengths and indices.
pub(super) fn element(owner:FunctionId,generics:usize,params:&[Ty],result:Ty,types:&Types)->Option<Ty> {
    if generics==1{return Some(Ty::Param{owner,index:0});}if generics!=0{return None;}
    fn leaves(t:Ty,types:&Types,depth:usize,out:&mut Vec<Ty>){if depth>16||out.len()>64{return;}match t{
        Ty::Buffer(i)=>out.push(types.buffers()[i as usize].element),
        Ty::Aggregate(_)=>if let Some(r)=types.get(t){for f in &r.fields{leaves(f.ty,types,depth+1,out);}},
        Ty::Sum(_)=>if let Some(s)=types.result_sum(t){for arm in s.arms{leaves(arm,types,depth+1,out);}},
        Ty::Borrow{pointee,..}=>leaves(pointee.ty(),types,depth+1,out),_=>{}}}
    let mut found=vec![];for p in params{leaves(*p,types,0,&mut found);}leaves(result,types,0,&mut found);
    let first=*found.first()?;(found.iter().all(|t|*t==first)&&matches!(first,Ty::Unsigned(_))&&first!=Ty::U64).then_some(first)
}
pub(super) fn eligible(owner:FunctionId,generics:usize,params:&[Ty],result:Ty,types:&Types)->bool {
    let Some(t)=element(owner,generics,params,result,types) else{return false;};let mut owners=0;let mut visits=0;let mut mutable_owner=false;
    for p in params {match *p {
        Ty::Buffer(_)|Ty::Aggregate(_)=>{if shape(*p,t,types,0,&mut visits)!=Some(1){return false;}owners+=1;},
        Ty::Borrow{mutable:false,pointee:super::logical_body::Pointee::Allocator}=>{},
        Ty::Borrow{mutable:true,pointee} if !mutable_owner => {
            if shape(pointee.ty(),t,types,0,&mut visits)!=Some(1){return false;}
            mutable_owner=true;
            owners+=1;
        },
        // A shared borrow is only ever handed to read-only helpers; it owns nothing here.
        Ty::Borrow{mutable:false,..}=>{},
        p if p==Ty::U64||p==t=>{},_=>return false,
    }}
    if owners>1{return false;}
    if result==Ty::Unit{return owners==1;}
    if mutable_owner {
        return types.result_sum(result).is_some_and(|sum|sum.arms==[Ty::Unit,Ty::U64]);
    }
    let owned=|ty:Ty,visits:&mut usize|matches!(ty,Ty::Aggregate(_)|Ty::Buffer(_))&&shape(ty,t,types,0,visits)==Some(1);
    // A consuming operation that cannot fail returns the owner directly.
    let Some(sum)=types.result_sum(result) else{return owners==1&&owned(result,&mut visits);};
    // Constructors may start with no owner; their typed failure carries only a
    // code. Existing consuming operations retain their two-owned-arm domain.
    // A release that can fail gives nothing back on success and the owner back on failure.
    if owners==1&&sum.arms[0]==Ty::Unit{return owned(sum.arms[1],&mut visits);}
    owned(sum.arms[0],&mut visits)&&((owners==0&&sum.arms[1]==Ty::U64)||(owners==1&&owned(sum.arms[1],&mut visits)))
}
struct Resolve<'a>{scope:&'a BTreeMap<String,(u32,Ty)>,types:&'a Types,element:Ty,arm:Option<(&'a str,u8,Ty)>,ensures:bool,nodes:usize,leaves:usize,
    /// Inside a loop invariant: names are current locals, and ids below this are parameters.
    loop_parameters:Option<u32>,
    /// Reading a type invariant: bare names are the fields of this value.
    base:Option<Path>,
    /// The variable of the `forall` being read.
    bound:Option<String>}
/// An expression that is true or false rather than a number.
fn truth(e:&a::Expr)->bool{match &e.kind{
    a::Expression::Bool(_)|a::Expression::Unary("!",_)|a::Expression::Forall{..}=>true,
    a::Expression::Binary(op,..)=>matches!(*op,"&&"|"||"|"<"|"<="|">"|">="|"=="|"!="),
    _=>super::storage_contract::method(e).is_some_and(|(m,_)|m!="capacity"),
}}
impl Resolve<'_> {
    fn count(&mut self,d:usize)->Result<()> {self.nodes+=1;if d>64||self.nodes>4096{return Err(error("record contracts expression complexity limit"));}Ok(())}
    fn raw_path(&mut self,e:&a::Expr,d:usize)->Result<Path> {
        self.count(d)?;match &e.kind {
            a::Expression::Name(n) if self.base.is_some()=>{
                let mut p=self.base.clone().expect("invariant base");
                let (i,f)=self.types.get(p.ty).and_then(|r|r.fields.iter().enumerate().find(|(_,f)|f.name==*n)).ok_or_else(||error("a type invariant names only the type's own fields"))?;
                p.fields.push(i);p.ty=f.ty;Ok(p)
            },
            a::Expression::Name(n)=>{
                if let Some((name,arm,ty))=self.arm{if n==name{return Ok(Path{root:Root::Arm(arm),fields:vec![],ty,before:false});}}
                let (id,ty)=self.scope.get(n).ok_or_else(||error("record contracts clause name is not a parameter or active result binder"))?;
                if self.loop_parameters.is_some(){return Ok(Path{root:Root::Local(*id),fields:vec![],ty:*ty,before:false});}
                let (value_ty,current)=match *ty {
                    Ty::Borrow{mutable:true,pointee} => (pointee.ty(),self.ensures),
                    _ => (*ty,false),
                };
                Ok(Path{root:if current { Root::Local(*id) } else { Root::Parameter(*id) },fields:vec![],ty:value_ty,before:!self.ensures})
            },
            a::Expression::Field(v,name)=>{let mut p=self.raw_path(v,d+1)?;let record=self.types.get(p.ty).ok_or_else(||error("record contracts field path requires a record"))?;
                let (i,f)=record.fields.iter().enumerate().find(|(_,f)|f.name==super::spec_expand::member_scope(name).1).ok_or_else(||error("record contracts unknown field"))?;p.fields.push(i);p.ty=f.ty;Ok(p)},
            _=>Err(error("record contracts require typed parameter/result field paths")),
        }
    }
    fn path(&mut self,e:&a::Expr,d:usize)->Result<Path> {
        if let a::Expression::Call(c,args)=&e.kind {if matches!(&c.kind,a::Expression::Name(n) if n=="old")&&args.len()==1 {
            self.count(d)?;if !self.ensures{return Err(error("record contracts old is ensures-only"));}
            let mut p=self.raw_path(&args[0],d+1)?;
            if let Root::Local(id)=p.root {
                if self.loop_parameters.is_some_and(|parameters|id<parameters)
                    || self.scope.values().any(|(parameter,ty)|*parameter==id&&matches!(ty,Ty::Borrow{mutable:true,..}))
                {
                    p.root=Root::Parameter(id);
                }
            }
            if !matches!(p.root,Root::Parameter(_)){return Err(error("record contracts old requires an entry parameter path"));}p.before=true;return Ok(p);
        }}
        let p=self.raw_path(e,d)?;
        if self.base.is_some(){return Ok(p);}
        if self.ensures {if let Root::Parameter(id)=p.root {
            if self.scope.values().any(|(i,t)|*i==id&&matches!(t,Ty::Aggregate(_)|Ty::Buffer(_))){return Err(error("record contracts consumed record parameters require old snapshots"));}
        }}Ok(p)
    }
    fn buffer(&mut self,e:&a::Expr,d:usize)->Result<Path>{let p=self.path(e,d)?;if !matches!(p.ty,Ty::Buffer(i) if self.types.buffers()[i as usize].element==self.element){return Err(error("record contracts view requires the generic Buffer leaf"));}Ok(p)}
    fn term(&mut self,e:&a::Expr,d:usize)->Result<(Term,Ty)> {
        self.count(d)?;Ok(match &e.kind {
            a::Expression::Integer(n)=>(Term::Literal(*n),Ty::U64),
            a::Expression::Name(n) if self.bound.as_deref()==Some(n.as_str())=>(Term::Bound,Ty::U64),
            a::Expression::Binary("+",a,b)=>(Term::Add(Box::new(self.number(a,d+1)?),Box::new(self.number(b,d+1)?)),Ty::U64),
            _=>if let Some(("capacity",[v]))=super::storage_contract::method(e){(Term::Capacity(self.buffer(v,d+1)?),Ty::U64)}else{
                let p=self.path(e,d+1)?;if p.ty!=Ty::U64&&p.ty!=self.element{return Err(error("record contracts term requires U64 or the generic Copy element"));}let t=p.ty;(Term::Path(p),t)
            },
        })
    }
    fn number(&mut self,e:&a::Expr,d:usize)->Result<Term>{let(t,ty)=self.term(e,d)?;if ty!=Ty::U64{return Err(error("record contracts numeric term requires U64"));}Ok(t)}
    fn predicate(&mut self,e:&a::Expr,d:usize)->Result<Predicate> {
        self.count(d)?;Ok(match &e.kind {
            a::Expression::Bool(b)=>Predicate::Bool(*b),a::Expression::Unary("!",p)=>Predicate::Not(Box::new(self.predicate(p,d+1)?)),
            a::Expression::Binary(op@("&&"|"||"),p,q)=>Predicate::Binary(if *op=="&&"{"∧"}else{"∨"},Box::new(self.predicate(p,d+1)?),Box::new(self.predicate(q,d+1)?)),
            // Two truths are equal when each holds exactly when the other does.
            a::Expression::Binary(op@("=="|"!="),p,q) if truth(p)||truth(q)=>{let iff=Predicate::Binary("↔",Box::new(self.predicate(p,d+1)?),Box::new(self.predicate(q,d+1)?));if *op=="=="{iff}else{Predicate::Not(Box::new(iff))}},
            a::Expression::Forall{variable,lower,upper,body}=>{
                if self.bound.is_some(){return Err(error("record contracts: a forall cannot sit inside another"));}
                if self.scope.contains_key(variable)||variable=="result"||self.arm.is_some_and(|(n,_,_)|n==variable.as_str()){return Err(error("record contracts: a forall variable cannot hide another name"));}
                let lo=self.number(lower,d+1)?;let hi=self.number(upper,d+1)?;
                self.bound=Some(variable.clone());let body=self.predicate(body,d+1);self.bound=None;let body=body?;
                // A clause that is itself quantified would reuse the bound variable's name in the
                // statement and capture it; a body speaks of single cells and numbers only.
                fn pointwise(p:&Predicate)->bool{match p{Predicate::Bool(_)|Predicate::Compare(..)|Predicate::Empty(..)|Predicate::Cell(..)=>true,Predicate::Not(p)=>pointwise(p),Predicate::Binary(_,a,b)=>pointwise(a)&&pointwise(b),_=>false}}
                if !pointwise(&body){return Err(error("record contracts: a forall body speaks of single cells (`memory.initialized`, `memory.empty`, `memory.cell`) and numbers"));}
                Predicate::Forall(lo,hi,Box::new(body))
            },
            a::Expression::Binary(op@("=="|"!="|"<"|"<="|">"|">="),p,q)=>{let(p,t)=self.term(p,d+1)?;let(q,u)=self.term(q,d+1)?;
                if t!=u||(t!=Ty::U64&&!matches!(*op,"=="|"!=")){return Err(error("record contracts comparison type mismatch"));}
                Predicate::Compare(match *op{"=="=>"=","!="=>"≠","<"=>"<","<="=>"≤",">"=>">",_=>"≥"},p,q)},
            _=>match super::storage_contract::method(e) {
                Some(("prefix",[v,n,c]))=>Predicate::Prefix(self.buffer(v,d+1)?,self.number(n,d+1)?,self.number(c,d+1)?),
                Some(("utf8_boundary",[v,index]))=>{
                    if self.element != Ty::Unsigned(super::scalar::Scalar::U8) {
                        return Err(error("memory.utf8_boundary requires a U8 buffer"));
                    }
                    Predicate::Utf8Boundary(self.buffer(v,d+1)?,self.number(index,d+1)?)
                },
                Some(("utf8",[v,start,count]))=>{
                    if self.element != Ty::Unsigned(super::scalar::Scalar::U8) {
                        return Err(error("memory.utf8 requires a U8 buffer"));
                    }
                    Predicate::Utf8(self.buffer(v,d+1)?,self.number(start,d+1)?,self.number(count,d+1)?)
                },
                Some(("empty",[v,i]))=>Predicate::Empty(self.buffer(v,d+1)?,self.number(i,d+1)?),
                Some(("initialized",[v,i]))=>Predicate::Not(Box::new(Predicate::Empty(self.buffer(v,d+1)?,self.number(i,d+1)?))),
                Some(("cell",[v,i,item]))=>{let(item,t)=self.term(item,d+1)?;if t!=self.element{return Err(error("record contracts cell payload requires the generic element"));}Predicate::Cell(self.buffer(v,d+1)?,self.number(i,d+1)?,item)},
                Some(("same_allocation",[a,b]))=>Predicate::Same(self.buffer(a,d+1)?,self.buffer(b,d+1)?),
                Some(("unchanged",[a,b]))=>Predicate::Unchanged(self.buffer(a,d+1)?,self.buffer(b,d+1)?),
                Some(("unchanged_except",[a,b,i]))=>Predicate::Except(self.buffer(a,d+1)?,self.buffer(b,d+1)?,self.number(i,d+1)?),
                Some(("equal_prefix",[a,b,n]))=>Predicate::EqualPrefix(self.buffer(a,d+1)?,self.buffer(b,d+1)?,self.number(n,d+1)?),
                // Nothing stored anywhere: an empty prefix that spans the allocation.
                Some(("empty_all",[v]))=>{let p=self.buffer(v,d+1)?;Predicate::Prefix(p.clone(),Term::Literal(0),Term::Capacity(p))},
                Some(("released",[v]))=>{let p=self.buffer(v,d+1)?;if !self.ensures||!p.before{return Err(error("record contracts released requires old entry allocation in ensures"));}Predicate::Released(p)},
                Some(("frame_unchanged",_))=>return Err(error("`memory.frame_unchanged` was removed: it stated nothing. Say what is unchanged with `memory.unchanged` or `memory.unchanged_except`")),
                _=>return Err(error("record contracts unsupported predicate")),
            },
        })
    }
    fn leaf(&mut self,e:&a::Expr)->Result<Predicate>{self.leaves+=1;if self.leaves>32{return Err(error("record contracts allow at most 32 combined predicate clauses"));}self.predicate(e,0)}
}
pub(super) fn resolve(f:&a::Function,scope:&BTreeMap<String,(u32,Ty)>,types:&Types,result:Ty,owner:FunctionId)->Result<super::contract::Contract> {
    if f.ensures.is_empty()||scope.len()>32||scope.contains_key("result"){return Err(error("record contracts require ensures, at most 32 parameters and no result parameter"));}
    let result_arms=types.result_sum(result).map(|s|s.arms);
    let parameters=scope.values().map(|(_,t)|*t).collect::<Vec<_>>();
    let element=element(owner,f.generics.len(),&parameters,result,types).ok_or_else(||error("record contracts require one element type"))?;
    let mut r=Resolve{scope,types,element,arm:None,ensures:false,nodes:0,leaves:0,loop_parameters:None,base:None,bound:None};
    // Without a Result there is one outcome, and `result` names the returned owner.
    let plain=(result!=Ty::Unit&&result_arms.is_none()).then_some(("result",0u8,result));
    let requires=f.requires.iter().map(|p|r.leaf(p)).collect::<Result<Vec<_>>>()?;r.ensures=true;let mut ensures=vec![];
    for p in &f.ensures {match &p.kind {
        a::PostconditionKind::Predicate(e)=>{r.arm=plain;ensures.push((None,r.leaf(e)?));r.arm=None;},
        a::PostconditionKind::Match{value,arms:cases}=>{
            r.count(0)?;if !matches!(&value.kind,a::Expression::Name(n) if n=="result")||cases.len()!=2{return Err(error("record contracts require exhaustive match result"));}
            let arms=result_arms.ok_or_else(||error("record contracts match result requires Result"))?;
            let mut seen=[false;2];for c in cases {
                let arm=match c.variant.as_str(){"Ok"=>0,"Err"=>1,_=>return Err(error("record contracts require Ok and Err"))};
                // An arm that carries nothing has nothing to name.
                let name=match c.binding.as_deref(){Some(name)=>name,None if arms[arm]==Ty::Unit=>"",None=>return Err(error("record contracts active record arm requires a binder"))};
                if seen[arm]||name=="result"||scope.contains_key(name)||c.clauses.is_empty(){return Err(error("record contracts result binders must be fresh and arms unique"));}seen[arm]=true;r.arm=Some((name,arm as u8,arms[arm]));
                for p in &c.clauses {let a::PostconditionKind::Predicate(e)=&p.kind else{return Err(error("record contracts nested matches unsupported"));};ensures.push((Some(arm as u8),r.leaf(e)?));}r.arm=None;
            }
        },
    }}
    // Every sealed value that comes in or goes out, however deeply it sits in a record.
    fn sealed(types:&Types,p:Path,depth:usize,out:&mut Vec<Path>){
        if depth>8{return;}let Some(record)=types.get(p.ty) else{return;};if !matches!(record.kind,Kind::Record(_)){return;}
        if types.invariants(p.ty).is_some(){out.push(p.clone());}
        for(i,f)in record.fields.iter().enumerate(){let mut q=p.clone();q.fields.push(i);q.ty=f.ty;sealed(types,q,depth+1,out);}
    }
    let stated=|r:&mut Resolve,p:Path|->Result<Vec<(u32,Predicate)>>{
        let(module,invariants)=types.invariants(p.ty).expect("sealed path");r.base=Some(p);
        let found=invariants.iter().map(|e|Ok((module,r.predicate(e,0)?))).collect::<Result<Vec<_>>>();r.base=None;found
    };
    let mutable_owner=scope.values().find_map(|(id,ty)|matches!(ty,Ty::Borrow{mutable:true,..}).then_some(*id));
    let mut assumes=vec![];r.ensures=false;
    let mut maintains=vec![];
    for (id, ty) in scope.values() {
        let (ty, mutable) = match *ty {
            Ty::Borrow{mutable:true,pointee} => (pointee.ty(),true),
            ty => (ty,false),
        };
        let mut found=vec![];
        sealed(types,Path{root:Root::Parameter(*id),fields:vec![],ty,before:true},0,&mut found);
        for p in found {
            assumes.extend(stated(&mut r,p.clone())?);
            if mutable {
                let mut current=p;
                current.root=Root::Local(*id);
                current.before=false;
                for (module,predicate) in stated(&mut r,current)? {
                    maintains.push((None,module,predicate));
                }
            }
        }
    }
    r.ensures=true;
    let outcomes:Vec<(Option<u8>,u8,Ty)>=match result_arms{Some(arms)=>vec![(Some(0),0,arms[0]),(Some(1),1,arms[1])],None if result!=Ty::Unit=>vec![(None,0,result)],None=>vec![]};
    for(selected,arm,ty)in outcomes{
        let mut found=vec![];sealed(types,Path{root:Root::Arm(arm),fields:vec![],ty,before:false},0,&mut found);
        for p in found{for(module,predicate)in stated(&mut r,p)?{maintains.push((selected,module,predicate));}}
    }
    let nodes=r.nodes;Ok(super::contract::Contract{requires:vec![],ensures:vec![],assumed:vec![],effects:vec![],elements:vec![],nodes,storage:None,record:Some(RecordContract{requires,ensures,nodes,element,assumes,maintains,module:f.module,mutable_owner}),memory:None})
}

/// Loop invariants of a record-model body: clauses over the current locals and `old` parameters.
pub(super) fn resolve_loop(expressions:&[a::Expr],scope:&BTreeMap<String,(u32,Ty)>,types:&Types,element:Ty,parameters:u32)->Result<Vec<Predicate>> {
    let mut r=Resolve{scope,types,element,arm:None,ensures:true,nodes:0,leaves:0,loop_parameters:Some(parameters),base:None,bound:None};
    expressions.iter().map(|e|r.leaf(e)).collect()
}
