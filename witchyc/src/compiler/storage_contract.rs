//! Clause-only typed Buffer snapshots; never runtime owner values.
use super::{logical_body::{FunctionId, Pointee, Ty}, semantic_types::Types};
use crate::{ast as a, diagnostic::Diagnostic};
use std::collections::BTreeMap;
type Result<T> = std::result::Result<T, Diagnostic>;
pub(super) fn error(message: &'static str) -> Diagnostic { Diagnostic::new("W0401",0..0,message) }
#[derive(Clone, Copy, Debug)]
pub(super) enum View { Before, After }
#[derive(Clone, Debug)]
pub(super) enum Number { Literal(u64), Parameter(u32), Code, Capacity(View) }
#[derive(Clone, Debug)]
pub(super) enum Predicate {
    Bool(bool), Compare(&'static str, Number, Number), Not(Box<Self>), Binary(&'static str, Box<Self>, Box<Self>),
    Empty(View, Number), Cell(View, Number, u32), Same(View, View), Unchanged(View, View),
    Except(View, View, Number),
}
#[derive(Clone, Debug)]
pub(super) struct StorageContract { pub buffer:u32, pub clauses:Vec<(Option<u8>, Predicate)>, pub nodes:usize }

pub(super) fn eligible(owner:FunctionId, generics:usize, params:&[Ty], result:Ty, types:&Types)->bool {
    let t=Ty::Param{owner,index:0};
    generics==1 && types.result_sum(result).is_some_and(|s|s.arms==[Ty::Unit,Ty::U64]) &&
    params.iter().filter(|p|matches!(p,Ty::Borrow{..})).count()==1 &&
    params.iter().any(|p|*p==t) && params.iter().all(|p|match p {
        Ty::Borrow{mutable:true,pointee:Pointee::Buffer(id)}=>types.buffers()[*id as usize].element==t,
        _=>*p==Ty::U64||*p==t,
    })
}
pub(super) fn method<'a>(e:&'a a::Expr)->Option<(&'a str,&'a [a::Expr])> {
        if let a::Expression::Call(c,args)=&e.kind {if let a::Expression::Field(receiver,name)=&c.kind {
            if matches!(&receiver.kind,a::Expression::Name(n) if n=="memory"){return Some((name,args));}
        }}None
    }

struct Resolve<'a> { scope:&'a BTreeMap<String,(u32,Ty)>, element:Ty, buffer:u32, code:Option<&'a str>, nodes:usize, clauses:usize }
impl Resolve<'_> {
    fn count(&mut self,depth:usize)->Result<()> {self.nodes+=1;if depth>64||self.nodes>4096 {return Err(error("storage contracts expression complexity limit"));}Ok(())}
    fn parameter(&self,e:&a::Expr,ty:Ty)->Result<u32> {
        if let a::Expression::Name(name)=&e.kind {
            if self.code!=Some(name.as_str()) {if let Some((id,t))=self.scope.get(name) {if *t==ty{return Ok(*id);}}}
        }
        Err(error("storage contracts require a parameter of the expected logical type"))
    }
    fn view(&mut self,e:&a::Expr,depth:usize)->Result<View> {
        self.count(depth)?;
        match &e.kind {
            a::Expression::Name(name) if self.code!=Some(name.as_str())&&self.scope.get(name).is_some_and(|(id,_)|*id==self.buffer)=>Ok(View::After),
            a::Expression::Call(c,args) if matches!(&c.kind,a::Expression::Name(n) if n=="old")&&args.len()==1=>{
                if matches!(self.view(&args[0],depth+1)?,View::After){Ok(View::Before)}else{Err(error("storage contracts old requires the buffer parameter, not another snapshot"))}
            },
            _=>Err(error("storage contracts snapshot requires the exact buffer parameter")),
        }
    }
    fn number(&mut self,e:&a::Expr,depth:usize)->Result<Number> {
        self.count(depth)?;
        Ok(match &e.kind {
            a::Expression::Integer(n)=>Number::Literal(*n),
            a::Expression::Name(n) if self.code==Some(n.as_str())=>Number::Code,
            a::Expression::Name(_)=>Number::Parameter(self.parameter(e,Ty::U64)?),
            _=>match method(e) {
                Some(("capacity",[v]))=>Number::Capacity(self.view(v,depth+1)?),
                _=>return Err(error("storage contracts require U64 parameters, literals, error codes or capacity")),
            },
        })
    }
    fn predicate(&mut self,e:&a::Expr,depth:usize)->Result<Predicate> {
        self.count(depth)?;
        Ok(match &e.kind {
            a::Expression::Bool(b)=>Predicate::Bool(*b),
            a::Expression::Unary("!",p)=>Predicate::Not(Box::new(self.predicate(p,depth+1)?)),
            a::Expression::Binary(op@("&&"|"||"),p,q)=>Predicate::Binary(if *op=="&&"{"∧"}else{"∨"},Box::new(self.predicate(p,depth+1)?),Box::new(self.predicate(q,depth+1)?)),
            a::Expression::Binary(op@("=="|"!="|"<"|"<="|">"|">="),p,q)=>Predicate::Compare(match *op{"=="=>"=","!="=>"≠","<"=>"<","<="=>"≤",">"=>">",_=>"≥"},self.number(p,depth+1)?,self.number(q,depth+1)?),
            _=>match method(e) {
                Some(("empty",[v,i]))=>Predicate::Empty(self.view(v,depth+1)?,self.number(i,depth+1)?),
                Some(("cell",[v,i,p]))=>{self.count(depth+1)?;Predicate::Cell(self.view(v,depth+1)?,self.number(i,depth+1)?,self.parameter(p,self.element)?)},
                Some(("same_allocation",[a,b]))=>Predicate::Same(self.view(a,depth+1)?,self.view(b,depth+1)?),
                Some(("unchanged",[a,b]))=>Predicate::Unchanged(self.view(a,depth+1)?,self.view(b,depth+1)?),
                Some(("unchanged_except",[a,b,i]))=>Predicate::Except(self.view(a,depth+1)?,self.view(b,depth+1)?,self.number(i,depth+1)?),
                Some(("frame_unchanged",_))=>return Err(error("`memory.frame_unchanged` was removed: it stated nothing. Say what is unchanged with `memory.unchanged` or `memory.unchanged_except`")),
                _=>return Err(error("storage contracts require supported snapshot predicates")),
            },
        })
    }
    fn clause(&mut self,p:&a::Postcondition,arm:Option<u8>,out:&mut Vec<(Option<u8>,Predicate)>)->Result<()> {
        self.clauses+=1;if self.clauses>32{return Err(error("storage contracts allow at most 32 clauses"));}
        match &p.kind {
            a::PostconditionKind::Predicate(e)=>{let value=self.predicate(e,0)?;out.push((arm,value));Ok(())},
            _=>Err(error("storage contracts do not support nested result matches")),
        }
    }
}
pub(super) fn resolve(f:&a::Function,scope:&BTreeMap<String,(u32,Ty)>)->Result<super::contract::Contract> {
    if !f.requires.is_empty(){return Err(error("storage contracts do not support requires clauses"));}
    if scope.contains_key("result") {
        return Err(error("storage contracts parameter cannot be named result"));
    }
    if f.ensures.is_empty()||scope.len()>32{return Err(error("storage contracts require ensures and at most 32 parameters"));}
    let buffer=*scope.values().find(|(_,t)|matches!(t,Ty::Borrow{..})).expect("eligible signature");
    let element=scope.values().find_map(|(_,t)|matches!(t,Ty::Param{..}).then_some(*t)).expect("eligible payload");
    let mut r=Resolve{scope,element,buffer:buffer.0,code:None,nodes:0,clauses:0};let mut clauses=vec![];
    for p in &f.ensures {
        if let a::PostconditionKind::Match{value,arms}=&p.kind {
            r.count(0)?;
            if !matches!(&value.kind,a::Expression::Name(n) if n=="result")||arms.len()!=2 {
                return Err(error("storage contracts require exhaustive match result with Ok and Err"));
            }
            let mut seen=[false;2];
            for a in arms {
                let arm=match a.variant.as_str(){"Ok"=>0,"Err"=>1,_=>return Err(error("storage contracts require Ok and Err arms"))};
                if seen[arm]||(arm==0&&a.binding.is_some())||(arm==1&&a.binding.is_none())||a.clauses.is_empty() {
                    return Err(error("storage contracts require Unit Ok and bound U64 Err exactly once"));
                }
                seen[arm]=true;
                if a.binding.as_deref().is_some_and(|n|n=="result"||scope.contains_key(n)){return Err(error("storage contracts Err binder cannot shadow a parameter or result"));}
                r.code=a.binding.as_deref();
                for c in &a.clauses{r.clause(c,Some(arm as u8),&mut clauses)?;}
                r.code=None;
            }
        }else{r.clause(p,None,&mut clauses)?;}
    }
    let nodes=r.nodes;
    Ok(super::contract::Contract{requires:vec![],ensures:vec![],assumed:vec![],effects:vec![],elements:vec![],nodes,storage:Some(StorageContract{buffer:buffer.0,clauses,nodes}),record:None,memory:None})
}
