use witchy_syntax::ast::{Expr, Item, Module, Stmt};
use crate::source::TaskAwaitProof;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind { ScalarScope, PacketScope, SelectDrain, ForwardedPacketScope }

impl RegionKind {
    fn companion(self) -> &'static str {
        match self {
            Self::ScalarScope => "__witchy_closed_scalar_root",
            Self::PacketScope | Self::ForwardedPacketScope => "__witchy_closed_packet_root",
            Self::SelectDrain => "__witchy_closed_select_root",
        }
    }
    fn inventory(self, awaits: usize, receive: bool) -> bool {
        match self {
            Self::ScalarScope | Self::PacketScope => awaits == 3 && receive,
            Self::ForwardedPacketScope => awaits == 4 && receive,
            Self::SelectDrain => matches!(awaits, 5 | 6) && !receive,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarRegionReceipt {
    kind: RegionKind,
    root: u32,
    companion: u32,
    source_graph_digest: [u8; 32],
    companion_digest: [u8; 32],
    root_digest: [u8; 32],
    awaits: Vec<u32>,
    await_coordinates: Vec<(String, u32, Option<u32>, u32)>,
    receive_control: Option<(String, u32, Option<u32>, u32, u32, u32)>,
}

impl ScalarRegionReceipt {
    #[doc(hidden)]
    pub fn from_checked_frontend(kind: RegionKind, root: u32, companion: u32, source_graph_digest: [u8; 32],
        companion_digest: [u8; 32], root_digest: [u8; 32], awaits: Vec<u32>,
        await_coordinates: Vec<(String, u32, Option<u32>, u32)>,
        receive_control: Option<(String, u32, Option<u32>, u32, u32, u32)>) -> Self {
        Self { kind, root, companion, source_graph_digest, companion_digest, root_digest, awaits, await_coordinates, receive_control }
    }
    pub fn kind(&self) -> RegionKind { self.kind }
    pub fn root_item(&self) -> u32 { self.root }
    pub fn companion_item(&self) -> u32 { self.companion }
    pub fn covered_awaits(&self) -> &[u32] { &self.awaits }
    pub fn source_graph_digest(&self) -> &[u8; 32] { &self.source_graph_digest }
    pub fn validate_checked_source(&self, module: &Module, proofs: &[TaskAwaitProof], witness: &crate::source::CheckedSourceWitness) -> bool {
        self.digest() == witness.scalar_region_manifest_digest() && self.validate(module, proofs)
    }
    pub fn validate(&self, module: &Module, proofs: &[TaskAwaitProof]) -> bool {
        let Some(Item::Function(root)) = module.items.get(self.root as usize) else { return false };
        let Some(Item::Function(companion)) = module.items.get(self.companion as usize) else { return false };
        if root.name != "main" || companion.name != self.kind.companion()
            || blake3::hash(format!("{companion:?}").as_bytes()).as_bytes() != &self.companion_digest
            || blake3::hash(format!("{root:?}").as_bytes()).as_bytes() != &self.root_digest
            || !self.kind.inventory(self.awaits.len(), self.receive_control.is_some())
            || self.await_coordinates.len() != self.awaits.len()
            || self.source_graph_digest == [0; 32] { return false; }
        let [Stmt::Expr(Expr::Call { name, args })] = root.body.stmts.as_slice() else { return false };
        if name != &companion.name || args.len() != root.params.len() + 1 { return false; }
        if args.iter().skip(1).zip(&root.params).any(|(argument, parameter)|
            !matches!(argument, Expr::Var(name) if name == &parameter.name)) { return false; }
        let mut covered = std::collections::BTreeSet::new();
        for (index, coordinate) in self.awaits.iter().zip(&self.await_coordinates) {
            let Some(proof) = proofs.get(*index as usize) else { return false };
            if !covered.insert(*index) || proof.source_identity() != (coordinate.0.as_str(), coordinate.1, coordinate.2, coordinate.3) {
                return false;
            }
        }
        true
    }
    pub fn digest(&self) -> [u8; 32] {
        fn number(bytes: &mut Vec<u8>, value: u32) { bytes.extend_from_slice(&value.to_le_bytes()); }
        fn text(bytes: &mut Vec<u8>, value: &str) {
            bytes.extend_from_slice(&(value.len() as u64).to_le_bytes()); bytes.extend_from_slice(value.as_bytes());
        }
        fn method(bytes: &mut Vec<u8>, value: Option<u32>) {
            bytes.push(u8::from(value.is_some())); if let Some(value) = value { number(bytes, value); }
        }
        let mut bytes = b"witchy-closed-async-region-v2".to_vec();
        bytes.push(match self.kind { RegionKind::ScalarScope => 0, RegionKind::PacketScope => 1, RegionKind::SelectDrain => 2, RegionKind::ForwardedPacketScope => 3 });
        number(&mut bytes, self.root); number(&mut bytes, self.companion);
        bytes.extend_from_slice(&self.source_graph_digest); bytes.extend_from_slice(&self.companion_digest);
        bytes.extend_from_slice(&self.root_digest);
        bytes.extend_from_slice(&(self.awaits.len() as u64).to_le_bytes());
        for (index, coordinate) in self.awaits.iter().zip(&self.await_coordinates) {
            number(&mut bytes, *index); text(&mut bytes, &coordinate.0); number(&mut bytes, coordinate.1);
            method(&mut bytes, coordinate.2); number(&mut bytes, coordinate.3);
        }
        bytes.push(u8::from(self.receive_control.is_some()));
        if let Some((module, item, source_method, control, selector, endpoint)) = &self.receive_control {
            text(&mut bytes, module); number(&mut bytes, *item); method(&mut bytes, *source_method);
            number(&mut bytes, *control); number(&mut bytes, *selector); number(&mut bytes, *endpoint);
        }
        *blake3::hash(&bytes).as_bytes()
    }
}

pub fn manifest_digest(receipt: Option<&ScalarRegionReceipt>) -> [u8; 32] {
    receipt.map_or([0; 32], ScalarRegionReceipt::digest)
}
