//! Constraint convergence and concrete instance inventory over the ONE checked body.
use super::{logical_body::*, logical_place::{Place,Projection}, semantic_types::Types};
use crate::diagnostic::Diagnostic;
type Result<T> = std::result::Result<T,Diagnostic>;
fn error(message:&'static str)->Diagnostic{Diagnostic::new("W0401",0..0,message)}
#[derive(Clone, Debug)]
pub(super) struct Definition {pub name:String,pub checked_nodes:usize,pub checks:u32}
#[derive(Clone, Debug)]
pub(super) struct Instance {pub definition:FunctionId,pub arguments:Vec<Ty>,pub function:Function}
#[derive(Clone)]
pub(super) struct Plan {pub definitions:Vec<Definition>,pub instances:Vec<Instance>,pub types:Types,pub main:u32,pub constant_limits:Vec<Ty>,
    /// Bodies nothing reaches: not emitted, but held to the limits their types decide (concrete.rs).
    pub unreached:Vec<Instance>}
fn constraints(functions:&mut[Function],types:&mut Types)->Result<()> {
    let n=functions.len();
    let edges:Vec<Vec<usize>>=functions.iter().map(|f|f.calls.iter().map(|e|e.definition.0 as usize).collect()).collect();
    // A call is recursive exactly when caller and callee are in one strongly connected component.
    let component=super::call_graph::components(&edges);
    for (i,f)in functions.iter().enumerate(){for edge in &f.calls{let callee=&functions[edge.definition.0 as usize];
        if component[edge.definition.0 as usize]==component[i]&&(!f.type_parameters.is_empty()||!callee.type_parameters.is_empty()) {
            if edge.substitutions.len()!=f.type_parameters.len()||edge.substitutions.iter().zip(&f.type_parameters).any(|(a,p)|*a!=p.ty){
                return Err(error("polymorphic recursion requires identical type substitutions"));
            }
        }
    }}
    for f in functions.iter_mut(){let mut unique=vec![];for t in &f.element_limits{if !unique.contains(t){unique.push(*t);}}f.element_limits=unique;if f.element_limits.len()+f.type_parameters.len()>4096{return Err(error("generic constraint state limit"));}}
    loop {let mut changed=false;
        for i in 0..n {let edges=functions[i].calls.clone();for edge in edges {
            let required=functions[edge.definition.0 as usize].element_limits.clone();
            for t in required{let t=types.substitute(t,edge.definition,&edge.substitutions)?;
                if !functions[i].element_limits.contains(&t){functions[i].element_limits.push(t);changed=true;}
            }
            if functions[i].element_limits.len()+functions[i].type_parameters.len()>4096{return Err(error("generic constraint state limit"));}
        }}
        if !changed{break;}
    }Ok(())
}
struct Pending {definition:FunctionId,arguments:Vec<Ty>,body:Option<Function>,active:bool}
struct Builder {functions:Vec<Function>,types:Types,pending:Vec<Pending>,limit:usize,by_name:std::collections::BTreeMap<String,FunctionId>}
impl Builder {
    fn register(&mut self,definition:FunctionId,arguments:Vec<Ty>)->Result<u32>{
        debug_assert_eq!(self.functions[definition.0 as usize].definition,definition);
        // Brands are erased here (docs/design-brands.md): checking is done, a brand is a type
        // argument only and never a value, and a body generic over one was checked once with it
        // abstract, so every brand shares one instance.
        let arguments:Vec<Ty>=arguments.into_iter().map(|t|self.types.erased_brand(t)).collect::<Result<_>>()?;
        if let Some(i)=self.pending.iter().position(|p|p.definition==definition&&p.arguments==arguments){return Ok(i as u32);}
        if self.pending.len()>=self.limit{return Err(Diagnostic::new("W0403",0..0,"the program needs more than the 4096 functions a module may hold"));}
        if arguments.len()!=self.functions[definition.0 as usize].type_parameters.len(){return Err(error("incomplete concrete type arguments"));}
        let name=&self.functions[definition.0 as usize].name;
        let call_trait=name.strip_prefix("WitchyFn").and_then(|rest|rest.strip_suffix(".call")).is_some_and(|n|n.parse::<usize>().is_ok());
        let count=self.functions[definition.0 as usize].type_parameters.len();
        for (at,(parameter,t)) in self.functions[definition.0 as usize].type_parameters.iter().zip(&arguments).enumerate(){
            if !self.types.concrete(*t){return Err(error("unbound function type parameter has no concrete layout"));}
            let any=matches!(parameter.family,ParameterFamily::AnyStorage);
            // As in check.rs: a function type's parameter, or `Self` of a call trait's `call`.
            // As in check.rs: never the result `R`, the last.
            if parameter.work{self.types.work_argument(*t,any)?;continue;}
            let callable=parameter.name.starts_with("#F")||(call_trait&&at+1<count);
            let (params,result)=(self.functions[definition.0 as usize].params.clone(),self.functions[definition.0 as usize].result);
            if !callable&&any&&self.types.shared_view(*t)&&!self.types.authority_in(*t,0)&&self.types.only_lent(&params,result,definition,at as u32){continue;}
            if !callable&&any&&self.types.unsized_str(*t)&&self.types.only_lent(&params,result,definition,at as u32){continue;}
            if callable{self.types.callable_argument(*t,any)?}else{self.types.function_argument(*t,any)?}
        }
        let i=self.pending.len() as u32;self.pending.push(Pending{definition,arguments,body:None,active:false});Ok(i)
    }
    fn expand(&mut self,i:u32)->Result<()> {
        if self.pending[i as usize].body.is_some()||self.pending[i as usize].active{return Ok(());}
        let owner=self.pending[i as usize].definition;let args=self.pending[i as usize].arguments.clone();
        let mut f=self.functions[owner.0 as usize].clone();
        // Register active recursion before traversing callees, without cloning another body.
        self.pending[i as usize].active=true;
        for t in &mut f.params{*t=self.types.substitute(*t,owner,&args)?;}f.result=self.types.substitute(f.result,owner,&args)?;
        for l in &mut f.locals{l.ty=self.types.substitute(l.ty,owner,&args)?;}
        for t in &mut f.element_limits{*t=self.types.substitute(*t,owner,&args)?;}
        f.body=self.block(f.body,owner,&args)?;f.type_parameters.clear();f.calls.clear();
        self.pending[i as usize].body=Some(f);Ok(())
    }
    fn place(&mut self,mut p:Place,owner:FunctionId,args:&[Ty])->Result<Place>{
        p.root_ty=self.types.substitute(p.root_ty,owner,args)?;p.ty=self.types.substitute(p.ty,owner,args)?;
        for proj in &mut p.projections{if let Projection::Index{value,..}=proj{*value=self.expr(value.clone(),owner,args)?;}}Ok(p)
    }
    fn buffer(&mut self,id:u32,owner:FunctionId,args:&[Ty])->Result<u32>{let Ty::Buffer(i)=self.types.substitute(Ty::Buffer(id),owner,args)? else{unreachable!()};Ok(i)}
    fn expr(&mut self,mut e:Expr,owner:FunctionId,args:&[Ty])->Result<Expr>{
        e.ty=self.types.substitute(e.ty,owner,args)?;
        e.kind=match e.kind {
            Value::ReadPlace(p)=>Value::ReadPlace(self.place(p,owner,args)?),
            // `&*key` of `key: &Q` with `Q = str`: the reference is the address and the length,
            // so lending it on is reading `key` itself, not taking an address.
            Value::Address(p)=>{let p=self.place(p,owner,args)?;
                if p.pointer&&p.projections.is_empty()&&matches!(p.ty,Ty::Slice(_))&&matches!(e.ty,Ty::Borrow{mutable:false,..}){Value::ReadPlace(Place{pointer:false,root_ty:e.ty,ty:e.ty,..p})}else{Value::Address(p)}},
            Value::Stored(p,v)=>{let v=self.expr(*v,owner,args)?;Value::Stored(self.place(p,owner,args)?,Box::new(v))},
            Value::Move{slot,place}=>Value::Move{slot,place:self.place(place,owner,args)?},Value::Extract{wrapper,place}=>Value::Extract{wrapper,place:self.place(place,owner,args)?},
            Value::Discard(v)=>Value::Discard(Box::new(self.expr(*v,owner,args)?)),
            Value::Aggregate(v)=>Value::Aggregate(v.into_iter().map(|v|self.expr(v,owner,args)).collect::<Result<_>>()?),
            Value::Float(op,v)=>Value::Float(op,v.into_iter().map(|v|self.expr(v,owner,args)).collect::<Result<_>>()?),
            Value::BoxOp(op,id,v)=>Value::BoxOp(op,self.buffer(id,owner,args)?,v.into_iter().map(|v|self.expr(v,owner,args)).collect::<Result<_>>()?),
            Value::Bits(op,v)=>Value::Bits(op,v.into_iter().map(|v|self.expr(v,owner,args)).collect::<Result<_>>()?),
            Value::Signed(op,v)=>Value::Signed(op,v.into_iter().map(|v|self.expr(v,owner,args)).collect::<Result<_>>()?),
            Value::Sum{arm,payload}=>Value::Sum{arm,payload:Box::new(self.expr(*payload,owner,args)?)},
            Value::Retag(v,map)=>Value::Retag(Box::new(self.expr(*v,owner,args)?),map),
            Value::Repeat(v,n)=>Value::Repeat(Box::new(self.expr(*v,owner,args)?),n),Value::Project(v,n)=>Value::Project(Box::new(self.expr(*v,owner,args)?),n),
            Value::Index(v,j,n)=>Value::Index(Box::new(self.expr(*v,owner,args)?),Box::new(self.expr(*j,owner,args)?),n),
            Value::SliceOf(p,n)=>Value::SliceOf(self.place(p,owner,args)?,n),
            Value::SliceLen(v)=>Value::SliceLen(Box::new(self.expr(*v,owner,args)?)),
            Value::SliceAt(v,j,r)=>Value::SliceAt(Box::new(self.expr(*v,owner,args)?),Box::new(self.expr(*j,owner,args)?),r),
            Value::SliceIndex(v,j,tested)=>Value::SliceIndex(Box::new(self.expr(*v,owner,args)?),Box::new(self.expr(*j,owner,args)?),tested),
            Value::SliceCopy(t,v)=>Value::SliceCopy(Box::new(self.expr(*t,owner,args)?),Box::new(self.expr(*v,owner,args)?)),
            Value::SliceReadLe64(v,i)=>Value::SliceReadLe64(Box::new(self.expr(*v,owner,args)?),Box::new(self.expr(*i,owner,args)?)),
            Value::SlicePart(v,f,t)=>Value::SlicePart(Box::new(self.expr(*v,owner,args)?),Box::new(self.expr(*f,owner,args)?),Box::new(self.expr(*t,owner,args)?)),
            Value::SliceWithin(v,f,t)=>Value::SliceWithin(Box::new(self.expr(*v,owner,args)?),Box::new(self.expr(*f,owner,args)?),Box::new(self.expr(*t,owner,args)?)),
            Value::Widen(v)=>Value::Widen(Box::new(self.expr(*v,owner,args)?)),
            Value::ProofUtf8(v) => Value::ProofUtf8(Box::new(self.expr(*v, owner, args)?)),Value::Not(v)=>Value::Not(Box::new(self.expr(*v,owner,args)?)),
            Value::Binary(op,a,b)=>Value::Binary(op,Box::new(self.expr(*a,owner,args)?),Box::new(self.expr(*b,owner,args)?)),
            Value::Read(p,r)=>Value::Read(self.place(p,owner,args)?,r),
            Value::Host(op,p,vs)=>Value::Host(op,p.map(|p|self.place(p,owner,args)).transpose()?,vs.into_iter().map(|v|self.expr(v,owner,args)).collect::<Result<_>>()?),
            Value::Print(p,v,w)=>Value::Print(self.place(p,owner,args)?,Box::new(self.expr(*v,owner,args)?),w),
            Value::Exchange(a,b)=>Value::Exchange(self.place(a,owner,args)?,self.place(b,owner,args)?),
            // The entry call is planned like any call: its instance is the task's first function.
            Value::TaskSpawn{allocator,bytes,entry}=>Value::TaskSpawn{allocator:Box::new(self.expr(*allocator,owner,args)?),bytes:Box::new(self.expr(*bytes,owner,args)?),entry:Box::new(self.expr(*entry,owner,args)?)},
            Value::TaskStatus(v)=>Value::TaskStatus(Box::new(self.expr(*v,owner,args)?)),
            Value::TaskStep(v,w)=>Value::TaskStep(Box::new(self.expr(*v,owner,args)?),Box::new(self.expr(*w,owner,args)?)),Value::TaskFinish(v)=>Value::TaskFinish(Box::new(self.expr(*v,owner,args)?)),
            Value::Pause(v)=>Value::Pause(Box::new(self.expr(*v,owner,args)?)),
            Value::Call(mut call)=>{
                call.result=self.types.substitute(call.result,owner,args)?;
                call.args=call.args.into_iter().map(|v|self.expr(v,owner,args)).collect::<Result<_>>()?;
                call.substitutions=call.substitutions.into_iter().map(|t|self.types.substitute(t,owner,args)).collect::<Result<_>>()?;
                if self.functions[call.definition.0 as usize].required {
                    // A trait method: `Self` is known now, so the call is to that type's own method.
                    let this=call.substitutions[0];
                    let method=self.functions[call.definition.0 as usize].name.split_once('.').map(|(_,m)|m.to_string()).expect("a trait method name");
                    let own=self.types.nominal(this).and_then(|owner|self.by_name.get(&super::methods::name(owner,&method))).copied();
                    // Otherwise the impl for every type that meets its bounds, whose one parameter is `Self`.
                    let trait_name=self.functions[call.definition.0 as usize].name.split_once('.').map(|(t,_)|t.to_string()).expect("a trait method name");
                    let blanket=self.by_name.get(&super::methods::name(&format!("#{trait_name}"),&method)).copied();
                    let (implementation,substitutions)=match (own,blanket) {
                        (Some(own),_)=>(own,self.types.arguments(this)),
                        (None,Some(blanket))=>(blanket,vec![this]),
                        (None,None)=>return Err(error("a trait method was called on a type with no implementation")),
                    };
                    call.substitutions=substitutions;call.definition=implementation;call.contract_site=None;
                }
                let instance=self.register(call.definition,call.substitutions.clone())?;call.instance=Some(instance);self.expand(instance)?;Value::Call(call)
            },
            Value::TryAlloc{allocator,capacity,type_id,prefix_only}=>Value::TryAlloc{allocator:Box::new(self.expr(*allocator,owner,args)?),capacity:Box::new(self.expr(*capacity,owner,args)?),type_id:self.buffer(type_id-1,owner,args)?+1,prefix_only},
            Value::Capacity(v,id)=>Value::Capacity(Box::new(self.expr(*v,owner,args)?),self.buffer(id-1,owner,args)?+1),
            Value::AllocationKey(v,id)=>Value::AllocationKey(Box::new(self.expr(*v,owner,args)?),self.buffer(id-1,owner,args)?+1),
            Value::TryRelease(v,id)=>Value::TryRelease(Box::new(self.expr(*v,owner,args)?),self.buffer(id-1,owner,args)?+1),
            Value::Storage{op,receiver,index,value,buffer}=>Value::Storage{op,receiver:Box::new(self.expr(*receiver,owner,args)?),index:Box::new(self.expr(*index,owner,args)?),value:value.map(|v|self.expr(*v,owner,args).map(Box::new)).transpose()?,buffer:self.buffer(buffer,owner,args)?},
            // Leaves only, named one by one: a new kind of value that holds expressions or places
            // must say how its types are made concrete (a catch-all once skipped slices).
            leaf @ (Value::StaticStr(_)|Value::Unit|Value::U64(_)|Value::Bool(_)|Value::EntryConsole|Value::EntryInput|Value::EntryAllocator|Value::TaskWord|Value::InTask|Value::EntryHost(..))=>leaf,
        };Ok(e)
    }
    fn block(&mut self,body:Vec<Stmt>,owner:FunctionId,args:&[Ty])->Result<Vec<Stmt>>{body.into_iter().map(|s|Ok(match s{
        Stmt::Set(id,e)=>Stmt::Set(id,self.expr(e,owner,args)?),Stmt::StorePlace(p,op,e)=>Stmt::StorePlace(self.place(p,owner,args)?,op,self.expr(e,owner,args)?),
        Stmt::Eval(e)=>Stmt::Eval(self.expr(e,owner,args)?),Stmt::Return(e,x,at)=>Stmt::Return(self.expr(e,owner,args)?,x,at),
        Stmt::Replace(id,p,e)=>Stmt::Replace(id,self.place(p,owner,args)?,self.expr(e,owner,args)?),Stmt::If(c,a,b)=>Stmt::If(self.expr(c,owner,args)?,self.block(a,owner,args)?,self.block(b,owner,args)?),
        Stmt::While(c,b,invariants)=>Stmt::While(self.expr(c,owner,args)?,self.block(b,owner,args)?,invariants),
        Stmt::Break=>Stmt::Break,Stmt::Continue=>Stmt::Continue,
        Stmt::Match{value,arms}=>Stmt::Match{value:self.expr(value,owner,args)?,arms:arms.into_iter().map(|a|self.arm(a,owner,args)).collect::<Result<_>>()?},other=>other,
    })).collect()}
    fn arm(&mut self,mut a:Arm,owner:FunctionId,args:&[Ty])->Result<Arm>{a.payload=self.types.substitute(a.payload,owner,args)?;a.body=self.block(a.body,owner,args)?;Ok(a)}
}
/// Shared suspension validation and concrete instance selection for both targets.
pub(super) fn prepare(mut p:Program)->Result<Plan>{
    super::effects::check(&mut p)?;
    plan(p)
}
pub(super) fn plan(mut p:Program)->Result<Plan>{
    constraints(&mut p.functions,&mut p.types)?;
    let definitions=p.functions.iter().map(|f|Definition{name:f.name.clone(),checked_nodes:f.checked_nodes,checks:f.checks}).collect();
    let constant_limits=p.functions.iter().flat_map(|f|f.element_limits.iter().copied()).filter(|t|p.types.concrete(*t)).collect();
    let imports=2*usize::from(p.types.uses_console())+usize::from(p.types.uses_input())+usize::from(p.types.uses_heap());let helpers=6+if p.types.uses_heap(){6}else{0};
    let by_name=p.functions.iter().map(|f|(f.name.clone(),f.definition)).collect();
    let mut b=Builder{functions:p.functions,types:p.types,pending:vec![],limit:4096-imports-helpers,by_name};
    let nongeneric=b.functions.iter().enumerate().filter(|(_,f)|f.type_parameters.is_empty()).map(|(i,_)|FunctionId(i as u32)).collect::<Vec<_>>();
    // Only what `main` reaches is emitted. Every other nongeneric body is still expanded once, so
    // its layout and storage refusals do not depend on whether something calls it; those
    // instances are then dropped and do not count toward the function limit.
    let main=b.register(p.main,vec![])?;b.expand(main)?;
    // A `nopanic fn` is checked against what is emitted for it, so one `main` does not reach is
    // emitted too (with what it calls): its promise holds whoever calls it later.
    let promised=b.functions.iter().enumerate().filter(|(_,f)|f.type_parameters.is_empty()&&f.nopanic.is_some()).map(|(i,_)|FunctionId(i as u32)).collect::<Vec<_>>();
    for d in promised{let i=b.register(d,vec![])?;b.expand(i)?;}
    let reached=b.pending.len();b.limit=usize::MAX;
    for d in nongeneric{let i=b.register(d,vec![])?;b.expand(i)?;}
    b.types.validate_reference_storage()?;
    let unreached=b.pending.split_off(reached).into_iter().map(|i|Instance{definition:i.definition,arguments:i.arguments,function:i.body.expect("expanded concrete instance")}).collect();
    let instances=b.pending.into_iter().map(|i|Instance{definition:i.definition,arguments:i.arguments,function:i.body.expect("expanded concrete instance")}).collect();
    b.types.validate_reference_storage()?;
    Ok(Plan{definitions,instances,types:b.types,main,constant_limits,unreached})
}
