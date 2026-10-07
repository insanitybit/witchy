//! Structural identities of callables and proper-tail sites in the checked
//! canonical AST, and the manifest digest shared by the checked frontend
//! witness and the SafeWIR history. The checker derives the sites; this
//! crate only names them.

/// Stable identity of one callable in the canonical checked AST.  The item and
/// method coordinates are structural addresses in the final module; nested
/// closures use the structural path of their `Lambda` expression.  No callable
/// spelling is retained because trait and generic lowering may replace names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProperTailCallableId {
    pub item: u32,
    pub method: Option<u32>,
    pub lambda_path: Vec<ProperTailPathStep>,
}

/// The operation kind is part of the key so direct and closure/table dispatch
/// cannot alias even when their enclosing AST paths happen to agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProperTailDispatchKind {
    Direct,
    Indirect,
}

/// Canonical structural path components.  `Child` is a field-local child
/// ordinal, not a preorder counter; `Node` carries the AST variant so paths
/// through different expression shapes cannot collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProperTailPathStep {
    Statement(u32),
    Value,
    ReturnValue,
    YieldValue,
    Node(ProperTailExprKind),
    Child(u32),
    Arm(u32),
    Guard,
    /// The forwarding call of the adapter that makes a named function a
    /// value. It has no source expression: the callable is the target
    /// function, and the adapter discharges the site.
    Adapter,
}

/// Stable tags for every current expression variant.  The explicit repr is a
/// canonical encoding contract, not an identity derived from enum traversal.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProperTailExprKind {
    Int,
    Float,
    Duration,
    Str,
    Bool,
    List,
    Tuple,
    Var,
    Call,
    LabeledCall,
    LabeledMethodCall,
    MethodCall,
    Apply,
    Ctor,
    AnonCtor,
    Unary,
    Field,
    Lambda,
    RecordUpdate,
    Record,
    Try,
    As,
    ExistentialPack,
    ExistentialUpcast,
    ExistentialCall,
    Binary,
    If,
    Match,
    Block,
    While,
    For,
    Range,
    Index,
    WhileLet,
    TaggedLit,
}

/// Canonical serialization shared by the checked frontend witness and the
/// SafeWIR history. Callers provide stable site identities and canonical
/// function ordinals, never source item positions.
pub fn canonical_proper_tail_manifest_digest(sites: &[([u8; 32], u32)]) -> [u8; 32] {
    let mut bytes = b"witchy.proper-tail-manifest.v1".to_vec();
    put_u32(
        &mut bytes,
        u32::try_from(sites.len()).expect("validated proper-tail manifest site count"),
    );
    for (site_id, canonical_function) in sites {
        bytes.extend_from_slice(site_id);
        put_u32(&mut bytes, *canonical_function);
    }
    *blake3::hash(&bytes).as_bytes()
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}
