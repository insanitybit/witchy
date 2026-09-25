    use witchy_syntax::type_resolve::*;
    #[allow(unused_imports)]
    use witchy_syntax::ast::*;
    use witchy_syntax::linker::LinkError;
    use witchy_syntax::parser::{parse_module, parse_reference_migration};

    /// Run the per-module type/constructor resolution over a set of `(name, src)`
    /// modules — the linker's `type_resolve::resolve` step in isolation.
    fn resolve_src(mods: &[(&str, &str)]) -> Result<(), LinkError> {
        resolved_src(mods).map(|_| ())
    }

    fn resolved_src(mods: &[(&str, &str)]) -> Result<Vec<(String, Module)>, LinkError> {
        let mut modules: Vec<(String, Module)> = mods
            .iter()
            .map(|(n, s)| (n.to_string(), parse_module(s).expect("parse")))
            .collect();
        resolve(&mut modules)?;
        Ok(modules)
    }

    #[test]
    fn normal_modules_cannot_import_or_name_reference_bearing_opt_declarations() {
        let api = "mode opt\n\n\
                   type View('a):\n    value: &'a String\n\n\
                   trait Inspect:\n    fn inspect(let self, text: &'a String) -> &'a String\n";

        let mut modules = vec![
            ("api".into(), parse_reference_migration(api).expect("parse migration api")),
            ("normal".into(), parse_module("from api import View\n\nfn main():\n    ()\n").expect("parse normal")),
        ];
        let err = resolve(&mut modules)
        .expect_err("normal source must not import a reference-bearing opt type");
        assert!(err.message.contains("non-rooted-reference opt type `api.View`"), "{}", err.message);

        let mut modules = vec![
            ("api".into(), parse_reference_migration(api).expect("parse migration api")),
            ("normal".into(), parse_module("import api\n\nfn use(value: api.View) -> ():\n    ()\n").expect("parse normal")),
        ];
        let err = resolve(&mut modules)
        .expect_err("normal source must not name a reference-bearing opt type");
        assert!(err.message.contains("non-rooted-reference opt type `api.View`"), "{}", err.message);

        let mut modules = vec![
            ("api".into(), parse_reference_migration(api).expect("parse migration api")),
            ("normal".into(), parse_module("from api import Inspect\n\nfn main():\n    ()\n").expect("parse normal")),
        ];
        let err = resolve(&mut modules)
        .expect_err("normal source must not import a reference-bearing opt trait");
        assert!(err.message.contains("non-rooted-reference opt trait `api.Inspect`"), "{}", err.message);

        let mut modules = vec![
            ("api".into(), parse_reference_migration(api).expect("parse migration api")),
            ("normal".into(), parse_module("import api\n\nfn use(value: dyn Inspect) -> ():\n    ()\n").expect("parse normal")),
        ];
        let err = resolve(&mut modules)
        .expect_err("plain imports must not put a reference-bearing opt trait in normal scope");
        assert!(err.message.contains("non-rooted-reference opt trait `api.Inspect`"), "{}", err.message);

        resolve_src(&[
            ("api", "mode opt\n\ntype Token:\n    value: Int\n"),
            (
                "normal",
                "from api import Token\n\nfn use(value: Token) -> ():\n    ()\n",
            ),
        ])
        .expect("normal modules retain conventional opt type interfaces");
    }

    #[test]
    fn from_import_private_fn_is_not_an_export() {
        // Module privacy is part of the language surface: a private helper may be
        // called inside its module, but cannot be imported unqualified elsewhere.
        let err = resolve_src(&[
            ("lib", "fn hidden(n: Int) -> Int:\n    n + 1\n\npub fn shown(n: Int) -> Int:\n    hidden(n)\n"),
            ("main", "from lib import hidden\n\nfn main(console: Console):\n    console.print(\"x\")\n"),
        ])
        .unwrap_err();
        assert!(
            err.message
                .contains("module `lib` exports no type or function named `hidden`"),
            "{}",
            err.message
        );
    }

    const CMP_STUB: &str =
        "type Ordering:\n    Less\n    Equal\n    Greater\n\npub fn reverse(o: Ordering) -> Ordering:\n    o\n";

    #[test]
    fn user_type_shadowing_ambient_name_is_rejected() {
        // BUG-289: a user module declaring `type Secret`/`type Set`/… is a loud
        // error (its constructors would be unreachable), while the canonical std
        // declarer (`cmp`) may still declare `type Ordering`.
        for ambient in [
            "Secret", "Set", "Iter", "Ordering", "NetPolicy", "DirPolicy", "Option", "Result",
        ] {
            let src = format!("type {ambient}:\n    Wrapped(Int)\n");
            let err = resolve_src(&[("main", &src)]).unwrap_err();
            assert!(
                err.message.contains(&format!("type `{ambient}` shadows the ambient built-in name")),
                "{ambient}: {}",
                err.message
            );
        }
        // A std module keeps declaring its own ambient type.
        resolve_src(&[("cmp", CMP_STUB)]).expect("cmp may declare Ordering");
        // Being some other std module does not grant ownership of every ambient
        // declaration. Only the canonical owner above is exempt.
        let err = resolve_src(&[("json", "type Secret:\n    Secret(Int)\n")]).unwrap_err();
        assert!(err.message.contains("type `Secret` shadows the ambient built-in name"));

        // Provenance, not the filename alone, identifies a canonical std owner.
        // A local `policy.witchy` must not gain authority to mint policy values.
        let mut local_policy = vec![(
            "policy".to_string(),
            parse_module("type NetPolicy:\n    pattern: String\n").expect("parse"),
        )];
        let user_modules = std::collections::HashSet::from(["policy".to_string()]);
        let err = resolve_with_user_modules(&mut local_policy, &user_modules).unwrap_err();
        assert!(err.message.contains("type `NetPolicy` shadows the ambient built-in name"));
    }

    #[test]
    fn inherent_impls_require_definition_site_type_ownership() {
        let err = resolve_src(&[(
            "malicious",
            "impl Net:\n    pub fn audit(self: Net) -> String:\n        \"stolen\"\n",
        )])
        .unwrap_err();
        assert!(
            err.message.contains("cannot define inherent methods on non-owned type `Net`"),
            "{}",
            err.message
        );

        let err = resolve_src(&[
            ("model", "type Token:\n    Token(Int)\n"),
            (
                "malicious",
                "import model\n\nimpl model.Token:\n    pub fn audit(self: model.Token) -> String:\n        \"stolen\"\n",
            ),
        ])
        .unwrap_err();
        assert!(
            err.message
                .contains("cannot define inherent methods on non-owned type `model.Token`"),
            "{}",
            err.message
        );

        resolve_src(&[(
            "model",
            "type Token:\n    Token(Int)\n\nimpl Token:\n    pub fn value(self: Token) -> Int:\n        1\n",
        )])
        .expect("a module may define inherent methods on its own nominal type");

        resolve_src(&[(
            "policy",
            "impl Net:\n    pub fn marker(self: Net) -> String:\n        \"toolchain\"\n",
        )])
        .expect("the canonical bundled owner may define Net policy builders");

        let mut local_policy = vec![(
            "policy".to_string(),
            parse_module("impl Net:\n    pub fn audit(self: Net) -> String:\n        \"stolen\"\n")
                .expect("parse"),
        )];
        let user_modules = std::collections::HashSet::from(["policy".to_string()]);
        let err = resolve_with_user_modules(&mut local_policy, &user_modules).unwrap_err();
        assert!(err.message.contains("cannot define inherent methods on non-owned type `Net`"));
    }

