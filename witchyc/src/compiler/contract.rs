//! Resolved source clauses. Parameter bindings and ghost return have distinct roles.
use crate::{ast as a, diagnostic::Diagnostic};
use std::{collections::{BTreeMap,BTreeSet},ops::Range};
type Result<T> = std::result::Result<T,Diagnostic>;
fn error(m: impl Into<std::borrow::Cow<'static, str>>)->Diagnostic{Diagnostic::new("W0401",0..0,m)}
/// A natural number in a clause. `-` stops at zero and `/` by zero is zero, as for Lean's naturals;
/// the run-time operations trap instead, and a proved body shows they do not.
#[derive(Clone,Debug,PartialEq,Eq,Hash)]
pub(super) enum Math { Literal(u64),
    /// A literal read as an integer: it stands beside I64 values.
    Int(i64), Binding(u32), Result, Add(Box<Self>,Box<Self>), Sub(Box<Self>,Box<Self>), Mul(Box<Self>,Box<Self>), Div(Box<Self>,Box<Self>), Mod(Box<Self>,Box<Self>),
    /// What a method that returns one field of its receiver would return for this local: now, or (`before`) on entry.
    Observe{root:u32,function:super::logical_body::FunctionId,before:bool},
    /// A number named directly: the field at `path` of this local, or of what it refers to; an empty path is `*name`.
    Field{root:u32,path:Vec<u32>,ty:super::logical_body::Ty,before:bool},
    /// Between an I64 and its 64 bits (the walk's reading of `I64.bits` and `I64.from_bits`):
    /// `true`, the integer those bits spell; `false`, the bits of an integer. No clause writes it.
    Cast(bool,Box<Math>) }
#[derive(Clone,Debug,PartialEq,Eq,Hash)]
pub(super) enum Prop { Bool(bool), Compare(&'static str,Math,Math), Not(Box<Self>), Binary(&'static str,Box<Self>,Box<Self>),
    /// A Bool value read as a proposition: it is 1.
    Truth(Math) }
#[derive(Clone,Debug)]
pub(super) struct Clause {pub value:Prop,pub span:Range<usize>,
    /// The clause is stated for one variant of a `Result` result: `Ok` is 0, `Err` is 1.
    pub arm:Option<u8>}
#[derive(Clone,Debug)]
pub(super) struct Contract {pub requires:Vec<Clause>,pub ensures:Vec<Clause>,
    /// Facts every value received already satisfies (a type's invariant: every place that makes or
    /// changes one proves it), assumed on entry and never shown by a caller.
    pub assumed:Vec<Clause>,
    /// What the function says it does to the elements of a buffer it is lent (`elements.` clauses).
    pub effects:Vec<ElementClause>,
    /// Invariants over the elements of a container field, of the types the function receives
    /// (docs/proposal-element-invariants.md).
    pub elements:Vec<ElementInvariant>,pub nodes:usize,pub storage:Option<super::storage_contract::StorageContract>,pub record:Option<super::record_contract::RecordContract>,pub memory:Option<super::memory_contract::MemoryContract>}
/// What a function does to the cells of one buffer, as an `ensures` states it.
#[derive(Clone,Debug,PartialEq)]
pub(super) enum ElementKind {
    /// Every cell holds what it held.
    Kept,
    /// Every cell below this index holds what it held.
    KeptBelow(Math),
    /// Every cell but the one at this index holds what it held.
    KeptExcept(Math),
    /// The cell at this index holds the value of this parameter.
    Holds(Math,u32),
    /// Every value in a cell below this index was in a cell below it before.
    Among(Math) }
#[derive(Clone,Debug)]
pub(super) struct ElementClause {pub arm:Option<u8>,
    /// The buffer: a `Field` of a parameter.
    pub buffer:Math,pub kind:ElementKind,pub span:Range<usize>}
/// `forall i in (0, v.len()): P`, of the value of one parameter: `P` reads an element's number
/// fields through an `ELEMENT_METHOD` step after the container's fields, and the value's own.
#[derive(Clone,Debug)]
pub(super) struct ElementInvariant {
    /// How many elements there are: the container's length, read through its method.
    pub upper:Math,pub body:Prop,pub span:Range<usize>}
/// The root of a field read from the result: no local has this number.
pub(super) const RESULT:u32=u32::MAX;
struct Resolve<'a>{scope:&'a BTreeMap<String,u32>,result:bool,
    /// The clause is an `ensures`: it may read a field of the result.
    ensures:bool,nodes:usize,
    /// Methods the clauses call that return an I64.
    observed:Vec<super::logical_body::FunctionId>,
    /// Names that are Bool, and whether the result is.
    bools:&'a BTreeSet<u32>,bool_result:bool,
    /// In a `match result:` arm, its binder and the arm (`Ok` 0, `Err` 1): the value the variant
    /// holds, read as a step of the result (`#arm0` to the field resolver).
    arm:Option<(String,u8)>,
    /// Names that are numbers or Bools; any other name is read only through a method.
    scalars:&'a BTreeSet<u32>,
    /// Names that are I64, and whether the result is. An I64 is an integer; it is compared only with integers.
    integers:&'a BTreeSet<u32>,integer_result:bool,
    /// The method of this name on a local's type.
    observe:Observe<'a>,
    /// The number at these field names of a local (`true`: written with `*`), if the clause may read it.
    field:Fields<'a>,
    /// `old(...)` is allowed: an `ensures` or a loop invariant.
    old:bool}
/// The method, and whether it returns an I64.
pub(super) type Observe<'a>=&'a dyn Fn(u32,&str)->Option<(super::logical_body::FunctionId,bool)>;
pub(super) type Fields<'a>=&'a dyn Fn(u32,&[&str],bool)->Option<(Vec<u32>,super::logical_body::Ty)>;
fn scalar_place(e: &a::Expr) -> bool {
    match &e.kind {
        a::Expression::Field(..) | a::Expression::Index(..) | a::Expression::Unary("*", _) => true,
        a::Expression::Call(c, _) => matches!(&c.kind, a::Expression::Field(ns, op)
            if op=="variant_value" && matches!(&ns.kind, a::Expression::Name(n) if n=="memory")),
        _ => false,
    }
}
/// A refusal with no place of its own takes the place of the part of the clause it is about.
fn placed(mut refusal:Diagnostic,e:&a::Expr)->Diagnostic{if refusal.span==(0..0)&&e.span!=(0..0){refusal.span=e.span.clone();}refusal}
/// What a part of a clause is, said in a refusal: the construct outside the clauses' vocabulary.
fn construct(e:&a::Expr)->String{
    match &e.kind{
        a::Expression::Call(callee,arguments)=>match &callee.kind{
            a::Expression::Field(_,method) if !arguments.is_empty()=>format!("a method call with arguments (`.{method}(...)`), and a clause reads a number only through a method that takes nothing but its receiver"),
            a::Expression::Field(_,method)=>format!("a method call (`.{method}()`) on a value that is not a parameter, a field of one, or `result`"),
            a::Expression::Name(function)=>format!("a call of the function `{function}`: a clause calls no function, only `old`, `memory.` observations and methods that return a field"),
            _=>"a call of a computed function".into(),
        },
        a::Expression::Index(..)=>"an element at an index that is not a literal".into(),
        a::Expression::Unary("&"|"&mut",_)=>"a reference".into(),
        a::Expression::Unary(op,_)=>format!("the operator `{op}`"),
        a::Expression::Binary(op,..)=>format!("the operator `{op}` here"),
        a::Expression::Name(n)=>format!("the name `{n}`"),
        _=>"this form".into(),
    }
}
impl Resolve<'_>{
    fn count(&mut self,depth:usize)->Result<()>{self.nodes+=1;if depth>64||self.nodes>4096{return Err(error("function contracts expression complexity limit"));}Ok(())}
    /// A refusal made at a part of a clause is placed at that part, not at the function: a
    /// type's invariant is resolved inside each function that receives the type, and the
    /// refusal points at the invariant's line.
    fn math(&mut self,e:&a::Expr,depth:usize)->Result<Math>{self.math_at(e,depth).map_err(|refusal|placed(refusal,e))}
    fn math_at(&mut self,e:&a::Expr,depth:usize)->Result<Math>{self.count(depth)?;Ok(match &e.kind{
        a::Expression::Integer(n)=>Math::Literal(*n),
        // A negated literal is an integer; anything else negated is taken from zero.
        a::Expression::Unary("-",v)=>match &v.kind{
            a::Expression::Integer(n) if *n<=9223372036854775808=>Math::Int((*n as i128).wrapping_neg() as i64),
            a::Expression::Integer(_)=>return Err(error("function contracts integer literal exceeds the range of I64")),
            _=>Math::Sub(Box::new(Math::Int(0)),Box::new(self.math(v,depth+1)?))},
        a::Expression::Name(n) if n=="result"=>{if !self.result{return Err(error("function contracts name `result` only in ensures, and only when it is a number or Bool"));}if self.bool_result{return Err(error("function contracts compare numbers; a Bool is a proposition"));}Math::Result},
        a::Expression::Name(n) if self.arm.as_ref().is_some_and(|(name,_)|name==n)=>{let arm=format!("#arm{}",self.arm.as_ref().map(|(_,k)|*k).unwrap_or(0));
            let(path,ty)=(self.field)(RESULT,&[arm.as_str()],false).ok_or_else(||error("function contracts read the value a variant holds when it is a number, or its number fields"))?;
            if matches!(ty,super::logical_body::Ty::Bool){return Err(error("function contracts compare numbers; a Bool is a proposition"));}
            Math::Field{root:RESULT,path,ty,before:false}},
        a::Expression::Name(n)=>{let id=*self.scope.get(n).ok_or_else(||error("function contracts clause name is not a parameter"))?;if self.bools.contains(&id){return Err(error("function contracts compare numbers; a Bool is a proposition"));}
            if !self.scalars.contains(&id){return Err(error("function contracts name only numbers and Bools; they read a record through a method that returns one of its fields, as in `items.len()`"));}Math::Binding(id)},
        a::Expression::Call(c,args) if matches!(&c.kind, a::Expression::Name(n) if n == "variant_index") => {
            let [target] = args.as_slice() else { return Err(error("variant_index takes one named sum or field")); };
            self.part(&a::Expr { kind:a::Expression::Field(Box::new(target.clone()), "#variant".into()), span:e.span.clone() })?
        },
        a::Expression::Call(c,args) if matches!(&c.kind, a::Expression::Field(base,op)
            if matches!(&base.kind, a::Expression::Name(n) if n == "memory") && matches!(op.as_str(), "box_id" | "pointer_bits")) => {
            let [target] = args.as_slice() else { return Err(error("a pointer observation takes one box or weak pointer path")); };
            let a::Expression::Field(_,op) = &c.kind else { unreachable!() };
            let field = if op == "box_id" { "#incarnation" } else { "#address" };
            self.part(&a::Expr { kind:a::Expression::Field(Box::new(target.clone()), field.into()), span:e.span.clone() })?
        },
        a::Expression::Call(c,args) if matches!(&c.kind,a::Expression::Name(n) if n=="old")&&args.len()==1=>{
            if !self.old{return Err(error("function contracts old is for ensures and loop invariants"));}
            match self.math(&args[0],depth+1)?{Math::Observe{root,function,before:false}=>Math::Observe{root,function,before:true},Math::Field{root,path,ty,before:false} if root!=RESULT=>Math::Field{root,path,ty,before:true},_=>return Err(error("function contracts old takes a method call on a parameter, as in `old(items.len())`"))}},
        a::Expression::Call(c,args) if args.is_empty()&&matches!(&c.kind,a::Expression::Field(v,_) if matches!(&v.kind,a::Expression::Name(_)))=>{
            let a::Expression::Field(v,method)=&c.kind else{unreachable!()};let a::Expression::Name(n)=&v.kind else{unreachable!()};
            // `result.len()` of a returned slice: its length (`SLICE_LENGTH` of the result).
            if n=="result"&&method=="len"&&self.ensures&&!self.scope.contains_key(n)&&let Some((path,ty))=(self.field)(RESULT,&["#len"],false){return Ok(Math::Field{root:RESULT,path,ty,before:false});}
            let root=*self.scope.get(n).ok_or_else(||error("function contracts clause name is not a parameter"))?;
            match (self.observe)(root,method){
                Some((function,integer))=>{if integer{self.observed.push(function);}Math::Observe{root,function,before:false}}
                // A slice's `len()`: its length, as the prover keys it.
                None if method=="len"&&let Some((path,ty))=(self.field)(root,&["#len"],false)=>Math::Field{root,path,ty,before:false},
                None=>return Err(error("function contracts clause calls a method the value's type does not have"))}},
        // A number read through a method of a field (`items.len()` in an invariant, which is
        // `queue.items.len()` once its fields are named from the value): the field the method's
        // one-line body returns (`clause_field`'s `#view` step with a number result), as for a
        // parameter's own method above.
        a::Expression::Call(c,args) if args.is_empty()&&matches!(&c.kind,a::Expression::Field(v,_) if matches!(&v.kind,a::Expression::Field(..)|a::Expression::Unary("*",_)))=>{
            let m=self.part(e)?;if matches!(&m,Math::Field{ty:super::logical_body::Ty::Bool,..}){return Err(error("function contracts compare numbers; a Bool is a proposition"));}m},
        a::Expression::Binary(op@("+"|"-"|"*"|"/"|"%"),a,b)=>{let(a,b)=(Box::new(self.math(a,depth+1)?),Box::new(self.math(b,depth+1)?));match *op{"+"=>Math::Add(a,b),"-"=>Math::Sub(a,b),"*"=>Math::Mul(a,b),"/"=>Math::Div(a,b),_=>Math::Mod(a,b)}},
        _ if scalar_place(e)=>{let m=self.part(e)?;if matches!(&m,Math::Field{ty:super::logical_body::Ty::Bool,..}){return Err(error("function contracts compare numbers; a Bool is a proposition"));}m},
        _=>return Err(error(format!("function contracts support scalar names, literals and arithmetic; this is {}",construct(e)))),
    })}
    /// A field of a named parameter or of the result, or `*name`.
    fn part(&self,e:&a::Expr)->Result<Math>{self.part_at(e).map_err(|refusal|placed(refusal,e))}
    fn part_at(&self,e:&a::Expr)->Result<Math>{
        // Fields by name, and array elements at a literal index, written `[k]` for the resolver.
        let mut steps:Vec<String>=Vec::new();let mut root=e;
        loop{match &root.kind{
            a::Expression::Call(c,args) => match (&c.kind,args.as_slice()) {
                (a::Expression::Field(ns,name),[sum,arm]) if name=="variant_value" && matches!(&ns.kind,a::Expression::Name(n) if n=="memory") => {
                    let a::Expression::Integer(k)=arm.kind else {return Err(error("variant_value requires a literal variant index"));};
                    steps.push(format!("#payload{k}"));root=sum;
                }
                (a::Expression::Field(base,name),[])=>{steps.push(format!("#view{name}"));root=base;}
                _=>break,
            },
            a::Expression::Field(base,name)=>{steps.push(name.clone());root=base;}
            a::Expression::Index(base,at) if matches!(at.kind,a::Expression::Integer(_))=>{let a::Expression::Integer(k)=at.kind else{unreachable!()};steps.push(format!("[{k}]"));root=base;}
            _=>break}}
        steps.reverse();let names:Vec<&str>=steps.iter().map(String::as_str).collect();
        let(name,starred)=match &root.kind{a::Expression::Name(n)=>(n,false),a::Expression::Unary("*",v)=>match &v.kind{a::Expression::Name(n)=>(n,true),_=>return Err(error("function contracts read fields of a named parameter"))},_=>return Err(error("function contracts read fields of a named parameter"))};
        let arm_step=self.arm.as_ref().filter(|(binder,_)|binder==name&&!starred).map(|(_,k)|format!("#arm{k}"));
        if let Some(step)=arm_step{
            let mut names=names.clone();names.insert(0,step.as_str());
            let(path,ty)=(self.field)(RESULT,&names,false).ok_or_else(||error("function contracts name a number or Bool: a field this file may read, or `*name` for a reference to one"))?;
            return Ok(Math::Field{root:RESULT,path,ty,before:false});
        }
        let root=match self.scope.get(name){Some(id)=>*id,
            None if name=="result"&&!starred&&self.ensures=>RESULT,
            None if name=="result"=>return Err(error("function contracts read a field of `result` only in ensures")),
            None=>return Err(error("function contracts clause name is not a parameter"))};
        let(path,ty)=(self.field)(root,&names,starred).ok_or_else(||error("function contracts name a number or Bool: a field this file may read, or `*name` for a reference to one"))?;
        Ok(Math::Field{root,path,ty,before:false})}
    /// A Bool field, now or (`old(...)`) on entry, read as a proposition.
    fn flag(&self,e:&a::Expr)->Option<Math>{match &e.kind{
        _ if scalar_place(e)=>self.part(e).ok().filter(|m|matches!(m,Math::Field{ty:super::logical_body::Ty::Bool,..})),
        a::Expression::Call(c,args) if self.old&&matches!(&c.kind,a::Expression::Name(n) if n=="old")&&args.len()==1=>match self.flag(&args[0])?{Math::Field{root,path,ty,before:false} if root!=RESULT=>Some(Math::Field{root,path,ty,before:true}),_=>None},
        _=>None}}
    /// Whether a value is an integer (`Some(true)`), a natural number, or a literal that is either.
    /// The two kinds do not meet, and an integer is not divided: `/` rounds toward zero at run time.
    fn kind(&self,m:&Math)->Result<Option<bool>>{Ok(match m{
        Math::Literal(_)=>None,Math::Int(_)=>Some(true),Math::Cast(integer,_)=>Some(*integer),
        Math::Binding(id)=>Some(self.integers.contains(id)),Math::Result=>Some(self.integer_result),
        Math::Field{ty,..}=>Some(*ty==super::logical_body::Ty::I64),Math::Observe{function,..}=>Some(self.observed.contains(function)),
        Math::Add(a,b)|Math::Sub(a,b)|Math::Mul(a,b)=>self.same(a,b)?,
        Math::Div(a,b)|Math::Mod(a,b)=>self.same(a,b)?,
    })}
    /// Whether every literal of a value that stands among integers is one an I64 can hold.
    fn fits(m:&Math)->bool{match m{Math::Literal(n)=>*n<=i64::MAX as u64,Math::Add(a,b)|Math::Sub(a,b)|Math::Mul(a,b)|Math::Div(a,b)|Math::Mod(a,b)=>Self::fits(a)&&Self::fits(b),_=>true}}
    fn same(&self,a:&Math,b:&Math)->Result<Option<bool>>{match(self.kind(a)?,self.kind(b)?){
        (Some(a),Some(b)) if a!=b=>Err(error("function contracts compare an I64 with an I64, and an unsigned number with an unsigned number")),
        (a,b)=>Ok(a.or(b))}}
    fn truth(&self,n:&str)->Option<Math>{if n=="result"{return (self.result&&self.bool_result).then_some(Math::Result);}self.scope.get(n).filter(|id|self.bools.contains(id)).map(|id|Math::Binding(*id))}
    fn prop_form(&self,e:&a::Expr)->bool{match &e.kind{a::Expression::Name(n)=>self.truth(n).is_some(),_ if self.flag(e).is_some()=>true,_=>matches!(&e.kind,a::Expression::Bool(_)|a::Expression::Unary("!",_)|a::Expression::Binary("=="|"!="|"<"|"<="|">"|">="|"&&"|"||",_,_))}}
    fn prop(&mut self,e:&a::Expr,depth:usize)->Result<Prop>{self.prop_at(e,depth).map_err(|refusal|placed(refusal,e))}
    fn prop_at(&mut self,e:&a::Expr,depth:usize)->Result<Prop>{self.count(depth)?;Ok(match &e.kind{
        a::Expression::Call(c,args) if matches!(&c.kind, a::Expression::Field(base,op)
            if op=="member" && matches!(&base.kind, a::Expression::Name(n) if n=="memory")) => {
            let [loan, selector]=args.as_slice() else { return Err(error("memory.member takes a shared loan and a recursive shared projection")); };
            let shared=|expr:&a::Expr| self.part(&a::Expr{kind:a::Expression::Field(Box::new(expr.clone()),"#shared_projection".into()),span:expr.span.clone()});
            let Math::Field{root:holder,path:mut carried,ty:held,..}=shared(loan)? else {return Err(error("expected a shared loan"));};
            let Math::Field{root,path:mut selected,ty,..}=shared(selector)? else {return Err(error("expected a shared projection"));};
            if held!=ty {return Err(error("the loan and member projection must designate the same element type"));}
            carried.push(super::scalar_vc::MEMBER_STAMP);
            carried.extend_from_slice(&selected);
            selected.push(super::scalar_vc::ROOT_STAMP);
            Prop::Compare("=", Math::Field{root:holder,path:carried,ty:super::logical_body::Ty::U64,before:false},
                Math::Field{root,path:selected,ty:super::logical_body::Ty::U64,before:false})
        }
        a::Expression::Bool(b)=>Prop::Bool(*b),a::Expression::Unary("!",v)=>Prop::Not(Box::new(self.prop(v,depth+1)?)),
        a::Expression::Name(n) if self.truth(n).is_some()=>Prop::Truth(self.truth(n).expect("Bool name")),
        _ if self.flag(e).is_some()=>Prop::Truth(self.flag(e).expect("Bool field")),
        a::Expression::Binary(op@("&&"|"||"),a,b)=>Prop::Binary(if *op=="&&"{"∧"}else{"∨"},Box::new(self.prop(a,depth+1)?),Box::new(self.prop(b,depth+1)?)),
        a::Expression::Binary(op@("=="|"!="),a,b) if self.prop_form(a)||self.prop_form(b)=>{
            let p=Prop::Binary("↔",Box::new(self.prop(a,depth+1)?),Box::new(self.prop(b,depth+1)?));if *op=="!="{Prop::Not(Box::new(p))}else{p}},
        a::Expression::Binary(op@("=="|"!="|"<"|"<="|">"|">="),a,b)=>{let(a,b)=(self.math(a,depth+1)?,self.math(b,depth+1)?);
            if self.same(&a,&b)?==Some(true)&&!(Self::fits(&a)&&Self::fits(&b)){return Err(error("function contracts integer literal exceeds the range of I64"));}
            Prop::Compare(match *op{"=="=>"=","!="=>"≠","<"=>"<","<="=>"≤",">"=>">",_=>"≥"},a,b)},
        _=>return Err(error(format!("function contracts require supported Bool propositions; this is {}",construct(e)))),
    })}
}
impl Resolve<'_>{
    /// `elements.kept(b)` and the like: what the function does to the cells of the buffer `b`,
    /// a field of a parameter. `None`: the clause is not one of these.
    fn element(&mut self,e:&a::Expr,arm:Option<u8>)->Result<Option<ElementClause>>{
        let a::Expression::Call(c,args)=&e.kind else{return Ok(None)};
        let a::Expression::Field(base,op)=&c.kind else{return Ok(None)};
        if !matches!(&base.kind,a::Expression::Name(n) if n=="elements"&&!self.scope.contains_key(n)){return Ok(None);}
        let Some((buffer,rest))=args.split_first() else{return Err(placed(error("an `elements.` clause names a buffer: a field of a parameter"),e))};
        let buffer=self.part(&a::Expr{kind:a::Expression::Field(Box::new(buffer.clone()),"#buffer".into()),span:buffer.span.clone()})
            .map_err(|_|placed(error("an `elements.` clause names a buffer: a field of a parameter that is a `Buffer`"),buffer))?;
        if !matches!(&buffer,Math::Field{root,..} if *root!=RESULT){return Err(placed(error("an `elements.` clause names a buffer of a parameter, not of the result"),e));}
        let kind=match (op.as_str(),rest){
            ("kept",[])=>ElementKind::Kept,
            ("kept_below",[n])=>ElementKind::KeptBelow(self.math(n,1)?),
            ("kept_except",[i])=>ElementKind::KeptExcept(self.math(i,1)?),
            ("among",[n])=>ElementKind::Among(self.math(n,1)?),
            ("holds",[i,value])=>{
                let a::Expression::Name(name)=&value.kind else{return Err(placed(error("`elements.holds` names the parameter whose value the cell holds"),value))};
                let id=*self.scope.get(name).ok_or_else(||placed(error("`elements.holds` names the parameter whose value the cell holds"),value))?;
                ElementKind::Holds(self.math(i,1)?,id)}
            _=>return Err(placed(error("an `elements.` clause is `kept(b)`, `kept_below(b, n)`, `kept_except(b, i)`, `holds(b, i, item)` or `among(b, n)`"),e))};
        Ok(Some(ElementClause{arm,buffer,kind,span:e.span.clone()}))}
}
/// Loop invariants of a scalar function: clauses over the locals in scope at the loop.
pub(super) fn resolve_loop(expressions:&[a::Expr],scope:&BTreeMap<String,u32>,bools:&BTreeSet<u32>,scalars:&BTreeSet<u32>,integers:&BTreeSet<u32>,observe:Observe,field:Fields)->Result<Vec<Prop>>{
    if expressions.len()>32{return Err(error("function contracts allow at most 32 invariants on a loop"));}
    let mut r=Resolve{scope,result:false,ensures:false,nodes:0,observed:Vec::new(),bools,bool_result:false,scalars,integers,integer_result:false,observe,field,old:true,arm:None};
    expressions.iter().map(|e|r.prop(e,0)).collect()
}
/// `result`: whether the function returns a number or Bool (`Some(true)` for Bool); any other result cannot be named.
pub(super) fn resolve(f:&a::Function,parameters:&BTreeMap<String,u32>,bools:&BTreeSet<u32>,scalars:&BTreeSet<u32>,integers:&BTreeSet<u32>,integer_result:bool,result:Option<bool>,variants:Option<&[String]>,observe:Observe,field:Fields)->Result<Contract>{
    if parameters.contains_key("result"){return Err(error("function contracts parameter cannot be named result"));}
    // Which of the two: no `ensures`, or past the limit (the message starts as it always did).
    if f.ensures.is_empty(){return Err(error("function contracts require ensures and at most 32 combined clauses/parameters: this contract has no `ensures`; a function that promises nothing about its result says `ensures true`"));}
    if f.requires.len()+f.ensures.len()>32||parameters.len()>32{return Err(error(format!("function contracts require ensures and at most 32 combined clauses/parameters: this one has {} clauses and {} parameters",f.requires.len()+f.ensures.len(),parameters.len())));}
    let mut r=Resolve{scope:parameters,result:false,ensures:false,nodes:0,observed:Vec::new(),bools,bool_result:result==Some(true),scalars,integers,integer_result,observe,field,old:false,arm:None};
    let requires=f.requires.iter().map(|e|Ok(Clause{value:r.prop(e,0)?,span:e.span.clone(),arm:None})).collect::<Result<_>>()?;
    r.result=result.is_some();r.old=true;r.ensures=true;
    let mut ensures=Vec::new();let mut effects=Vec::new();
    for e in &f.ensures{match &e.kind{
        a::PostconditionKind::Predicate(p)=>match r.element(p,None)?{Some(effect)=>effects.push(effect),None=>ensures.push(Clause{value:r.prop(p,0)?,span:p.span.clone(),arm:None})},
        // `match result:` over a Result: each arm's clauses hold when that variant is returned.
        // They speak of the parameters; the value the variant holds cannot be named.
        a::PostconditionKind::Match{value,arms}=>{
            let variants=variants.filter(|_|matches!(&value.kind,a::Expression::Name(n) if n=="result"))
                .ok_or_else(||error("function contracts match postcondition takes `result` of a function returning a sum"))?;
            for arm in arms{
                let index=variants.iter().position(|name| name==&arm.variant).and_then(|i|u8::try_from(i).ok())
                    .ok_or_else(||error("function contracts match postcondition names a variant of the returned sum"))?;
                if arm.binding.as_ref().is_some_and(|name|parameters.contains_key(name)){return Err(error("function contracts match postcondition binds a name that is a parameter"));}
                r.arm=arm.binding.clone().map(|name|(name,index));
                for clause in &arm.clauses{
                    let a::PostconditionKind::Predicate(p)=&clause.kind else{return Err(error("function contracts do not support nested match postconditions"));};
                    match r.element(p,Some(index))?{Some(effect)=>effects.push(effect),None=>ensures.push(Clause{value:r.prop(p,0)?,span:p.span.clone(),arm:Some(index)})}
                }
                r.arm=None;
            }
        }
    }}
    // Clauses inside arms count like any other.
    if f.requires.len()+ensures.len()>32{return Err(error("function contracts require ensures and at most 32 combined clauses/parameters"));}
    Ok(Contract{requires,ensures,assumed:vec![],effects,elements:vec![],nodes:r.nodes,storage:None,record:None,memory:None})
}
impl Math{
    /// Every name, `result` and method call replaced by what `leaf` gives for it.
    pub fn rewrite(&self,leaf:&mut dyn FnMut(&Math)->Result<Math>)->Result<Math>{
        let mut pair=|a:&Math,b:&Math|->Result<(Box<Math>,Box<Math>)>{Ok((Box::new(a.rewrite(leaf)?),Box::new(b.rewrite(leaf)?)))};
        Ok(match self{
            Self::Literal(n)=>return Ok(Self::Literal(*n)),Self::Int(n)=>return Ok(Self::Int(*n)),
            Self::Add(a,b)=>{let(a,b)=pair(a,b)?;Self::Add(a,b)},Self::Sub(a,b)=>{let(a,b)=pair(a,b)?;Self::Sub(a,b)},Self::Mul(a,b)=>{let(a,b)=pair(a,b)?;Self::Mul(a,b)},
            Self::Div(a,b)=>{let(a,b)=pair(a,b)?;Self::Div(a,b)},Self::Mod(a,b)=>{let(a,b)=pair(a,b)?;Self::Mod(a,b)},
            Self::Cast(to,inner)=>Self::Cast(*to,Box::new(inner.rewrite(leaf)?)),
            Self::Binding(_)|Self::Result|Self::Observe{..}|Self::Field{..}=>return leaf(self),
        })
    }
}
impl Prop{
    pub fn rewrite(&self,leaf:&mut dyn FnMut(&Math)->Result<Math>)->Result<Prop>{Ok(match self{
        Self::Bool(b)=>Self::Bool(*b),Self::Truth(m)=>Self::Truth(m.rewrite(leaf)?),Self::Compare(op,a,b)=>Self::Compare(op,a.rewrite(leaf)?,b.rewrite(leaf)?),
        Self::Not(v)=>Self::Not(Box::new(v.rewrite(leaf)?)),Self::Binary(op,a,b)=>Self::Binary(op,Box::new(a.rewrite(leaf)?),Box::new(b.rewrite(leaf)?)),
    })}
    /// Every method call a clause makes.
    pub fn observes(&self,out:&mut Vec<Math>){let _=self.rewrite(&mut |m|{if matches!(m,Math::Observe{..}|Math::Field{..}){out.push(m.clone());}Ok(m.clone())});}
}
impl Math{
    pub fn variables(&self,out:&mut Vec<u32>){match self{Self::Binding(id)=>out.push(*id),Self::Cast(_,inner)=>inner.variables(out),Self::Add(a,b)|Self::Sub(a,b)|Self::Mul(a,b)|Self::Div(a,b)|Self::Mod(a,b)=>{a.variables(out);b.variables(out);},_=>{}}}
}
impl Prop{
    pub fn variables(&self,out:&mut Vec<u32>){match self{Self::Truth(m)=>m.variables(out),Self::Compare(_,a,b)=>{a.variables(out);b.variables(out);},Self::Not(v)=>v.variables(out),Self::Binary(_,a,b)=>{a.variables(out);b.variables(out);},_=>{}}}
}
