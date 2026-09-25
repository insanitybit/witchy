    use witchy_syntax::cap_ops::*;
    #[allow(unused_imports)]
    use witchy_syntax::ast::*;

    #[test]
    fn every_catalog_receiver_has_one_nominal_type_identity() {
        for operation in OPS {
            let matches: Vec<_> = RECEIVER_TYPES
                .iter()
                .filter(|(receiver, _)| receiver == &operation.receiver)
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "{:?} must have exactly one receiver type identity",
                operation.receiver
            );
            assert_eq!(
                ReceiverKind::from_type_name(matches[0].1),
                Some(operation.receiver)
            );
        }
    }
