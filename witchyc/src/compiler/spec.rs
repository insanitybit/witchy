//! Shared named-predicate normalization before any proof-model routing.
use crate::{ast as a,diagnostic::Diagnostic};
use super::{logical_body::{FunctionId,Ty},spec_expr::Binder,spec_resolve::{Catalog,Scope,error},spec_expand::{self,Cost}};
use std::{borrow::Cow,collections::BTreeMap};
type Result<T> = std::result::Result<T,Diagnostic>;

pub(super) fn prepare(program:&a::Program)->Result<Cow<'_,a::Program>>{
    fn pattern(e:&a::Expr)->bool{matches!(e.kind,a::Expression::SpecCase{..})}
    fn needs(p:&a::Postcondition)->bool{match &p.kind {
        a::PostconditionKind::Predicate(e)=>pattern(e),
        a::PostconditionKind::Match{arms,..}=>arms.iter().any(|arm|arm.clauses.iter().any(needs))
    }}
    if !program.declarations.iter().any(|d|match d{
        a::Declaration::Predicate{..}=>true,
        a::Declaration::Record{conditions,..}=>conditions.iter().any(pattern),
        a::Declaration::Function(f)=>f.requires.iter().any(pattern)||f.ensures.iter().any(needs),
        _=>false,
    }){return Ok(Cow::Borrowed(program));}
    let mut catalog=Catalog::new(program)?;let mut output=program.clone();let mut function=0;let mut budget=Cost::default();
    for (declaration,d) in output.declarations.iter_mut().enumerate(){match d{
        a::Declaration::Record{module,generics,fields,conditions,..} if conditions.iter().any(|e|named(e,&catalog))=>{
            catalog.module=*module;
            let owner=FunctionId(0x4000_0000+declaration as u32);
            let mut scope=Scope::new();
            for (i,(name,t)) in fields.iter().enumerate(){
                scope.insert(name.clone(),(Binder{declaration:declaration as u32,parameter:i as u32},catalog.source_type(t,owner,generics)?));
            }
            for e in conditions { expand(e,&mut catalog,&scope,&mut budget)?; }
        }
        a::Declaration::Function(f)=>{
            catalog.module=f.module;
            let owner=FunctionId(function);function+=1;
            statements(&f.body,&catalog)?;
            if !f.requires.iter().any(|e|named(e,&catalog)) && !f.ensures.iter().any(|p|post_named(p,&catalog)){continue;}
            let mut scope=Scope::new();
            for(index,(name,t))in f.parameters.iter().enumerate(){
                let ty=catalog.source_type(t,owner,&f.generics)?;
                if scope.insert(name.clone(),(Binder{declaration:declaration as u32,parameter:index as u32},ty)).is_some(){return Err(error(0..0,"duplicate function parameter"));}
            }
            for e in &mut f.requires{expand(e,&mut catalog,&scope,&mut budget)?;}
            let result=catalog.source_type(&f.result,owner,&f.generics)?;
            let mut post_scope=scope.clone();post_scope.insert("result".into(),(Binder{declaration:declaration as u32,parameter:u32::MAX},result));
            posts(&mut f.ensures,&mut catalog,&post_scope,result,&mut budget,declaration as u32)?;
        },
        a::Declaration::Lemma(l)=>{
            if l.requires.iter().any(|e|named(e,&catalog))||named(&l.ensures,&catalog){return Err(error(l.ensures.span.clone(),"named predicates in lemmas are unsupported in this slice"));}
        },_=>{},
    }}
    output.proof_views.extend(catalog.views);
    output.declarations.retain(|d|!matches!(d,a::Declaration::Predicate{..}));Ok(Cow::Owned(output))
}
fn expand(e:&mut a::Expr,c:&mut Catalog,scope:&Scope,budget:&mut Cost)->Result<()>{
    if !named(e,c){return Ok(());}
    let resolved=c.expression(e,scope,false,0)?;
    let names=scope.iter().map(|(name,(id,_))|(*id,a::Expr{kind:a::Expression::Name(name.clone()),span:e.span.clone()})).collect::<BTreeMap<_,_>>();
    let costs=names.iter().map(|(id,e)|(*id,Cost{nodes:1,bytes:if let a::Expression::Name(n)=&e.kind{n.len()}else{0}})).collect();
    // Preflight actual arguments and all repeated substitutions before cloning
    // any expanded syntax. No model can perform a separate lookup or expansion.
    let size=spec_expand::cost(&resolved,c,&costs,0)?;*budget=budget.add(size,&e.span)?;
    *e=spec_expand::render(&resolved,c,&names,None)?;Ok(())
}
fn posts(clauses:&mut[a::Postcondition],c:&mut Catalog,scope:&Scope,result:Ty,budget:&mut Cost,declaration:u32)->Result<()>{
    for p in clauses {
        if let a::PostconditionKind::Predicate(e)=&p.kind
            && let a::Expression::SpecCase{value,arms,guarded:true}=&e.kind
            && matches!(&value.kind,a::Expression::Name(n) if n=="result")
        {
            // A result binder represents the actual returned loan. Keep it in the
            // existing result-arm IR so origin and lifetime checks see that loan.
            let sum=c.types.sum(result).ok_or_else(||error(value.span.clone(),"returned pattern requires a sum"))?;
            let mut out=Vec::new();
            for (variant,binding,clause) in arms {
                let tag=sum.names.iter().position(|n|n==variant).ok_or_else(||error(clause.span.clone(),"pattern does not belong to the returned sum"))?;
                if binding.as_ref().is_some_and(|n|scope.contains_key(n) || sum.arms[tag]==Ty::Unit){
                    return Err(error(clause.span.clone(),"result pattern needs a payload and a fresh binder"));
                }
                out.push(a::PostconditionArm{variant:variant.clone(),binding:binding.clone(),clauses:vec![
                    a::Postcondition{kind:a::PostconditionKind::Predicate(clause.clone()),span:clause.span.clone()}
                ]});
            }
            p.kind=a::PostconditionKind::Match{value:*value.clone(),arms:out};
        }
        match &mut p.kind{
        a::PostconditionKind::Predicate(e)=>expand(e,c,scope,budget)?,
        a::PostconditionKind::Match{value,arms}=>{
            if !arms.iter().any(|a|a.clauses.iter().any(|p|post_named(p,c))){continue;}
            if !matches!(&value.kind,a::Expression::Name(n) if n=="result"){return Err(error(value.span.clone(),"named predicates require a supported result match"));}
            let sum=c.types.sum(result).ok_or_else(||error(value.span.clone(),"predicate post match requires a sum"))?.clone();
            for arm in arms{
                let selected=sum.names.iter().position(|n|n==&arm.variant).ok_or_else(||error(value.span.clone(),"unknown result pattern"))?;
                let mut arm_scope=scope.clone();
                if let Some(name)=&arm.binding{
                    if scope.contains_key(name){return Err(error(value.span.clone(),"predicate result binder must be fresh"));}
                    arm_scope.insert(name.clone(),(Binder{declaration,parameter:u32::MAX-1-selected as u32},sum.arms[selected]));
                }
                posts(&mut arm.clauses,c,&arm_scope,sum.arms[selected],budget,declaration)?;
            }
        },
    }}Ok(())
}
fn post_named(p:&a::Postcondition,c:&Catalog)->bool{match &p.kind{a::PostconditionKind::Predicate(e)=>named(e,c),a::PostconditionKind::Match{value,arms}=>named(value,c)||arms.iter().any(|a|a.clauses.iter().any(|p|post_named(p,c)))}}
fn named(e:&a::Expr,c:&Catalog)->bool{match &e.kind{
    a::Expression::SpecCase{..}=>true,
    a::Expression::Call(f,args)=>matches!(&f.kind,a::Expression::Name(n)if c.names.contains_key(n))||named(f,c)||args.iter().any(|a|named(a,c)),
    a::Expression::Binary(_,a,b)|a::Expression::Index(a,b)=>named(a,c)||named(b,c),
    a::Expression::Unary(_,v)|a::Expression::Field(v,_)|a::Expression::Try(v)|a::Expression::Repeat(v,_)=>named(v,c),
    a::Expression::Array(v)=>v.iter().any(|v|named(v,c)),a::Expression::Record(fields)=>fields.iter().any(|(_,v)|named(v,c)),
    a::Expression::Forall{lower,upper,body,..}=>named(lower,c)||named(upper,c)||named(body,c),_=>false,
}}
fn reject(e:&a::Expr,c:&Catalog)->Result<()>{if named(e,c){Err(error(e.span.clone(),"named predicates are specification-only; runtime and loop-invariant uses are unsupported in this slice"))}else{Ok(())}}
fn statements(body:&[a::Statement],c:&Catalog)->Result<()>{
    for s in body{match s{
        a::Statement::Bind{value,..}|a::Statement::Open{value,..}|a::Statement::OpenFields{value,..}|a::Statement::Expr(value)|a::Statement::Assert(value)=>reject(value,c)?,
        a::Statement::Assign{target,value,..}=>{reject(target,c)?;reject(value,c)?;},
        a::Statement::Return(Some(e))=>reject(e,c)?,
        a::Statement::While{condition,invariants,body}=>{reject(condition,c)?;for e in invariants{reject(e,c)?;}statements(body,c)?;},
        a::Statement::If{condition,yes,no}=>{reject(condition,c)?;statements(yes,c)?;statements(no,c)?;},
        a::Statement::Match{value,arms}=>{reject(value,c)?;for arm in arms{statements(&arm.body,c)?;}},
        a::Statement::Choose{inner,..}=>statements(std::slice::from_ref(&**inner),c)?,
        a::Statement::Yield{value,..}=>reject(value,c)?,
        a::Statement::Guard{value,failure,..}=>{reject(value,c)?;statements(&failure.body,c)?;},
        a::Statement::For{start,end,body,..}=>{reject(start,c)?;if let Some(e)=end{reject(e,c)?;}statements(body,c)?;},
        a::Statement::Proof(steps)=>for p in steps{match p{a::ProofStep::Have(e)|a::ProofStep::NamedHave{claim:e,..}|a::ProofStep::Apply(e)|a::ProofStep::ApplyUsing{application:e,..}=>reject(e,c)?,_=>{}}},_=>{},
    }}Ok(())
}
