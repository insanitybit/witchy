//! One typed resolution pass for named specifications, independent of proof models.
use super::{logical_body::{FunctionId, Pointee, Ty}, semantic_types::Types,
    spec_expr::{Binder, Expr, Kind}};
use crate::{ast as a, diagnostic::Diagnostic};
use std::collections::{BTreeMap, BTreeSet};
type Result<T> = std::result::Result<T, Diagnostic>;
pub(super) type Scope = BTreeMap<String, (Binder, Ty)>;
pub(super) fn error(span:std::ops::Range<usize>,message: impl Into<std::borrow::Cow<'static, str>>)->Diagnostic {
    Diagnostic::new("W0401",span,message)
}
#[derive(Clone)]
pub(super) struct Definition {
    pub module:u32, pub public:bool, pub owner:FunctionId, pub binders:Vec<Binder>, pub parameters:Vec<Ty>,
    pub generics:usize, pub source:a::Expr, pub scope:Scope,
}
pub(super) struct Catalog<'a> {
    pub names:BTreeMap<String,usize>, pub definitions:Vec<Definition>,
    pub bodies:Vec<Option<Expr>>, pub types:Types, pub views:BTreeMap<u32,std::ops::Range<usize>>,
    state:Vec<u8>, nodes:usize,
    methods:BTreeMap<&'a str,(FunctionId,&'a a::Function)>, pub module:u32, next_binder:u32,
}
pub(super) fn view(ty:Ty)->Ty {if let Ty::Borrow{pointee,..}=ty{pointee.ty()}else{ty}}
impl<'a> Catalog<'a> {
    pub fn new(program:&'a a::Program)->Result<Self>{
        let types=Types::new(program)?;
        let methods = program.declarations.iter().filter_map(|d| match d {
            a::Declaration::Function(f) => Some(f), _ => None,
        }).enumerate().filter(|(_,f)|f.name.contains('.')).map(|(id,f)|(f.name.as_str(),(FunctionId(id as u32), f))).collect();
        let mut out=Self{views:BTreeMap::new(),methods,module:0,next_binder:0,names:BTreeMap::new(),definitions:vec![],bodies:vec![],types,state:vec![],nodes:0};
        let mut all=BTreeSet::new();
        for d in &program.declarations {
            let name=match d {a::Declaration::Predicate{name,..}|a::Declaration::Record{name,..}|a::Declaration::Alias{name,..}|a::Declaration::Sum{name,..}=>Some(name),
                a::Declaration::Function(f)=>Some(&f.name),a::Declaration::Lemma(l)=>Some(&l.name),_=>None};
            if let Some(n)=name {if !all.insert(n.clone()){
                // Predicates share one namespace with every file's functions and types (a library's
                // `pub predicate` included), so say which name it is: the span is unknown here.
                let shown=n.split('@').next().unwrap_or(n);
                return Err(error(0..0,format!("duplicate selected declaration name: `{shown}` is declared more than once among this program's functions, types, lemmas and predicates, counting the files it imports; give one of them another name")));
            }}
        }
        for (declaration,d) in program.declarations.iter().enumerate(){
            let a::Declaration::Predicate{module,public,name,generics,parameters,body}=d else{continue;};
            if matches!(name.as_str(),"memory"|"old"|"result"|"main")||!out.types.parameter_name_available(name){return Err(error(body.span.clone(),"reserved predicate name"));}
            if generics.len()>8||parameters.len()>32{return Err(error(body.span.clone(),"predicate parameter limit exceeded"));}
            let mut seen=BTreeSet::new();
            for g in generics {if !out.types.parameter_name_available(g)||all.contains(g)||!seen.insert(g){return Err(error(body.span.clone(),"duplicate or conflicting predicate type parameter"));}}
            let owner=FunctionId(0x8000_0000+declaration as u32);
            let mut scope=Scope::new();let mut binders=vec![];let mut params=vec![];
            for (index,(n,t)) in parameters.iter().enumerate(){
                if generics.contains(n)||matches!(n.as_str(),"memory"|"old")||scope.contains_key(n){return Err(error(body.span.clone(),"duplicate or conflicting predicate parameter"));}
                let ty=out.source_type(t,owner,generics)?;
                if matches!(ty,Ty::Unit|Ty::Console|Ty::Input|Ty::Allocator){return Err(error(body.span.clone(),"unsupported predicate parameter type"));}
                let binder=Binder{declaration:declaration as u32,parameter:index as u32};scope.insert(n.clone(),(binder,ty));binders.push(binder);params.push(ty);
            }
            out.names.insert(name.clone(),out.definitions.len());
            out.definitions.push(Definition{module:*module,public:*public,owner,binders,parameters:params,generics:generics.len(),source:body.clone(),scope});
            out.bodies.push(None);out.state.push(0);
        }
        for id in 0..out.definitions.len(){out.resolve_definition(id)?;}
        Ok(out)
    }
    pub fn source_type(&mut self,t:&a::Type,owner:FunctionId,generics:&[String])->Result<Ty>{
        if let a::Type::Borrow{mutable,inner}=t{
            let inner=self.types.ty_in(inner,owner,generics)?;
            Ok(Ty::Borrow{mutable:*mutable,pointee:Pointee::from_ty(inner).ok_or_else(||error(0..0,"unsupported predicate borrowed type"))?})
        }else{self.types.ty_in(t,owner,generics)}
    }
    fn resolve_definition(&mut self,id:usize)->Result<()> {
        if self.state[id]==2{return Ok(());}
        if self.state[id]==1{return Err(error(self.definitions[id].source.span.clone(),"recursive specification predicates are unsupported"));}
        if self.state.iter().filter(|s|**s==1).count()>=64{return Err(error(self.definitions[id].source.span.clone(),"predicate definition dependency depth limit"));}
        self.state[id]=1;let d=self.definitions[id].clone();
        let caller_module=std::mem::replace(&mut self.module,d.module);
        let body=self.expression(&d.source,&d.scope,true,0)?;
        self.module=caller_module;
        if body.ty!=Ty::Bool{return Err(error(d.source.span,"predicate body must be Bool"));}
        self.bodies[id]=Some(body);self.state[id]=2;Ok(())
    }
    pub fn expression(&mut self,e:&a::Expr,scope:&Scope,definition:bool,depth:usize)->Result<Expr>{
        self.nodes+=1;if self.nodes>4096||depth>64{return Err(error(e.span.clone(),"specification resolution complexity limit"));}
        let fail=||error(e.span.clone(),"unsupported or ill-typed specification expression");
        let (kind,ty)=match &e.kind {
            a::Expression::Name(n)=>{let(b,t)=scope.get(n).ok_or_else(||error(e.span.clone(),"unbound specification name"))?;(Kind::Bound(*b),*t)},
            a::Expression::Integer(n)=>(Kind::Integer(*n),Ty::U64),a::Expression::Bool(b)=>(Kind::Bool(*b),Ty::Bool),
            a::Expression::Unary(op,v)=>{
                let v=self.expression(v,scope,definition,depth+1)?;
                let ty=match *op{"!" if v.ty==Ty::Bool=>Ty::Bool,"*" if matches!(v.ty,Ty::Borrow{..})=>view(v.ty),
                    "&"|"&mut"=>Ty::Borrow{mutable:*op=="&mut",pointee:Pointee::from_ty(view(v.ty)).ok_or_else(fail)?},_=>return Err(fail())};
                (Kind::Unary(*op,Box::new(v)),ty)
            },
            a::Expression::Binary(op,left,right)=>{
                let a=self.expression(left,scope,definition,depth+1)?;let b=self.expression(right,scope,definition,depth+1)?;
                let number = match (a.ty,b.ty) {
                    (Ty::Unsigned(x),Ty::Unsigned(y)) if x==y => Some(a.ty),
                    (Ty::Unsigned(x),Ty::Unsigned(_)) if matches!(b.kind,Kind::Integer(n) if n<=x.layout().maximum())=>Some(a.ty),
                    (Ty::Unsigned(_),Ty::Unsigned(y)) if matches!(a.kind,Kind::Integer(n) if n<=y.layout().maximum())=>Some(b.ty),
                    _=>None,
                };
                let ty=match *op{"&&"|"||" if a.ty==Ty::Bool&&b.ty==Ty::Bool=>Ty::Bool,
                    "+" if a.ty==Ty::U64&&b.ty==Ty::U64=>Ty::U64,
                    "<"|"<="|">"|">=" if number.is_some()=>Ty::Bool,
                    "=="|"!=" if number.is_some() || a.ty==b.ty&&matches!(a.ty,Ty::Bool|Ty::Param{..})=>Ty::Bool,_=>return Err(fail())};
                (Kind::Binary(*op,Box::new(a),Box::new(b)),ty)
            },
            a::Expression::SpecCase{value,arms,guarded} => return self.case(e,value,arms,*guarded,scope,definition,depth),
            a::Expression::Field(base,name)=>{
                let base=self.expression(base,scope,definition,depth+1)?;
                if self.types.sealed_in(view(base.ty)).is_some_and(|owner|owner!=self.module){return Err(error(e.span.clone(),"specification cannot read a sealed field from another module"));}
                let ty=self.types.get(view(base.ty)).and_then(|r|r.fields.iter().find(|f|f.name==*name)).map(|f|f.ty).ok_or_else(fail)?;
                let home=definition.then(||self.types.sealed_in(view(base.ty))).flatten();
                (Kind::Field(Box::new(base),name.clone(),home),ty)
            },
            a::Expression::Call(c,args)=>{
                if let a::Expression::Name(name)=&c.kind {
                    if scope.contains_key(name){return Err(error(e.span.clone(),"a bound specification value is not callable"));}
                    if let Some(id)=self.names.get(name).copied(){
                        self.resolve_definition(id)?;let d=self.definitions[id].clone();
                        if !d.public && d.module!=self.module{return Err(error(e.span.clone(),"predicate is private to its defining module"));}
                        if args.len()!=d.parameters.len(){return Err(error(e.span.clone(),"predicate argument count mismatch"));}
                        let args=args.iter().map(|a|self.expression(a,scope,definition,depth+1)).collect::<Result<Vec<_>>>()?;
                        let mut substitutions=vec![None;d.generics];
                        for(pattern,actual)in d.parameters.iter().zip(&args){self.types.unify(*pattern,actual.ty,d.owner,&mut substitutions).map_err(|_|error(e.span.clone(),"predicate argument type mismatch"))?;}
                        if substitutions.iter().any(Option::is_none){return Err(error(e.span.clone(),"cannot infer predicate type arguments"));}
                        (Kind::Apply(id,args),Ty::Bool)
                    }else if name=="variant_index" && args.len()==1 {
                        let value=self.expression(&args[0],scope,definition,depth+1)?;
                        self.types.sum(view(value.ty)).ok_or_else(fail)?;
                        (Kind::Builtin(name.clone(),vec![value]),Ty::U64)
                    }else if name=="old"&&args.len()==1{
                        let v=self.expression(&args[0],scope,definition,depth+1)?;
                        if !path(&v)||old(&v){return Err(error(e.span.clone(),"old requires an unnested specification path"));}
                        let ty=v.ty;(Kind::Builtin("old".into(),vec![v]),ty)
                    }else{return Err(error(e.span.clone(),"unknown or effectful specification call"));}
                }else if let a::Expression::Field(namespace,name)=&c.kind{
                    if !matches!(&namespace.kind,a::Expression::Name(n) if n=="memory"){
                        if !args.is_empty(){return Err(fail());}
                        let recv=self.expression(namespace,scope,definition,depth+1)?;
                        let base=view(recv.ty);
                        let owner=self.types.nominal(base).ok_or_else(fail)?;
                        let (id,f)=self.methods.get(super::methods::name(owner,name).as_str()).copied().ok_or_else(fail)?;
                        if !(f.public || f.module==self.module) || f.suspends || f.required {return Err(fail());}
                        let [(_,a::Type::Borrow{mutable:false,inner})]=f.parameters.as_slice() else{return Err(fail());};
                        let receiver=self.source_type(inner,id,&f.generics)?;
                        let mut args=vec![None;f.generics.len()];
                        self.types.unify(receiver,base,id,&mut args)?;
                        let args=args.into_iter().collect::<Option<Vec<_>>>().ok_or_else(fail)?;
                        let result=self.source_type(&f.result,id,&f.generics)?;
                        if !matches!(result,Ty::Weak(_)|Ty::Borrow{mutable:false,..}){return Err(fail());}
                        let ty=self.types.substitute(result,id,&args)?;
                        if definition { self.views.entry(id.0).or_insert_with(||e.span.clone()); }
                        return Ok(Expr{kind:Kind::Method(Box::new(recv),name.clone(),(definition && !f.public).then_some(f.module)),ty,span:e.span.clone()});
                    }
                    if definition&&name=="frame_unchanged"{return Err(error(e.span.clone(),"frame_unchanged is not supported in named predicates"));}
                    let args=args.iter().map(|a|self.expression(a,scope,definition,depth+1)).collect::<Result<Vec<_>>>()?;
                    let ty=self.memory_type(name,&args).ok_or_else(fail)?;(Kind::Builtin(name.clone(),args),ty)
                }else{return Err(fail());}
            },
            _=>return Err(error(e.span.clone(),"unsupported specification form; no effects, quantifiers or runtime values")),
        };
        Ok(Expr{kind,ty,span:e.span.clone()})
    }
    fn fresh(&mut self) -> Binder {
        self.next_binder += 1;
        Binder { declaration: u32::MAX, parameter: self.next_binder }
    }

    fn case(
        &mut self,
        e: &a::Expr,
        value: &a::Expr,
        arms: &[(String, Option<String>, a::Expr)],
        guarded: bool,
        scope: &Scope,
        definition: bool,
        depth: usize,
    ) -> Result<Expr> {
        let at = |kind, ty| Expr { kind, ty, span: e.span.clone() };
        let boolean = |op, a, b| at(Kind::Binary(op, Box::new(a), Box::new(b)), Ty::Bool);
        let subject = self.expression(value, scope, definition, depth + 1)?;
        let sum = self
            .types
            .sum(view(subject.ty))
            .ok_or_else(|| error(value.span.clone(), "a specification pattern must match a sum"))?
            .clone();
        let id = self.fresh();
        let root = at(Kind::Bound(id), subject.ty);
        let mut seen = BTreeSet::new();
        let mut clauses = None;
        for (variant, binding, clause) in arms {
            let tag = sum
                .names
                .iter()
                .position(|n| n == variant)
                .ok_or_else(|| error(clause.span.clone(), "pattern does not belong to this sum"))?;
            if !seen.insert(tag) {
                return Err(error(clause.span.clone(), "duplicate specification match arm"));
            }
            let ordinal = at(Kind::Integer(tag as u64), Ty::U64);
            let mut local = scope.clone();
            let binding = if let Some(name) = binding {
                if sum.arms[tag] == Ty::Unit || local.contains_key(name) {
                    return Err(error(clause.span.clone(), "pattern needs a payload and a fresh binder"));
                }
                let payload_id = self.fresh();
                local.insert(name.clone(), (payload_id, sum.arms[tag]));
                Some(payload_id)
            } else {
                None
            };
            let mut body = self.expression(clause, &local, definition, depth + 1)?;
            if body.ty != Ty::Bool {
                return Err(error(clause.span.clone(), "a specification arm must be Bool"));
            }
            if !guarded && matches!(body.kind, Kind::Bool(false)) {
                continue;
            }
            if let Some(b) = binding {
                let payload = at(
                    Kind::Builtin("variant_value".into(), vec![root.clone(), ordinal.clone()]),
                    sum.arms[tag],
                );
                body = at(Kind::Let(b, Box::new(payload), Box::new(body)), Ty::Bool);
            }
            let index = at(Kind::Builtin("variant_index".into(), vec![root.clone()]), Ty::U64);
            let test = boolean(if guarded { "!=" } else { "==" }, index, ordinal);
            let arm = if !guarded && matches!(body.kind, Kind::Bool(true)) {
                test
            } else {
                boolean(if guarded { "||" } else { "&&" }, test, body)
            };
            clauses = Some(if let Some(prev) = clauses { boolean("||", prev, arm) } else { arm });
        }
        if !guarded && seen.len() != sum.arms.len() {
            return Err(error(e.span.clone(), "specification match must cover every variant"));
        }
        let body = clauses.unwrap_or_else(|| at(Kind::Bool(false), Ty::Bool));
        Ok(at(Kind::Let(id, Box::new(subject), Box::new(body)), Ty::Bool))
    }

    fn memory_type(&self,name:&str,args:&[Expr])->Option<Ty>{
        let buffer=|v:&Expr|if let Ty::Buffer(i)=view(v.ty){Some(self.types.buffers()[i as usize].element)}else{None};
        let valid=match(name,args){
            ("pointer_bits"|"box_id",[a]) if matches!(view(a.ty),Ty::Weak(_)|Ty::Boxed(_)|Ty::Buffer(_))=>
                return Some(if name=="pointer_bits"{Ty::Unsigned(super::scalar::Scalar::U32)}else{Ty::U64}),
            ("member",[a,b])=>matches!(a.ty,Ty::Borrow{mutable:false,..}) && a.ty==b.ty,
            ("variant_value",[s,k])=>{let Kind::Integer(i)=k.kind else{return None};return self.types.sum(view(s.ty))?.arms.get(usize::try_from(i).ok()?).copied()},
            ("capacity"|"allocation_key",[a])=>{buffer(a)?;return Some(Ty::U64)},
            ("initialized"|"empty",[a,i])=>buffer(a).is_some()&&i.ty==Ty::U64,
            ("cell",[a,i,v])=>buffer(a)==Some(v.ty)&&i.ty==Ty::U64,
            ("same_allocation"|"unchanged",[a,b])=>buffer(a).is_some()&&buffer(a)==buffer(b),
            ("unchanged_except"|"unchanged_from"|"same_prefix"|"equal_prefix",[a,b,i])=>buffer(a).is_some()&&buffer(a)==buffer(b)&&i.ty==Ty::U64,
            ("empty_from",[a,i])=>buffer(a).is_some()&&i.ty==Ty::U64,
            ("empty_range"|"initialized_range"|"prefix",[a,i,j])=>buffer(a).is_some()&&i.ty==Ty::U64&&j.ty==Ty::U64,
            ("empty_all"|"released",[a])=>buffer(a).is_some(),_=>false,
        };valid.then_some(Ty::Bool)
    }
}
fn path(e:&Expr)->bool{match &e.kind{Kind::Bound(_)=>true,Kind::Method(v,_,_)|Kind::Field(v,_,_)|Kind::Unary("&"|"&mut"|"*",v)=>path(v),Kind::Builtin(n,a)if matches!(n.as_str(),"old"|"variant_index"|"pointer_bits"|"box_id"|"variant_value")=>a.first().is_some_and(path),_=>false}}
fn old(e:&Expr)->bool{match &e.kind{Kind::Builtin(n,_)if n=="old"=>true,Kind::Method(v,_,_)|Kind::Field(v,_,_)|Kind::Unary(_,v)=>old(v),Kind::Builtin(_,args)=>args.iter().any(old),_=>false}}
