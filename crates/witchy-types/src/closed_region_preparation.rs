use crate::pipeline::CheckedModule;
use crate::runtime_type::RuntimeDeclarationCatalog;
use crate::typeck::TypedModule;
use crate::scalar_region_preparation::BoundReceipt;

#[derive(Debug)]
pub(crate) enum Preparation {
    Scalar(crate::scalar_region_preparation::Preparation),
    Packet(crate::packet_region_preparation::Preparation),
    Select(crate::select_region_preparation::Preparation),
}

impl Preparation {
    pub(crate) fn bind(&self, checked: &CheckedModule, typed: &TypedModule,
        proofs: &[witchy_canonical_source::source::TaskAwaitProof], catalog: &RuntimeDeclarationCatalog)
        -> Result<BoundReceipt, String> {
        match self {
            Self::Scalar(preparation) => preparation.bind(checked, typed, proofs, catalog),
            Self::Packet(preparation) => preparation.bind(checked, typed, proofs, catalog),
            Self::Select(preparation) => preparation.bind(checked, typed, proofs, catalog),
        }
    }
}

pub(crate) fn prepare(checked: &CheckedModule, typed: TypedModule, catalog: &RuntimeDeclarationCatalog)
    -> Result<(TypedModule, Option<Preparation>), String> {
    let (typed, selected) = crate::select_region_preparation::prepare(checked, typed, catalog)?;
    if let Some(selected) = selected { return Ok((typed, Some(Preparation::Select(selected)))); }
    let (typed, preparation) = crate::scalar_region_preparation::prepare(checked, typed, catalog)?;
    if let Some(preparation) = preparation { return Ok((typed, Some(Preparation::Scalar(preparation)))); }
    let (typed, preparation) = crate::packet_region_preparation::prepare(checked, typed, catalog)?;
    Ok((typed, preparation.map(Preparation::Packet)))
}
