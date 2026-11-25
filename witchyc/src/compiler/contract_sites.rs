//! Primary typed call inventory. IDs were assigned once by source checking.
use super::{logical_body::*,logical_place::{Place,Projection}};
use std::collections::BTreeMap;
struct Walk<'a>{functions:&'a[Function],sites:BTreeMap<u32,FunctionId>}
impl Walk<'_>{
        fn place(&mut self,p:&Place){for p in &p.projections{if let Projection::Index{value,..}=p{self.value(value);}}}
        fn value(&mut self,e:&Expr){match &e.kind{
            Value::Call(c)=>{for a in &c.args{self.value(a);}if self.functions[c.definition.0 as usize].contract.as_ref().is_some_and(|c|c.record.is_some()){self.sites.insert(c.contract_site.expect("primary contracted site"),c.definition);}},
            Value::ReadPlace(p)|Value::Address(p)|Value::Move{place:p,..}|Value::Extract{place:p,..}=>self.place(p),
            Value::Stored(p,v)=>{self.value(v);self.place(p)},
            Value::TryAlloc{allocator,capacity,..}=>{self.value(allocator);self.value(capacity);},
            Value::Storage{receiver,index,value,..}=>{self.value(receiver);self.value(index);if let Some(v)=value{self.value(v);}},
            Value::Binary(_,a,b)|Value::Index(a,b,_)|Value::SliceAt(a,b,_)|Value::SliceIndex(a,b,_)|Value::SliceCopy(a,b)|Value::SliceReadLe64(a,b)=>{self.value(a);self.value(b);},
            Value::SliceOf(p,_)=>self.place(p),Value::SliceLen(v)=>self.value(v),
            Value::SlicePart(a,b,c)|Value::SliceWithin(a,b,c)=>{self.value(a);self.value(b);self.value(c);},
            Value::AllocationKey(v,_)|Value::Capacity(v,_)|Value::TryRelease(v,_)|Value::Discard(v)|Value::Sum{payload:v,..}|Value::Repeat(v,_)|Value::Retag(v,_)|Value::Project(v,_)|Value::Widen(v)|Value::ProofUtf8(v)|Value::Not(v)=>self.value(v),
            Value::Aggregate(fs)|Value::Signed(_,fs)|Value::Float(_,fs) | Value::Bits(_, fs) | Value::BoxOp(_, _, fs)=>for v in fs{self.value(v);},Value::Print(p,v,_)=>{self.place(p);self.value(v);},Value::Read(p,_)=>self.place(p),Value::Host(_,p,vs)=>{if let Some(p)=p{self.place(p);}for v in vs{self.value(v);}},Value::Exchange(a,b)=>{self.place(a);self.place(b);}
            Value::TaskSpawn{allocator,bytes,entry}=>{self.value(allocator);self.value(bytes);self.value(entry);}
            Value::TaskStep(v,w)=>{self.value(v);self.value(w);}
            Value::TaskFinish(v)|Value::TaskStatus(v)|Value::Pause(v)=>self.value(v),
            Value::StaticStr(_) | Value::Unit|Value::U64(_)|Value::Bool(_)|Value::EntryConsole|Value::EntryInput|Value::EntryAllocator|Value::TaskWord|Value::InTask|Value::EntryHost(..)=>{},
        }}
        fn body(&mut self,body:&[Stmt]){for s in body{match s{
            Stmt::Set(_,e)|Stmt::Eval(e)|Stmt::Return(e, _, _)=>self.value(e),
            Stmt::StorePlace(p,_,e)|Stmt::Replace(_,p,e)=>{self.place(p);self.value(e);},
            Stmt::If(e,a,b)=>{self.value(e);self.body(a);self.body(b);},Stmt::While(e,b,_)=>{self.value(e);self.body(b);},
            Stmt::Match{value,arms}=>{self.value(value);for a in arms{self.body(&a.body);}},Stmt::Expire(_)|Stmt::Break|Stmt::Trap(_)|Stmt::Continue=>{},
        }}}
    }
pub(super) fn record_sites(body:&[Stmt],functions:&[Function])->BTreeMap<u32,FunctionId> {
    let mut w=Walk{functions,sites:BTreeMap::new()};w.body(body);w.sites
}
pub(super) fn has_record(e:&Expr,functions:&[Function])->bool {
    let mut w=Walk{functions,sites:BTreeMap::new()};w.value(e);!w.sites.is_empty()
}
