//! Logical values retain record ordinals and the sole existing allocation focus.
#[derive(Clone,Debug)]
pub(super) enum Symbol {Unit,Nat(String),Prop(String),Element(String),Stored(String),Buffer(u32),Permission,SliceLength(String),Unknown,Record(Vec<Self>),Sum(u8,Box<Self>)}
impl Symbol {
    pub fn text(&self)->Option<&str>{match self{Self::Nat(s)|Self::Prop(s)|Self::Element(s)=>Some(s),_=>None}}
    pub fn field(&self,fields:&[usize])->Option<&Self>{let mut v=self;for i in fields{let Self::Record(fs)=v else{return None;};v=fs.get(*i)?;}Some(v)}
    pub fn store(&mut self,fields:&[usize],value:Self)->Option<()> {if let Some((i,rest))=fields.split_first(){let Self::Record(fs)=self else{return None;};fs.get_mut(*i)?.store(rest,value)}else{*self=value;Some(())}}
}
