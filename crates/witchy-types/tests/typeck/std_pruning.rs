//! Generated-artifact consistency: the std pruning root census. The gate and
//! scripts/test-for-paths.sh route it by the name
//! `std_pruning::tests::pruning_roots_cover_every_compiler_std_literal`.

mod tests {
    /// Every std name the compiler spells must be a pruning root; regenerate
    /// with scripts/gen-std-pruning-roots.py when this fails.
    #[test]
    fn pruning_roots_cover_every_compiler_std_literal() {
        let root = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR for tests"),
        )
        .join("../..");
        let modules = witchy_syntax::linker::STD_MODULES;
        let mut found = std::collections::BTreeSet::new();
        let mut stack: Vec<std::path::PathBuf> = [
            "crates/witchy-types", "crates/witchy-lower", "crates/witchy-syntax",
            "crates/witchy-wir", "crates/witchy-interp", "crates/witchy-runtime", "src",
        ]
        .iter()
        .map(|crate_dir| root.join(crate_dir))
        .collect();
        while let Some(path) = stack.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                    stack.push(entry.path());
                }
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs")
                || path.file_name().is_some_and(|name| name == "std_pruning_roots.rs" || name == "std_pruning.rs")
            {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            for literal in text.split('"').skip(1).step_by(1) {
                let Some((module, rest)) = literal.split_once('.') else { continue };
                if modules.contains(&module)
                    && !rest.is_empty()
                    && rest.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    found.insert(format!("{module}.{rest}"));
                }
            }
        }
        let listed: std::collections::BTreeSet<String> =
            witchy_types::std_pruning_roots::COMPILER_STD_NAMES.iter().map(|name| name.to_string()).collect();
        let missing: Vec<_> = found.difference(&listed).collect();
        assert!(missing.is_empty(), "run scripts/gen-std-pruning-roots.py; missing roots: {missing:?}");
    }
}
