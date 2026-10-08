use crate::pipeline::CheckedModule;
use crate::runtime_type::RuntimeDeclarationCatalog;
use crate::typeck::TypedModule;
use crate::scalar_region_preparation::BoundReceipt;

#[derive(Debug)]
pub(crate) enum Preparation {
    Scalar(crate::scalar_region_preparation::Preparation),
}

impl Preparation {
    pub(crate) fn bind(&self, checked: &CheckedModule, typed: &TypedModule,
        proofs: &[witchy_canonical_source::source::TaskAwaitProof], catalog: &RuntimeDeclarationCatalog)
        -> Result<BoundReceipt, String> {
        match self { Self::Scalar(preparation) => preparation.bind(checked, typed, proofs, catalog) }
    }
}

pub(crate) fn prepare(checked: &CheckedModule, typed: TypedModule, catalog: &RuntimeDeclarationCatalog)
    -> Result<(TypedModule, Option<Preparation>), String> {
    let (typed, preparation) = crate::scalar_region_preparation::prepare(checked, typed, catalog)?;
    Ok((typed, preparation.map(Preparation::Scalar)))
}
