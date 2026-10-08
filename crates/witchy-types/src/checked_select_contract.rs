use witchy_syntax::ast::{Item, Module};
use crate::runtime_type::{DeclarationKind, PackageSource, RuntimeDeclarationCatalog};

pub(crate) fn contract(runtime: &Module, original: &Module, catalog: &RuntimeDeclarationCatalog) -> Result<(), String> {
    crate::checked_scalar_contract::scheduler_contract(runtime, original, catalog)?;
    for &(name, runtime_hash, original_hash) in RECIPES {
        let kind = if name == "chan.Selected" { DeclarationKind::Type } else { DeclarationKind::Function };
        let identity = catalog.resolve(name, kind).ok_or_else(|| format!("missing select declaration {name}"))?;
        let (namespace, declaration) = name.split_once('.').ok_or("invalid select declaration")?;
        if identity.package().source() != &PackageSource::Toolchain || identity.package().name() != "witchy/std"
            || identity.module() != ["std", namespace] || identity.name() != declaration {
            return Err(format!("unauthenticated select declaration {name}"));
        }
        for (module, expected) in [(runtime, runtime_hash), (original, original_hash)] {
            let hashes = module.items.iter().filter_map(|item| match item {
                Item::Function(function) if kind == DeclarationKind::Function && function.name == name => Some(crate::checked_scalar_contract::function_hash(function)),
                Item::Type(definition) if kind == DeclarationKind::Type && definition.name == name => Some(crate::checked_scalar_contract::type_hash(definition)), _ => None,
            }).collect::<Vec<_>>();
            if hashes.as_slice() != [expected] { return Err(format!("changed or absent select recipe {name}")); }
        }
    }
    Ok(())
}

// Frozen contracts bind original decoding and scheduler selection to this source lowering.
const RECIPES: &[(&str, &str, &str)] = &[
    ("chan.Selected", "d2c79f6ace541edbc5f98aba5803cfaa46a9c739217613ae0045947c0df27b1e", "d2c79f6ace541edbc5f98aba5803cfaa46a9c739217613ae0045947c0df27b1e"),
    ("chan.select", "7358362ff663dcb0c06bef6da436dd8ff57090092b66fa3f91e51fce0d51e69c", "7358362ff663dcb0c06bef6da436dd8ff57090092b66fa3f91e51fce0d51e69c"),
    ("chan.select2_result", "ffac34ce10200c96bcfd2067afc0407dca456d4a8818242e031b9a7854ee8344", "ffac34ce10200c96bcfd2067afc0407dca456d4a8818242e031b9a7854ee8344"),
    ("chan.select2_frame_decode", "89892afaf5d99feb0da93b8ff251d2456fc0c2d9cb256f4cb903cc00350f66a4", "89892afaf5d99feb0da93b8ff251d2456fc0c2d9cb256f4cb903cc00350f66a4"),
    ("chan.__select2_lane_frame", "7fd1e8ed03cef6db23c7387637c5325f7cc8600950bf1156e316586b6f3196d0", "7fd1e8ed03cef6db23c7387637c5325f7cc8600950bf1156e316586b6f3196d0"),
    ("chan.__rfc0152_selected_materialized", "a9fa0b01561ba6b0c074c721dcfa30677c2640a42c18962855c34508d25dd6f0", "a9fa0b01561ba6b0c074c721dcfa30677c2640a42c18962855c34508d25dd6f0"),
    ("task.__channel_select2", "6913df1845b094fea6114a08d793ee8446ff695c582a5314238118095e303218", "6913df1845b094fea6114a08d793ee8446ff695c582a5314238118095e303218"),
    ("task.__prepare_select", "41fe2956a2f967ee6daad12ec6e475f58b86e32f02f900e071d557b6701bfd11", "41fe2956a2f967ee6daad12ec6e475f58b86e32f02f900e071d557b6701bfd11"),
    ("task.__select_lane_frame", "692bb5d4525acc2fcb71638ceefcbedb6dcdf3b3957bce895fc5eb4b0ef00755", "692bb5d4525acc2fcb71638ceefcbedb6dcdf3b3957bce895fc5eb4b0ef00755"),
];
