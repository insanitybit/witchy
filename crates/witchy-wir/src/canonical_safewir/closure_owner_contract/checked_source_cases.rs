//! Closure-contract cases that judge a real checked source (RFC-0170 P4).
//! They moved out of `closure_owner_contract::tests` so witchy-wir's tests
//! need no checker; `crates/witchy-lower/tests/safewir_checked_source.rs`
//! runs each one under the same name. See
//! `canonical_safewir::checked_source_test_api`.

use super::*;
use crate::canonical_safewir::checked_source_test_api::SourceChecker;

pub fn opaque_closure_contract_keeps_caps_and_managed_captures_typed(checker: &dyn SourceChecker) {
    checker.with_checked("main", b"fn logging(console: Console, prefix: String):\n    let log = fn(msg: String): console.print(prefix + msg)\n    log(\"ok\")\nfn main(console: Console):\n    logging(console, \"log: \")\n", &mut |checked| {
        let contract = SourceConstructor::from_checked(&checked.view, ConstructorId(0)).unwrap();
        assert!(std::ptr::eq(contract.source().module(), checked.typed_module));
        assert!(std::ptr::eq(contract.source().closure_witness(), checked.closure_witness));
        assert!(std::ptr::eq(contract.witness(), &checked.closure_witness.entries()[0]));
        assert_eq!(contract.transfers().len(), 2);
        assert!(contract.transfers().contains(&CaptureTransfer::CloneManaged));
        for (index, transfer) in contract.transfers().iter().enumerate() {
            let read = contract.capability_read(CaptureOrdinal(index as u32));
            match transfer {
                CaptureTransfer::CopyCapability(kind) => {
                    assert_eq!(*kind, CapabilityKindId::new(0));
                    assert_eq!(read.unwrap(), BridgeAbi {
                        parameters: vec![BridgeValue::OpaqueOwner],
                        results: vec![BridgeValue::Capability(*kind)],
                    });
                }
                _ => assert_eq!(read, Err(ContractError::NotCapabilityCapture)),
            }
        }
        assert_eq!(contract.capability_read(CaptureOrdinal(u32::MAX)), Err(ContractError::NotCapabilityCapture));
        assert!(matches!(SourceConstructor::from_checked(&checked.view, ConstructorId(u32::MAX)), Err(ContractError::MissingWitness)));
        assert_eq!(BridgeAbi::seal().parameters, vec![BridgeValue::TypedGcPayload]);
        assert_eq!(BridgeAbi::release().results, vec![BridgeValue::FinalPayload]);
        assert_eq!(BridgeAbi::retain().parameters, vec![BridgeValue::OpaqueOwner]);
    });
}
