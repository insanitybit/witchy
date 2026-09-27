//! Source-bound mutation controls for copyable opaque environment projections.
use witchy_types::pipeline::CanonicalCheckedSource;
use witchy_wir::canonical_safewir::{Operation, check_initial_with_witness};
use witchy_wir::canonical_safewir::closure_owner_contract::{
    CaptureOrdinal, CaptureTransfer, OwnerOperation as O,
};

pub fn projection_mutations_refuse(source: &CanonicalCheckedSource) {
    let layouts = crate::canonical_layouts::derive_layout_bundle(source).expect("checked layouts");
    let candidate = crate::canonical_lower::lower_typed_module(source, layouts).expect("checked lowering")
        .with_launch_contract_payload(source.launch_contract_payload().to_vec());
    check_initial_with_witness(candidate.clone(), &source.view()).expect("original field reads admitted");
    let (constructor, affine_slot) = candidate.owner_families().iter()
        .flat_map(|family| &family.constructors)
        .find_map(|constructor| constructor.captures.iter().position(|capture|
            capture.transfer == CaptureTransfer::MoveManaged).map(|slot| (constructor.id, slot)))
        .expect("fixture has an affine sibling");
    let wrong_constructor = candidate.owner_families().iter()
        .flat_map(|family| &family.constructors).find(|entry| entry.id != constructor)
        .expect("fixture has a different authenticated constructor").id;
    for mutation in 0..3 {
        let mut changed = false;
        let altered = candidate.clone().transform_functions(|functions| {
            for function in functions {
                for operation in function.blocks.iter_mut().flat_map(|block| &mut block.operations) {
                    let Operation::ClosureOwner(O::ReadEnvironmentField {
                        constructor: target, capture, ..
                    }) = operation else { continue; };
                    if *target != constructor || changed { continue; }
                    match mutation {
                        0 => *target = wrong_constructor,
                        1 => *capture = CaptureOrdinal(u32::MAX),
                        2 => *capture = CaptureOrdinal(affine_slot as u32),
                        _ => unreachable!(),
                    }
                    changed = true;
                }
            }
        });
        assert!(changed, "mutation must exercise a real projection");
        assert!(check_initial_with_witness(altered, &source.view()).is_err(),
            "wrong constructor, missing capture or affine child must not acquire read authority");
    }
}
