//! Bounded materialization of resolved predicates into the existing spec syntax.
use super::{spec_expr::{Binder,Expr,Kind},spec_resolve::{Catalog,error}};
use crate::{ast as a,diagnostic::Diagnostic};
use std::{collections::BTreeMap,ops::Range};
type Result<T> = std::result::Result<T,Diagnostic>;
#[derive(Clone,Copy,Default)]
pub(super) struct Cost {pub nodes:usize,pub bytes:usize}
impl Cost {
    fn node(bytes:usize)->Self{Self{nodes:1,bytes}}
    pub fn add(self,other:Self,span:&Range<usize>)->Result<Self>{
        let result=Self{nodes:self.nodes.saturating_add(other.nodes),bytes:self.bytes.saturating_add(other.bytes)};
        if result.nodes>4096||result.bytes>super::MAX_SOURCE_BYTES{return Err(error(span.clone(),"specification expansion budget exceeded before materialization"));}Ok(result)
    }
}
pub(super) fn cost(e:&Expr,c:&Catalog,bindings:&BTreeMap<Binder,Cost>,depth:usize)->Result<Cost>{
    if depth>64{return Err(error(e.span.clone(),"specification expansion depth exceeded"));}
    let mut total=Cost::node(8);
    match &e.kind {
        Kind::Bound(b)=>return bindings.get(b).copied().ok_or_else(||error(e.span.clone(),"unbound resolved specification identity")),
        Kind::Integer(_)=>total.bytes=20,Kind::Bool(_)=>total.bytes=5,
        Kind::Unary(_,v)|Kind::Field(v,_,_)=>{total=total.add(cost(v,c,bindings,depth+1)?,&e.span)?;if let Kind::Field(_,name,home)=&e.kind{total.bytes+=member_name(name,*home,c.module).len();}},
        Kind::Method(v,name,home)=>{total=Cost{nodes:2,bytes:member_name(name,*home,c.module).len()+3}.add(cost(v,c,bindings,depth+1)?,&e.span)?;}
        Kind::Let(id,value,body)=>{
            let size=cost(value,c,bindings,depth+1)?;
            let mut scope=bindings.clone();scope.insert(*id,size);
            total=size.add(cost(body,c,&scope,depth+1)?,&e.span)?;
        }
        Kind::Binary(_,a,b)=>{total=total.add(cost(a,c,bindings,depth+1)?,&e.span)?.add(cost(b,c,bindings,depth+1)?,&e.span)?;},
        Kind::Builtin(name,args)=>{
            // Account for the AST Call+Name for old, or Call+Field+Name for
            // memory methods, plus spelling/delimiters before materializing.
            total=Cost{nodes:if matches!(name.as_str(),"old"|"variant_index"){2}else{3},bytes:name.len()+if name=="old"{2}else{9}+args.len()*2};
            for a in args{total=total.add(cost(a,c,bindings,depth+1)?,&e.span)?;}
        },
        Kind::Apply(id,args)=>{
            let mut actual=BTreeMap::new();for(b,a)in c.definitions[*id].binders.iter().zip(args){let size=cost(a,c,bindings,depth+1)?;total=total.add(size,&e.span)?;actual.insert(*b,size);}
            total=total.add(cost(c.bodies[*id].as_ref().expect("resolved definition"),c,&actual,depth+1)?,&e.span)?;
        },
    }
    Cost::default().add(total,&e.span)
}
pub(super) fn render(e:&Expr,c:&Catalog,bindings:&BTreeMap<Binder,a::Expr>,origin:Option<Range<usize>>)->Result<a::Expr>{
    let span=origin.clone().unwrap_or_else(||e.span.clone());
    let kind=match &e.kind {
        Kind::Bound(id)=>return bindings.get(id).cloned().ok_or_else(||error(span,"unbound resolved specification identity")),
        Kind::Integer(n)=>a::Expression::Integer(*n),Kind::Bool(b)=>a::Expression::Bool(*b),
        Kind::Unary(op,v)=>a::Expression::Unary(op,Box::new(render(v,c,bindings,origin)?)),
        Kind::Binary(op,a,b)=>a::Expression::Binary(op,Box::new(render(a,c,bindings,origin.clone())?),Box::new(render(b,c,bindings,origin)?)),
        Kind::Field(v,name,home)=>return Ok(project_snapshot(render(v,c,bindings,origin)?,&span,|base|a::Expression::Field(Box::new(base),member_name(name,*home,c.module)))),
        Kind::Method(recv,name,home)=>{
            let receiver=render(recv,c,bindings,origin)?;
            return Ok(project_snapshot(receiver,&span,|recv| a::Expression::Call(Box::new(a::Expr{
                kind:a::Expression::Field(Box::new(recv),member_name(name,*home,c.module)),span:span.clone()}),vec![])));
        }
        Kind::Let(id,value,body)=>{
            let mut scope=bindings.clone();
            scope.insert(*id,render(value,c,bindings,origin.clone())?);
            return render(body,c,&scope,origin);
        }
        Kind::Builtin(name,args)=>{
            let callee=if matches!(name.as_str(),"old"|"variant_index"){a::Expr{kind:a::Expression::Name(name.clone()),span:span.clone()}}else{
                a::Expr{kind:a::Expression::Field(Box::new(a::Expr{kind:a::Expression::Name("memory".into()),span:span.clone()}),name.clone()),span:span.clone()}};
            let mut args=args.iter().map(|a|render(a,c,bindings,origin.clone())).collect::<Result<Vec<_>>>()?;
            if matches!(name.as_str(),"pointer_bits"|"box_id"|"variant_index"|"variant_value") {
                let first=args.remove(0);
                return Ok(project_snapshot(first,&span,|first|{args.insert(0,first);a::Expression::Call(Box::new(callee),args)}));
            }
            a::Expression::Call(Box::new(callee),args)
        },
        Kind::Apply(id,args)=>{
            let mut actual=BTreeMap::new();for(b,a)in c.definitions[*id].binders.iter().zip(args){actual.insert(*b,render(a,c,bindings,origin.clone())?);}
            return render(c.bodies[*id].as_ref().expect("resolved definition"),c,&actual,Some(span));
        },
    };Ok(a::Expr{kind,span})
}

/// A path projected from an entry-state value observes that same entry state.
fn project_snapshot(
    base: a::Expr,
    span: &Range<usize>,
    project: impl FnOnce(a::Expr) -> a::Expression,
) -> a::Expr {
    let at = |kind| a::Expr { kind, span: span.clone() };
    if let a::Expression::Call(c, args) = &base.kind
        && matches!(&c.kind,a::Expression::Name(n) if n=="old")
        && let [inner] = args.as_slice()
    {
        at(a::Expression::Call(c.clone(), vec![at(project(inner.clone()))]))
    } else {
        at(project(base))
    }
}

/// Imported predicates keep the defining module on the exact private projection
/// that it checked. An argument substituted by the caller keeps its own scope.
fn member_name(name: &str, home: Option<u32>, caller: u32) -> String {
    match home {
        Some(home) if home != caller => format!("#spec{home}:{name}"),
        _ => name.to_owned(),
    }
}
pub(super) fn member_scope(name: &str) -> (Option<u32>, &str) {
    if let Some((home, field)) = name.strip_prefix("#spec").and_then(|s| s.split_once(':'))
        && let Ok(home) = home.parse()
    {
        (Some(home), field)
    } else {
        (None, name)
    }
}
