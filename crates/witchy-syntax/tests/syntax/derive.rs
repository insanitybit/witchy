    use witchy_syntax::derive::*;
    #[allow(unused_imports)]
    use witchy_syntax::ast::*;

    #[test]
    fn borrowed_nominal_deserialize_derive_requires_an_owned_companion() {
        let mut module = witchy_syntax::parser::parse_reference_migration(
            "mode opt\n\nimport json\n\ntype Holder('a) derive(Deserialize):\n    view: &'a String\n",
        )
        .expect("parse borrowed Deserialize derive");
        let error = expand(&mut module)
            .expect_err("deserialization must not invent a borrowed owner relation");

        assert!(error.contains("derive(Deserialize)"), "{error}");
        assert!(error.contains("authenticated owner relation"), "{error}");
        assert!(error.contains("owned companion type"), "{error}");
    }
