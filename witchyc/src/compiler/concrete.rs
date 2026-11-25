//! Sole logical-body -> concrete layout/register/frame boundary. No AST or ownership recheck.
use super::{ir as c, logical_body as s, layout::{Layouts, Kind}, semantic_types::{Types, Kind as SemanticKind},
    place::{Place,Projection}, logical_place, frame::Frame, match_arm::Arm, borrow::Pointee};
use crate::diagnostic::Diagnostic;
use std::collections::{BTreeMap, HashMap};
type Result<T> = std::result::Result<T, Diagnostic>;
fn error(message:&'static str)->Diagnostic { Diagnostic::new("W0403",0..0,message) }
struct Lower<'a> { types:&'a Types, layouts:Layouts, mapping:HashMap<s::Ty,c::Ty>,
    /// Buffers whose element type is laid out once the type naming them is complete,
    /// so that a type may hold itself behind a Buffer.
    waiting:Vec<(c::Ty,s::Ty)>, depth:usize,
    /// The Buffer<U64> type a task's block is allocated as, once one is.
    tasks:Option<u32>,
    /// The locals of the function being lowered that are indexed in place (`a[i]`): they live
    /// in the frame, so an element is reached at its address instead of chosen among locals.
    indexed:std::collections::BTreeSet<u32> }
impl Lower<'_> {
    fn ty(&mut self, ty:s::Ty)->Result<c::Ty> {
        self.depth+=1; let concrete=self.shape(ty); self.depth-=1; let concrete=concrete?;
        while self.depth==0 { let Some((buffer,element))=self.waiting.pop() else { break };
            self.depth+=1; let element=self.shape(element); self.depth-=1; self.layouts.buffer_element(buffer,element?)?; }
        Ok(concrete)
    }
    fn shape(&mut self, ty:s::Ty)->Result<c::Ty> {
        if let Some(t)=self.mapping.get(&ty) { return Ok(*t); }
        let concrete=match ty {
            s::Ty::Param{..}=>return Err(error("unbound function type parameter has no concrete layout")),
            s::Ty::Unit=>c::Ty::Unit,s::Ty::Bool=>c::Ty::Bool,s::Ty::Unsigned(s)=>c::Ty::Unsigned(s),s::Ty::I64=>c::Ty::I64,s::Ty::F64=>c::Ty::F64,
            s::Ty::Console=>c::Ty::Console,s::Ty::Input=>c::Ty::Input,s::Ty::Host(h)=>c::Ty::Host(h),s::Ty::Allocator=>c::Ty::Allocator,
            s::Ty::Borrow{mutable,pointee}=>{ let t=self.ty(pointee.ty())?;
                c::Ty::Borrow{mutable,pointee:Pointee::from_ty(t).ok_or_else(|| error("unsupported concrete borrow pointee"))?} },
            s::Ty::Slice(i)=>{ let element=self.ty(self.types.slice_element(i))?; self.layouts.slice(element)? },
            s::Ty::Weak(_)=>c::Ty::Unsigned(super::scalar::Scalar::U32),
            s::Ty::Boxed(_)=>c::Ty::Boxed,
            s::Ty::Buffer(i)=>{ let element=self.types.buffers()[i as usize].element; let name=self.types.type_name(element)?;
                let buffer=self.layouts.buffer(name)?; self.waiting.push((buffer,element)); buffer },
            s::Ty::Aggregate(_)=>{ let a=self.types.get(ty).expect("semantic aggregate");
                let kind=a.kind.clone(); let fields=a.fields.clone();
                let kind=match kind { SemanticKind::Record(_)=>Kind::Record(self.types.type_name(ty)?),SemanticKind::Array(t,n)=>Kind::Array(self.ty(t)?,n) };
                let fields=fields.into_iter().map(|f| Ok((f.name,self.ty(f.ty)?))).collect::<Result<Vec<_>>>()?;
                self.layouts.aggregate(kind,fields)? },
            s::Ty::Sum(_)=>{ let sum=self.types.sum(ty).expect("semantic sum"); let arms=sum.arms.clone(); let names=sum.names.clone(); let name=self.types.type_name(ty)?;
                let arms=arms.into_iter().map(|t| self.ty(t)).collect::<Result<Vec<_>>>()?; self.layouts.sum_layout(name,arms,names)? },
        };
        self.layouts.remember(concrete,self.types.properties(ty)); self.mapping.entry(ty).or_insert(concrete); Ok(concrete)
    }
    /// The byte stride of a slice's elements, from the slice reference's type.
    fn stride(&mut self,slice:s::Ty)->Result<u32> {
        let c::Ty::Borrow{pointee:Pointee::Slice(id),..}=self.ty(slice)? else { return Err(error("a slice operation takes a slice")) };
        u32::try_from(self.layouts.slice_element(id).1).map_err(|_| error("slice element stride limit"))
    }
    /// The type id (buffer index + 1) a task's block is allocated with: a Buffer of words that
    /// no source type names, so nothing but the task operations reaches its cells.
    fn task_buffer(&mut self)->Result<u32> {
        if let Some(id)=self.tasks { return Ok(id); }
        let buffer=self.layouts.buffer("#Task".into())?; self.layouts.buffer_element(buffer,c::Ty::U64)?;
        let c::Ty::Buffer(index)=buffer else { unreachable!() }; self.tasks=Some(index+1); Ok(index+1)
    }
    fn buffer(&mut self,id:u32)->Result<u32> { let c::Ty::Buffer(id)=self.ty(s::Ty::Buffer(id))? else { unreachable!() }; Ok(id) }
    fn place(&mut self,p:logical_place::Place,locals:&[u32])->Result<Place> {
        let root_ty=self.ty(p.root_ty)?; let ty=self.ty(p.ty)?; let mut current=p.root_ty; let mut projections=vec![];
        for projection in p.projections { match projection {
            logical_place::Projection::Field{ordinal}=>{
                let t=self.ty(current)?; let field=&self.layouts.get(t).expect("concrete field").fields[ordinal as usize];
                let slot=field.slot;let byte_offset=u32::try_from(field.byte_offset).map_err(|_|error("field offset exceeds memory32"))?;
                projections.push(Projection::Field{slot,byte_offset});
                current=self.types.get(current).expect("semantic field").fields[ordinal as usize].ty;
            },
            logical_place::Projection::Index{value,length}=>{
                if !p.pointer { self.indexed.insert(p.root); }
                let SemanticKind::Array(element,_)=self.types.get(current).expect("semantic array").kind else { unreachable!() };
                let t=self.ty(element)?; let stride=u32::try_from(self.layouts.storage(t)?.0).map_err(|_|error("array stride exceeds memory32"))?;
                let slots=self.layouts.flat(t).len() as u32;
                projections.push(Projection::Index{value:self.expr(value,locals)?,length,slots,stride}); current=element;
            },
        }}
        Ok(Place{root:locals[p.root as usize],root_ty,ty,pointer:p.pointer,projections})
    }
    fn expr(&mut self,e:s::Expr,locals:&[u32])->Result<c::Expr> {
        let ty=self.ty(e.ty)?;
        let kind=match e.kind {
            s::Value::StaticStr(bytes)=>{self.layouts.static_text=true; c::Value::StaticStr(bytes)},
            s::Value::Unit=>c::Value::Unit,s::Value::U64(n)=>c::Value::U64(n),s::Value::Bool(b)=>c::Value::Bool(b),
            s::Value::EntryConsole=>c::Value::EntryConsole,s::Value::TaskWord=>c::Value::TaskWord,s::Value::InTask=>c::Value::InTask,s::Value::EntryInput=>c::Value::EntryInput,s::Value::EntryHost(h,code)=>c::Value::EntryHost(h,code),s::Value::EntryAllocator=>c::Value::EntryAllocator,
            s::Value::ReadPlace(p)=>c::Value::ReadPlace(self.place(p,locals)?),
            s::Value::Address(p)=>c::Value::Address(self.place(p,locals)?),
            s::Value::Stored(p,v)=>{let v=self.expr(*v,locals)?;c::Value::Stored(self.place(p,locals)?,Box::new(v))},
            s::Value::Move{slot,place}=>c::Value::Move{slot,place:self.place(place,locals)?},
            s::Value::Extract{wrapper,place}=>c::Value::Extract{wrapper,place:self.place(place,locals)?},
            s::Value::Discard(v)=>c::Value::Discard(Box::new(self.expr(*v,locals)?)),
            s::Value::Aggregate(values)=>c::Value::Aggregate(values.into_iter().map(|v|self.expr(v,locals)).collect::<Result<_>>()?),
            s::Value::Sum{arm,payload}=>c::Value::Sum{arm,payload:Box::new(self.expr(*payload,locals)?)},
            s::Value::Repeat(v,n)=>c::Value::Repeat(Box::new(self.expr(*v,locals)?),n),
            s::Value::Retag(v,map)=>c::Value::Retag(Box::new(self.expr(*v,locals)?),map),
            s::Value::Project(v,ordinal)=>{let t=self.ty(v.ty)?;let slot=self.layouts.get(t).expect("projected record").fields[ordinal as usize].slot;
                c::Value::Project(Box::new(self.expr(*v,locals)?),slot)},
            s::Value::Index(v,i,n)=>c::Value::Index(Box::new(self.expr(*v,locals)?),Box::new(self.expr(*i,locals)?),n),
            // `&[T]` is two words: a reference to the first element, then the count.
            s::Value::SliceOf(p,n)=>{ let first=self.layouts.flat(ty)[0];
                c::Value::Aggregate(vec![c::Expr{ty:first,kind:c::Value::Address(self.place(p,locals)?)},c::Expr{ty:c::Ty::U64,kind:c::Value::U64(n as u64)}]) },
            s::Value::SliceLen(v)=>c::Value::Project(Box::new(self.expr(*v,locals)?),1),
            s::Value::SliceIndex(v,i,tested)=>{ let stride=self.stride(v.ty)?; c::Value::SliceIndex(Box::new(self.expr(*v,locals)?),Box::new(self.expr(*i,locals)?),stride,tested) },
            s::Value::SliceAt(v,i,r)=>{ let stride=self.stride(v.ty)?; c::Value::SliceAt(Box::new(self.expr(*v,locals)?),Box::new(self.expr(*i,locals)?),stride,r) },
            s::Value::SliceCopy(t,v)=>{ let stride=self.stride(v.ty)?; c::Value::SliceCopy(Box::new(self.expr(*t,locals)?),Box::new(self.expr(*v,locals)?),stride) },
            s::Value::SliceReadLe64(v,i)=>c::Value::SliceReadLe64(Box::new(self.expr(*v,locals)?),Box::new(self.expr(*i,locals)?)),
            s::Value::SlicePart(v,f,t)=>{
                let stride=self.stride(v.ty)?;
                let utf8=matches!(v.ty, s::Ty::Borrow { pointee: s::Pointee::Slice(id), .. } if self.types.is_str(id));
                c::Value::SlicePart(Box::new(self.expr(*v,locals)?),Box::new(self.expr(*f,locals)?),Box::new(self.expr(*t,locals)?),stride,utf8)
            },
            s::Value::SliceWithin(v,f,t)=>{
                let stride=self.stride(v.ty)?;
                let utf8=matches!(v.ty, s::Ty::Borrow { pointee: s::Pointee::Slice(id), .. } if self.types.is_str(id));
                c::Value::SliceWithin(Box::new(self.expr(*v,locals)?),Box::new(self.expr(*f,locals)?),Box::new(self.expr(*t,locals)?),stride,utf8)
            },
            s::Value::Widen(v) | s::Value::ProofUtf8(v) => c::Value::Widen(Box::new(self.expr(*v, locals)?)),
            s::Value::Float(op,vs)=>c::Value::Float(op,vs.into_iter().map(|v|self.expr(v,locals)).collect::<Result<_>>()?),
            s::Value::BoxOp(op, id, vs) => {
                use super::operations::BoxOp;
                self.layouts.boxed_operations |= matches!(op, BoxOp::New | BoxOp::Take);
                let element = self.buffer(id)?;
                let args = vs.into_iter().map(|v| self.expr(v, locals)).collect::<Result<_>>()?;
                c::Value::BoxOp(op, element, args)
            },
            s::Value::Bits(op,vs)=>c::Value::Bits(op,vs.into_iter().map(|v|self.expr(v,locals)).collect::<Result<_>>()?),
            s::Value::Signed(op,vs)=>c::Value::Signed(op,vs.into_iter().map(|v|self.expr(v,locals)).collect::<Result<_>>()?),
            s::Value::Not(v)=>c::Value::Not(Box::new(self.expr(*v,locals)?)),
            s::Value::Binary(op,a,b)=>c::Value::Binary(op,Box::new(self.expr(*a,locals)?),Box::new(self.expr(*b,locals)?)),
            s::Value::Call(call)=>{
                // The call's own result, or a `&mut T` it returns given where a `&T` is expected
                // (check.rs `argument` passes it on as shared: red-team review 35, a debug panic).
                debug_assert!(call.result==e.ty || matches!((call.result,e.ty),(s::Ty::Borrow{mutable:true,pointee:given},s::Ty::Borrow{mutable:false,pointee:wanted}) if given==wanted), "{:?} as {:?}", call.result, e.ty);
                c::Value::Call(call.instance.expect("registered concrete call instance"),call.args.into_iter().map(|a|self.expr(a,locals)).collect::<Result<_>>()?)},
            s::Value::Read(p,r)=>c::Value::Read(self.place(p,locals)?,r),
            s::Value::Host(op,p,vs)=>c::Value::Host(op,p.map(|p|self.place(p,locals)).transpose()?,vs.into_iter().map(|v|self.expr(v,locals)).collect::<Result<_>>()?),
            s::Value::Print(p,v,w)=>c::Value::Print(self.place(p,locals)?,Box::new(self.expr(*v,locals)?),w),
            s::Value::Exchange(a,b)=>c::Value::Exchange(self.place(a,locals)?,self.place(b,locals)?),
            s::Value::TaskSpawn{allocator,bytes,entry}=>{ let buffer=self.task_buffer()?;
                c::Value::TaskSpawn{allocator:Box::new(self.expr(*allocator,locals)?),bytes:Box::new(self.expr(*bytes,locals)?),entry:Box::new(self.expr(*entry,locals)?),buffer} },
            s::Value::TaskStatus(task)=>c::Value::TaskStatus(Box::new(self.expr(*task,locals)?)),
            s::Value::TaskStep(task,word)=>c::Value::TaskStep(Box::new(self.expr(*task,locals)?),Box::new(self.expr(*word,locals)?)),
            s::Value::TaskFinish(task)=>{ let buffer=self.task_buffer()?; c::Value::TaskFinish(Box::new(self.expr(*task,locals)?),buffer) },
            s::Value::Pause(token)=>c::Value::Pause(Box::new(self.expr(*token,locals)?)),
            s::Value::TryAlloc{allocator,capacity,type_id,prefix_only}=>c::Value::TryAlloc{allocator:Box::new(self.expr(*allocator,locals)?),capacity:Box::new(self.expr(*capacity,locals)?),type_id:self.buffer(type_id-1)?+1,prefix_only},
            s::Value::Capacity(v,id)=>c::Value::Capacity(Box::new(self.expr(*v,locals)?),self.buffer(id-1)?+1),
            s::Value::AllocationKey(v,id)=>c::Value::AllocationKey(Box::new(self.expr(*v,locals)?),self.buffer(id-1)?+1),
            s::Value::TryRelease(v,id)=>c::Value::TryRelease(Box::new(self.expr(*v,locals)?),self.buffer(id-1)?+1),
            s::Value::Storage{op,receiver,index,value,buffer}=>c::Value::Storage{op,receiver:Box::new(self.expr(*receiver,locals)?),index:Box::new(self.expr(*index,locals)?),
                value:value.map(|v|self.expr(*v,locals).map(Box::new)).transpose()?,buffer:self.buffer(buffer)?},
        };
        Ok(c::Expr{ty,kind})
    }
    fn arm(&mut self,a:s::Arm,locals:&[u32])->Result<Arm> {
        Ok(Arm{payload:self.ty(a.payload)?,binding:a.binding.map(|id|locals[id as usize]),body:self.block(a.body,locals)?})
    }
    fn block(&mut self,body:Vec<s::Stmt>,locals:&[u32])->Result<Vec<c::Stmt>> {
        body.into_iter().map(|stmt| Ok(match stmt {
            s::Stmt::Set(id,e)=>c::Stmt::Set(locals[id as usize],self.expr(e,locals)?),
            s::Stmt::StorePlace(p,op,e)=>c::Stmt::StorePlace(self.place(p,locals)?,op,self.expr(e,locals)?),
            s::Stmt::Eval(e)=>c::Stmt::Eval(self.expr(e,locals)?),
            s::Stmt::Return(e,expired,_)=>c::Stmt::Return(self.expr(e,locals)?,expired),
            s::Stmt::Expire(ids)=>c::Stmt::Expire(ids),
            s::Stmt::Replace(id,p,e)=>c::Stmt::Replace(id,self.place(p,locals)?,self.expr(e,locals)?),
            s::Stmt::If(c,a,b)=>c::Stmt::If(self.expr(c,locals)?,self.block(a,locals)?,self.block(b,locals)?),
            s::Stmt::While(c,b,_)=>c::Stmt::While(self.expr(c,locals)?,self.block(b,locals)?),
            s::Stmt::Break=>c::Stmt::Break,s::Stmt::Trap(_)=>c::Stmt::Trap,s::Stmt::Continue=>c::Stmt::Continue,
            s::Stmt::Match{value,arms} if matches!(value.ty,s::Ty::Borrow{..})=>{
                let s::Ty::Borrow{pointee,..}=value.ty else { unreachable!() };
                let sum=self.ty(pointee.ty())?;
                let offsets=self.layouts.sum(sum).filter(|sum|sum.stored.is_some()).ok_or_else(||error("a match through a reference needs a sum that can be stored"))?
                    .arm_offsets.iter().map(|offset|u32::try_from(*offset).map_err(|_|error("variant offset exceeds memory32"))).collect::<Result<Vec<_>>>()?;
                c::Stmt::MatchRef{value:self.expr(value,locals)?,offsets,arms:arms.into_iter().map(|a|self.arm(a,locals)).collect::<Result<_>>()?,tag:None} },
            s::Stmt::Match{value,arms}=>c::Stmt::Match{value:self.expr(value,locals)?,arms:arms.into_iter().map(|a|self.arm(a,locals)).collect::<Result<_>>()?},
        })).collect()
    }
    fn function(&mut self,f:s::Function)->Result<c::Function> {
        let mut ranges=Vec::new();let mut locals=Vec::new();
        for local in &f.locals {
            debug_assert_eq!(local.id as usize,ranges.len());
            let _=local.span; // semantic source position never becomes physical identity
            ranges.push(locals.len() as u32); let t=self.ty(local.ty)?;locals.extend(self.layouts.flat(t));
        }
        let mut params=Vec::new(); for t in f.params {let t=self.ty(t)?;params.extend(self.layouts.flat(t));}
        if params.len()>32 {return Err(Diagnostic::new("W0401",0..0,"at most 32 scalar parameters"));}
        let result=self.ty(f.result)?;
        if self.layouts.flat(result).len()>32 {return Err(Diagnostic::new("W0401",0..0,"a result holds at most 32 numbers: return a large array through `&mut`"));}
        self.indexed.clear();
        let body=self.block(f.body,&ranges)?;
        // Arrays indexed in place join the addressed locals in the frame (benchmark: `a[i]` on a
        // [U64; 16] chose among 16 locals, 4.6x slower than the same array through a slice).
        let indexed=std::mem::take(&mut self.indexed);
        let addressed=f.addressed.into_iter().chain(indexed).collect::<std::collections::BTreeSet<_>>().into_iter()
            .map(|id|Ok((ranges[id as usize],self.ty(f.locals[id as usize].ty)?))).collect::<Result<BTreeMap<_,_>>>()?;
        let frame=Frame::plan(&addressed,&self.layouts)?;
        // A frame larger than the whole stack could never be entered: say so here rather than trap
        // at the call (a [U64; 256] is 2 KiB; the default stack is 1 KiB). A function that may pause
        // runs only on a task's stack, chosen when the task is spawned, and is checked as it runs.
        let stack=super::allocator::heap().stack as u64;
        if frame.bytes as u64>stack && !f.suspends {return Err(Diagnostic::new("W0401",0..0,format!("a function's frame needs {} bytes, more than the {stack}-byte stack: build with a larger `--stack-bytes`",frame.bytes)));}
        // What lives in the frame is read and written there, not in its locals: only the others
        // count (an indexed [U8; 256] is one place in memory, not 256 locals).
        if (0..locals.len() as u32).filter(|slot|!frame.slots.contains_key(slot)).count()>256 {return Err(Diagnostic::new("W0401",0..0,"at most 256 scalar locals outside the frame: index a large array in place, or keep it in a Vec"));}
        let steps=f.steps.iter().map(|id|ranges[*id as usize]).collect();
        // A marked local keeps its mark only where its first slot is its own: a local with no
        // slots would share its number with the next one, which a source local could be.
        let overwrites=f.overwrites.iter().map(|id|ranges[*id as usize]).filter(|start|ranges.iter().filter(|other|*other==start).count()==1).collect();
        Ok(c::Function{params,result,locals,body,frame,steps,overwrites})
    }
}
pub(super) fn program(program:s::Program)->Result<c::Program> {
    lower_program(program, true)
}
pub(super) fn package_program(program:s::Program)->Result<c::Program> {
    lower_program(program, false)
}
fn lower_program(mut program:s::Program, executable:bool)->Result<c::Program> {
    let main_parameters=program.main_parameters;
    let secret_slots=std::mem::take(&mut program.secret_slots);
    lower_plan(super::instances::prepare(program)?,main_parameters,secret_slots,executable)
}
#[cfg(all(target_arch = "aarch64", any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
pub(super) fn from_plan(plan:super::instances::Plan, main_parameters:usize, secret_slots:Vec<(String,u8)>)->Result<c::Program> {
    lower_plan(plan, main_parameters, secret_slots, true)
}
fn lower_plan(plan:super::instances::Plan, main_parameters:usize, secret_slots:Vec<(String,u8)>, executable:bool)->Result<c::Program> {
    let main_params=plan.instances.get(plan.main as usize).map(|i|i.function.locals.iter().take(main_parameters).map(|l|l.ty).collect::<Vec<_>>()).unwrap_or_default();
    // What each emitted function holds, by name: a refusal below says which one imports it.
    let holders=plan.instances.iter().map(|i|(i.function.name.split('#').next().unwrap_or("").split('@').next().unwrap_or("").to_string(),
        i.function.locals.iter().find_map(|l|l.span).map(|(s,e)|s..e).unwrap_or(0..0),i.function.locals.iter().map(|l|l.ty).collect::<Vec<_>>())).collect::<Vec<_>>();
    let n=plan.definitions.len();let m=plan.instances.len();let total=plan.definitions.iter().map(|f|f.checks).sum::<u32>();
    let checks=plan.definitions.iter().enumerate().map(|(id,f)|{
        let count=plan.instances.iter().filter(|i|i.definition.0 as usize==id).count();
        format!("{{\"name\":\"{}\",\"checks\":{},\"instances\":{count}}}",f.name,f.checks)
    }).collect::<Vec<_>>().join(",");
    let receipt=format!("{{\"contract\":\"witchy.logical-body.v2\",\"selected_definitions\":{n},\"checked_bodies\":{total},\"concrete_instances\":{m},\"body_checks\":[{checks}]}}");
    let instance_receipt=plan.instances.iter().enumerate().map(|(ordinal,i)|{
        let name=&plan.definitions[i.definition.0 as usize].name;
        let args=i.arguments.iter().map(|t|Ok(format!("\"{}\"",plan.types.type_name(*t)?))).collect::<Result<Vec<_>>>()?.join(",");
        Ok(format!("{{\"definition\":\"{name}\",\"type_arguments\":[{args}],\"ordinal\":{ordinal}}}"))
    }).collect::<Result<Vec<_>>>()?.join(",");
    let selected=plan.definitions.iter().map(|f|(f.name.clone(),f.checked_nodes)).collect();
    let definitions=plan.instances.iter().map(|i|i.definition.0).collect::<Vec<_>>();
    let names=plan.instances.iter().map(|i|{
        let name=plan.definitions[i.definition.0 as usize].name.split('@').next().unwrap_or("").replace('#',"").to_string();
        let args=i.arguments.iter().filter_map(|t|plan.types.type_name(*t).ok()).map(|n|n.split('@').next().unwrap_or("").to_string()).collect::<Vec<_>>();
        if args.is_empty() { name } else { format!("{name}<{}>",args.join(", ")) }
    }).collect::<Vec<_>>();
    let mut lower=Lower{types:&plan.types,layouts:Layouts::default(),mapping:HashMap::new(),waiting:vec![],depth:0,tasks:None,indexed:Default::default()};lower.layouts.initialize(&plan.types);
    // A library may name `Console<Read>` in functions the program never reaches: the module
    // imports the input function only when an emitted function holds the grant or a reference to it.
    let input=plan.instances.iter().any(|i|i.function.locals.iter().any(|l|matches!(l.ty,s::Ty::Input|s::Ty::Borrow{pointee:s::Pointee::Input,..})));
    lower.layouts.input(input);
    // Likewise output (2026-11-22: `import net` names `Console<Write>` in functions a program
    // with no Console never reaches, and its module imported the print functions, which a run
    // that grants no output refuses at launch).
    let output=plan.instances.iter().any(|i|i.function.locals.iter().any(|l|matches!(l.ty,s::Ty::Console|s::Ty::Borrow{pointee:s::Pointee::Console,..})));
    lower.layouts.output(output);
    // Likewise the file imports: present when an emitted function holds a directory or file.
    let mut host=[false;super::host::Family::ALL.len()];
    for local in plan.instances.iter().flat_map(|i|i.function.locals.iter()) {
        if let s::Ty::Host(h)|s::Ty::Borrow{pointee:s::Pointee::Host(h),..}=local.ty && h.imported() { host[h.family().ordinal()]=true; }
    }
    // A refusal found while laying out a declared type has no position: it names the type.
    let in_type=|t:s::Ty,e:Diagnostic| match plan.types.type_name(t) {
        Ok(name) if e.span==(0..0) && !e.message.starts_with("the whole program") && !e.message.starts_with("in type `") =>
            Diagnostic::new(e.code,0..0,format!("in type `{name}`: {}",e.message)),
        _=>e };
    for t in &plan.types.order {if plan.types.concrete(*t)&&!matches!(t,s::Ty::Sum(_)){lower.ty(*t).map_err(|e|in_type(*t,e))?;}}
    for t in plan.constant_limits.iter().chain(plan.instances.iter().flat_map(|i|i.function.element_limits.iter())) {let t=lower.ty(*t)?;
        if lower.layouts.flat(t).len()>super::allocator::ELEMENT_CARRIERS {
            return Err(Diagnostic::new("W0401",0..0,"initialized Buffer operations require at most 30 element carriers"));
        }
    }
    for t in &plan.types.order {if plan.types.concrete(*t){lower.ty(*t).map_err(|e|in_type(*t,e))?;}}
    // Likewise the allocator and the heap: a library may name `Allocator` or a Buffer in
    // functions the program never reaches (`Show.push_on`), and a program that never holds one
    // must run without an allocator grant.
    let holds=|wanted:&dyn Fn(s::Ty)->bool| plan.instances.iter().any(|i|i.function.locals.iter().any(|l| wanted(l.ty) || matches!(l.ty,s::Ty::Borrow{pointee,..} if wanted(pointee.ty()))));
    let allocator=holds(&|t| t==s::Ty::Allocator);
    let heap=allocator || holds(&|t| matches!(t,s::Ty::Buffer(_)) || plan.types.get(t).is_some_and(|a| a.fields.iter().any(|f| matches!(f.ty,s::Ty::Buffer(_)))));
    // A body nothing reaches is not emitted, but the limits its types decide hold for it as for
    // any other, so a function is not refused only once something calls it: 32 numbers of
    // parameters and of result, and 256 locals outside the frame. Arrays are left out of that
    // count here, since one indexed in place lives in the frame and only lowering says which:
    // the count never refuses what lowering would accept. Laid out apart, so nothing about these
    // bodies reaches the module (a Buffer one names must not make it import the allocator).
    let mut probe=Lower{types:&plan.types,layouts:Layouts::default(),mapping:HashMap::new(),waiting:vec![],depth:0,tasks:None,indexed:Default::default()};
    probe.layouts.initialize(&plan.types); probe.layouts.input(true); probe.layouts.heap(true, true);
    for i in &plan.unreached {
        let name=i.function.name.split('#').next().unwrap_or("").split('@').next().unwrap_or("").to_string();
        let at=i.function.locals.iter().find_map(|l| l.span).map(|(s,e)| s..e).unwrap_or(0..0);
        let refuse=|message:&str| Diagnostic::new("W0401",at.clone(),format!("in `{name}`: {message}"));
        let mut params=0;
        for t in &i.function.params { let t=probe.ty(*t)?; params+=probe.layouts.flat(t).len(); }
        if params>32 { return Err(refuse("at most 32 scalar parameters")); }
        let result=probe.ty(i.function.result)?;
        if probe.layouts.flat(result).len()>32 { return Err(refuse("a result holds at most 32 numbers: return a large array through `&mut`")); }
        let mut locals=0;
        for local in &i.function.locals {
            if matches!(local.ty, s::Ty::Aggregate(_)) && plan.types.get(local.ty).is_some_and(|a| matches!(a.kind, super::semantic_types::Kind::Array(..))) { continue; }
            let t=probe.ty(local.ty)?; locals+=probe.layouts.flat(t).len();
        }
        if locals>256 { return Err(refuse("at most 256 scalar locals outside the frame: index a large array in place, or keep it in a Vec")); }
    }
    // A refusal found while laying out one function's instance has no position of its own:
    // it names the function, at its first parameter (from adversarial review round 12).
    let functions=plan.instances.into_iter().map(|i| {
        let name=i.function.name.split('#').next().unwrap_or("").to_string();
        let at=i.function.locals.iter().find_map(|l| l.span).map(|(s,e)| s..e);
        lower.function(i.function).map_err(|e| if e.span==(0..0) && !name.is_empty() {
            let message=format!("in `{name}`: {}",e.message);
            Diagnostic::new(e.code,at.unwrap_or(0..0),message)
        } else { e })
    }).collect::<Result<_>>()?;
    lower.layouts.heap(allocator, heap || lower.layouts.boxed_operations);
    let mut program=c::Program{functions,main:plan.main,output,input,host,layouts:lower.layouts,semantic_receipt:receipt,
        selected_definitions:selected,instance_receipt:format!("[{instance_receipt}]"),names,tasks:Default::default(),definitions,secret_slots,proved_index:Vec::new(),proved_slices:Vec::new(),proved_unreachable:Vec::new(),proved_arithmetic:Vec::new()};
    // These families have no root of their own. Import them when a function uses them.
    use super::host::Family;
    for family in [Family::FileSnapshot, Family::Crypto] {
        if program.functions.iter().any(|f| super::pause::any_expr(&f.body, &|e| matches!(e.kind, c::Value::Host(op, ..) if op.family() == family))) {
            program.host[family.ordinal()] = true;
        }
    }
    // Functions that may pause are made resumable (pause.rs).
    super::pause::lower(&mut program)?;
    if executable { granted_by_main(&plan.types,&main_params,&holders,&program)?; }
    Ok(program)
}

/// Every import a module makes is one a run grants by `main`'s parameters, or one granted to
/// every run: a module the engine would refuse at launch for an import its grants do not cover
/// is refused here instead (2026-11-22: `import net` with no Console built, passed review, and
/// was then rejected at launch). Authority enters only through `main`'s parameters (caps.rs),
/// so what they hold, at any depth, is what a run of this program can be granted.
fn granted_by_main(types:&Types, params:&[s::Ty], holders:&[(String,std::ops::Range<usize>,Vec<s::Ty>)], program:&c::Program)->Result<()> {
    fn walk(types:&Types, ty:s::Ty, out:&mut Vec<s::Ty>, seen:&mut std::collections::BTreeSet<u32>, depth:usize) {
        if depth>256 { return; }
        match ty {
            s::Ty::Console|s::Ty::Input|s::Ty::Allocator|s::Ty::Host(_) => out.push(ty),
            s::Ty::Borrow{pointee,..} => if !matches!(pointee,s::Pointee::Param(..)) { walk(types,pointee.ty(),out,seen,depth+1) },
            s::Ty::Aggregate(id) => if seen.insert(id) { for field in types.get(ty).map(|a| a.fields.as_slice()).unwrap_or_default() { walk(types,field.ty,out,seen,depth+1); } },
            s::Ty::Sum(id) => if seen.insert(u32::MAX-id) { for arm in types.sum(ty).map(|s| s.arms.as_slice()).unwrap_or_default() { walk(types,*arm,out,seen,depth+1); } },
            _ => {}
        }
    }
    let mut held=Vec::new(); let mut seen=Default::default();
    for p in params { walk(types,*p,&mut held,&mut seen,0); }
    // Named at the first emitted function holding it (a `nopanic fn` is emitted though `main`
    // does not reach it).
    let refuse=|what:&str, wanted:&dyn Fn(s::Ty)->bool| {
        let mut held_by=Vec::new();
        let found=holders.iter().find(|(_,_,locals)| locals.iter().any(|t| { held_by.clear(); walk(types,*t,&mut held_by,&mut Default::default(),0); held_by.iter().any(|t| wanted(*t)) }));
        let (at,who)=found.map_or((0..0,String::new()),|(name,at,_)|(at.clone(),format!(" (`{name}` holds it)")));
        Err(Diagnostic::new("W0401",at,format!("the module would import {what}{who}, which no parameter of `main` grants, so every run would refuse it at launch: give `main` that capability, or leave out what uses it")))
    };
    if program.output && !held.contains(&s::Ty::Console) { return refuse("the output functions (`Console<Write>`)",&|t| t==s::Ty::Console); }
    if program.input && !held.contains(&s::Ty::Input) { return refuse("the input functions (`Console<Read>`)",&|t| t==s::Ty::Input); }
    if program.layouts.uses_heap() && !held.contains(&s::Ty::Allocator) { return refuse("the allocator (`Allocator`)",&|t| t==s::Ty::Allocator); }
    for family in super::host::Family::ALL {
        // Public crypto needs no grant. The snapshot query shares the checked Files grant.
        if family.authorize().is_none() || !program.host[family.ordinal()] { continue; }
        if !held.iter().any(|t| matches!(t,s::Ty::Host(h) if h.imported() && h.family()==family)) { return refuse(&format!("the {family:?} host functions"),&|t| matches!(t,s::Ty::Host(h) if h.imported() && h.family()==family)); }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn test_layouts(source:&str, requests:&[crate::ast::Type])->Result<(Layouts,Vec<c::Ty>)> {
    let syntax=crate::compiler::parse(source)?; let mut types=Types::new(&syntax)?;
    let ids=requests.iter().map(|t|types.ty(t)).collect::<Result<Vec<_>>>()?;
    let mut lower=Lower{types:&types,layouts:Layouts::default(),mapping:HashMap::new(),waiting:vec![],depth:0,tasks:None,indexed:Default::default()};lower.layouts.initialize(&types);
    for t in &types.order {lower.ty(*t)?;}
    let ids=ids.into_iter().map(|t|lower.ty(t)).collect::<Result<_>>()?;
    Ok((lower.layouts,ids))
}

#[cfg(test)]
mod granted_tests {
    fn lowered(source:&str)->Result<super::c::Program,crate::diagnostic::Diagnostic> {
        let syntax=crate::compiler::parse(source)?; super::program(super::super::check::program(&syntax)?)
    }
    /// A function that prints, which `main` never reaches, does not make the module import the
    /// output functions (2026-11-22: `import net` with no Console was rejected at launch).
    #[test]
    fn output_is_imported_only_when_a_reached_function_holds_the_console() {
        let unreached="fn shout(console: &Console<Write>):\n    console.print_int(1)\n\nfn main(clock: Clock):\n    let now = clock.now_ms()\n";
        let program=lowered(unreached).expect("lowers");
        assert!(!program.output, "no reached function holds a Console");
        let reached="fn shout(console: &Console<Write>):\n    console.print_int(1)\n\nfn main(console: Console<Write>):\n    shout(&console)\n";
        assert!(lowered(reached).expect("lowers").output);
    }
    /// A module importing what no parameter of `main` grants is refused at build, not at launch:
    /// a `nopanic fn` is emitted though `main` does not reach it, and it holds a Clock.
    #[test]
    fn an_import_main_cannot_be_granted_is_refused_at_build() {
        let source="nopanic fn tick(clock: &Clock) -> U64:\n    return clock.now_ms()\n\nfn main():\n    return\n";
        let refusal=lowered(source).err().map(|e| e.message.to_string()).unwrap_or_default();
        assert!(refusal.contains("which no parameter of `main` grants"), "{refusal}");
        assert!(lowered("nopanic fn tick(clock: &Clock) -> U64:\n    return clock.now_ms()\n\nfn main(clock: Clock):\n    let n = tick(&clock)\n").is_ok());
    }
}
