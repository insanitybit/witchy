    use witchy_syntax::intrinsics::*;
    #[allow(unused_imports)]
    use witchy_syntax::ast::*;
    use std::collections::BTreeSet;

    #[test]
    fn catalog_names_and_ids_are_unique() {
        let mut names = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for spec in ALL {
            assert!(names.insert(spec.name), "duplicate intrinsic name {}", spec.name);
            assert!(ids.insert(format!("{:?}", spec.id)), "duplicate intrinsic id {:?}", spec.id);
            assert!(!spec.diagnostic_name.is_empty());
        }
    }

    #[test]
    fn dynamic_dispatch_and_reflection_rows_are_not_classified_as_pure() {
        for name in [
            DYNAMIC_RUNTIME_TYPE,
            DYNAMIC_DESCRIPTOR,
            DYNAMIC_DESCRIPTOR_ID,
            DYNAMIC_FIELDS,
            DYNAMIC_FIELD_STATUS,
            DYNAMIC_METHODS,
            DYNAMIC_CALL,
            DYNAMIC_CALL_WITH,
            DYNAMIC_IMPLEMENTS,
            DYNAMIC_AS_TRAIT,
            DYNAMIC_TRY_DECODE,
            DYNAMIC_TRY_DECODE_TYPED,
        ] {
            assert_eq!(
                lookup(name).expect("dynamic intrinsic row").effect,
                IntrinsicEffect::Dynamic,
                "{name}"
            );
        }
    }

