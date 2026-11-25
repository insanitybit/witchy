//! One logical allocation, independent of its physical layout and concrete codec.
use super::logical_body::Ty;
#[derive(Clone,Debug)]
pub(super) struct Focus {pub element:Ty,pub key:String,pub capacity:String,pub cells:String,pub provenance:&'static str,pub known:bool,pub live:String,
    /// Set when a verified callee consumed this owner and returned another one.
    /// `live` is then false for accounting, but the allocation was released only
    /// if the callee said so (the named proposition) or it is the successor's
    /// allocation and that one was released.
    pub moved:Option<(String,u32)>}
