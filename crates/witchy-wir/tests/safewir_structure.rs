use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use syn::parse::Parser as _;
use syn::punctuated::Punctuated;
use syn::visit::Visit;
use syn::{
    Attribute, Expr, ExprLit, ExprMethodCall, ExprStruct, ItemFn, ItemMod, Lit, Macro,
    Member, Meta, Token,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RawKind {
    Load,
    Load8U,
    Store,
    Store8,
    MemoryCopy,
    MemoryFill,
    HeapGlobalWrite,
    RawBody,
}

impl RawKind {
    fn from_variant(name: &str) -> Option<Self> {
        match name {
            "Load" => Some(Self::Load),
            "Load8U" => Some(Self::Load8U),
            "Store" => Some(Self::Store),
            "Store8" => Some(Self::Store8),
            "MemoryCopy" => Some(Self::MemoryCopy),
            "MemoryFill" => Some(Self::MemoryFill),
            _ => None,
        }
    }
}

#[derive(Default)]
struct FileCensus {
    counts: BTreeMap<RawKind, usize>,
}

impl FileCensus {
    fn record(&mut self, kind: RawKind) {
        *self.counts.entry(kind).or_default() += 1;
    }

    fn total(&self) -> usize {
        self.counts.values().sum()
    }
}

struct ConstructorVisitor<'a> {
    census: &'a mut FileCensus,
}

impl Visit<'_> for ConstructorVisitor<'_> {
    fn visit_item_mod(&mut self, item: &ItemMod) {
        if cfg_test(&item.attrs) {
            return;
        }
        syn::visit::visit_item_mod(self, item);
    }

    fn visit_item_fn(&mut self, item: &ItemFn) {
        if cfg_test(&item.attrs) {
            return;
        }
        syn::visit::visit_item_fn(self, item);
    }

    fn visit_expr_struct(&mut self, expression: &ExprStruct) {
        if expression
            .path
            .segments
            .iter()
            .any(|segment| segment.ident == "Instruction")
        {
            // The mechanical encoder is the eventual sealed template boundary.
            // `wasm_encoder::Instruction::MemoryCopy` is not a candidate-WIR
            // constructor and must not be confused with `WirNode::MemoryCopy`.
            syn::visit::visit_expr_struct(self, expression);
            return;
        }
        let variant = expression.path.segments.last().map(|segment| segment.ident.to_string());
        if let Some(kind) = raw_wir_constructor_kind(expression, variant.as_deref()) {
            self.census.record(kind);
        }
        if variant.as_deref() == Some("SetGlobal")
            && expression.fields.iter().any(|field| {
                member_is(&field.member, "global") && expression_contains_heap_literal(&field.expr)
            })
        {
            self.census.record(RawKind::HeapGlobalWrite);
        }
        if variant.as_deref() == Some("WirFunc")
            && expression.fields.iter().any(|field| {
                member_is(&field.member, "raw_body") && !expression_is_none(&field.expr)
            })
        {
            self.census.record(RawKind::RawBody);
        }
        syn::visit::visit_expr_struct(self, expression);
    }

    fn visit_macro(&mut self, invocation: &Macro) {
        // syn deliberately treats a macro invocation as opaque tokens. Most
        // production WIR constructors live inside vec![...] bodies, so parse
        // those expression lists explicitly or the census misses the majority
        // of the surface it is intended to freeze.
        let name = invocation.path.segments.last().map(|segment| segment.ident.to_string());
        if matches!(name.as_deref(), Some("vec") | Some("smallvec")) {
            let parser = Punctuated::<Expr, Token![,]>::parse_terminated;
            match parser.parse2(invocation.tokens.clone()) {
                Ok(expressions) => {
                    for expression in expressions {
                        self.visit_expr(&expression);
                    }
                }
                Err(error) if tokens_may_construct_raw_wir(invocation) => {
                    panic!("cannot inspect raw-WIR macro body `{name:?}`: {error}")
                }
                Err(_) => {}
            }
        } else if name.as_deref() != Some("matches") && tokens_may_construct_raw_wir(invocation) {
            panic!("unreviewed macro may construct raw WIR: `{name:?}`");
        }
    }
}

fn raw_wir_constructor_kind(
    expression: &ExprStruct,
    variant: Option<&str>,
) -> Option<RawKind> {
    let kind = variant.and_then(RawKind::from_variant)?;
    // Match the raw WIR representation by its structural address field rather
    // than by the enum path. Production code deliberately aliases WirExpr and
    // WirNode, while declarative SafeWIR operations also use the names Load and
    // Store but carry a typed `place` instead of a raw pointer.
    let address_field = match kind {
        RawKind::Load | RawKind::Load8U | RawKind::Store | RawKind::Store8 => "ptr",
        RawKind::MemoryCopy | RawKind::MemoryFill => "dest",
        RawKind::HeapGlobalWrite | RawKind::RawBody => return None,
    };
    expression
        .fields
        .iter()
        .any(|field| member_is(&field.member, address_field))
        .then_some(kind)
}

fn tokens_may_construct_raw_wir(invocation: &Macro) -> bool {
    let tokens = invocation.tokens.to_string();
    [
        "Load {",
        "Load8U {",
        "Store {",
        "Store8 {",
        "MemoryCopy {",
        "MemoryFill {",
        "SetGlobal {",
        "raw_body : Some",
    ]
    .iter()
    .any(|marker| tokens.contains(marker))
}

fn member_is(member: &Member, expected: &str) -> bool {
    match member {
        Member::Named(name) => name == expected,
        Member::Unnamed(_) => false,
    }
}

fn cfg_test(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        if !attribute.path().is_ident("cfg") {
            return false;
        }
        match &attribute.meta {
            Meta::List(list) => {
                let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
                parser
                    .parse2(list.tokens.clone())
                    .is_ok_and(|nested| nested.len() == 1 && cfg_requires_test(&nested[0]))
            }
            Meta::Path(_) | Meta::NameValue(_) => false,
        }
    })
}

fn cfg_requires_test(meta: &Meta) -> bool {
    match meta {
        Meta::Path(path) => path.is_ident("test"),
        Meta::List(list) if list.path.is_ident("all") || list.path.is_ident("any") => {
            let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
            let Ok(nested) = parser.parse2(list.tokens.clone()) else {
                return false;
            };
            if list.path.is_ident("all") {
                nested.iter().any(cfg_requires_test)
            } else {
                !nested.is_empty() && nested.iter().all(cfg_requires_test)
            }
        }
        // Negated or unknown predicates are conservatively production-visible.
        // A false positive keeps the ratchet strict; a false negative bypasses it.
        Meta::List(_) | Meta::NameValue(_) => false,
    }
}

fn expression_is_none(expression: &Expr) -> bool {
    matches!(expression, Expr::Path(path) if path.path.segments.last().is_some_and(|segment| segment.ident == "None"))
}

fn expression_contains_heap_literal(expression: &Expr) -> bool {
    struct HeapLiteral(bool);
    impl Visit<'_> for HeapLiteral {
        fn visit_expr_lit(&mut self, expression: &ExprLit) {
            if matches!(&expression.lit, Lit::Str(value) if value.value() == "heap" || value.value() == "$heap") {
                self.0 = true;
            }
        }

        fn visit_expr_method_call(&mut self, expression: &ExprMethodCall) {
            syn::visit::visit_expr_method_call(self, expression);
        }
    }
    let mut found = HeapLiteral(false);
    found.visit_expr(expression);
    found.0
}

fn rust_files(root: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).unwrap_or_else(|error| panic!("read {}: {error}", root.display())) {
        let path = entry.expect("read source entry").path();
        if path.is_dir() {
            rust_files(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}

fn is_test_source_path(path: &Path) -> bool {
    path.components().any(|component| {
        let component = component.as_os_str().to_string_lossy();
        component == "tests"
            || component.ends_with("_tests")
            || component.ends_with("_test.rs")
            || component.ends_with("_tests.rs")
    })
}

fn current_census() -> BTreeMap<String, FileCensus> {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = crate_root.parent().and_then(Path::parent).expect("workspace root");
    let mut files = Vec::new();
    for entry in fs::read_dir(workspace.join("crates")).expect("read workspace crates") {
        let source = entry.expect("read crate entry").path().join("src");
        if source.is_dir() {
            rust_files(&source, &mut files);
        }
    }
    files.sort();

    let mut result = BTreeMap::new();
    for path in files {
        let relative = path.strip_prefix(workspace).expect("workspace source");
        if is_test_source_path(relative) {
            continue;
        }
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let syntax = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        let mut census = FileCensus::default();
        ConstructorVisitor { census: &mut census }.visit_file(&syntax);
        if census.total() != 0 {
            result.insert(relative.display().to_string(), census);
        }
    }
    result
}

/// 471 migration sites remaining, plus the 6 sealed-boundary constructors in
/// `wir_encode.rs` (see the baseline entry below). RFC-0162 row 8 drives the
/// migration sites to zero; the 6 are the permitted floor, not debt.
///
/// This ceiling ratchets DOWN as the migration proceeds. It started at 584+6.
const REVIEWED_TOTAL: usize = 413;

const REVIEWED_BASELINE: &[(&str, &[(RawKind, usize)])] = &[
    (
        "crates/witchy-lower/src/codegen/assembly.rs",
        &[
            (RawKind::Load, 16),
            (RawKind::Store, 5),
            (RawKind::Store8, 5),
            (RawKind::MemoryCopy, 2),
            (RawKind::HeapGlobalWrite, 7),
        ],
    ),
    (
        "crates/witchy-lower/src/codegen/block_lower.rs",
        &[
            (RawKind::Load, 6),
            (RawKind::Store, 11),
            (RawKind::Store8, 1),
        ],
    ),
    (
        "crates/witchy-lower/src/codegen/builtins.rs",
        &[
            (RawKind::Load, 15),
            (RawKind::Store, 3),
        ],
    ),
    (
        "crates/witchy-lower/src/codegen/expr_lower.rs",
        &[
            (RawKind::Load, 15),
            (RawKind::Store, 2),
            (RawKind::MemoryCopy, 1),
            (RawKind::HeapGlobalWrite, 2),
        ],
    ),
    (
        "crates/witchy-lower/src/codegen/helpers.rs",
        &[
            (RawKind::Load, 16),
            (RawKind::Load8U, 3),
            (RawKind::Store, 10),
            (RawKind::MemoryCopy, 1),
        ],
    ),
    (
        "crates/witchy-lower/src/codegen/indexed_kernels.rs",
        &[
            (RawKind::Load, 1),
        ],
    ),
    (
        "crates/witchy-lower/src/codegen/linear_builder.rs",
        &[
            (RawKind::Load, 3),
            (RawKind::Load8U, 3),
            (RawKind::Store8, 3),
        ],
    ),
    (
        "crates/witchy-lower/src/codegen/match_lower.rs",
        &[
            (RawKind::Load, 7),
        ],
    ),
    (
        "crates/witchy-lower/src/codegen/mod.rs",
        &[
            (RawKind::Load, 9),
            (RawKind::Load8U, 4),
            (RawKind::Store, 21),
            (RawKind::Store8, 9),
            (RawKind::MemoryCopy, 3),
            (RawKind::HeapGlobalWrite, 1),
        ],
    ),
    // SEALED BOUNDARY -- not migration debt. `erase_node`/`erase_expr`
    // mint the plain wasm operation a validated SafeWIR operation always
    // denoted; one constructor per kind, so this still blocks growth.
    (
        "crates/witchy-wir/src/wir_encode.rs",
        &[
            (RawKind::Load, 2),
            (RawKind::Load8U, 2),
            (RawKind::Store, 2),
            (RawKind::Store8, 2),
            (RawKind::MemoryCopy, 2),
            (RawKind::MemoryFill, 2),

        ],
    ),
    (
        "crates/witchy-wir/src/wir_helpers/bytes.rs",
        &[
            (RawKind::Load, 2),

        ],
    ),
    (
        "crates/witchy-wir/src/wir_helpers/collections.rs",
        &[
            (RawKind::Load, 2),

        ],
    ),
    (
        "crates/witchy-wir/src/wir_helpers/collections/operations.rs",
        &[
            (RawKind::Load, 4),
            (RawKind::Store, 1),
            (RawKind::MemoryCopy, 1),





        ],
    ),
    (
        "crates/witchy-wir/src/wir_helpers/dict.rs",
        &[
            (RawKind::Load, 18),
            (RawKind::Load8U, 1),
            (RawKind::Store, 40),
            (RawKind::Store8, 2),
            (RawKind::MemoryCopy, 9),
            (RawKind::MemoryFill, 10),
        ],
    ),
    (
        "crates/witchy-wir/src/wir_helpers/dict/lookup.rs",
        &[
            (RawKind::Load, 20),
            (RawKind::Load8U, 9),


        ],
    ),
    (
        "crates/witchy-wir/src/wir_helpers/dict/read.rs",
        &[
            (RawKind::Load, 12),
            (RawKind::Store, 7),
        ],
    ),
    (
        "crates/witchy-wir/src/wir_helpers/host/environment.rs",
        &[
            (RawKind::Store, 2),
        ],
    ),
    (
        "crates/witchy-wir/src/wir_helpers/memory.rs",
        &[
            (RawKind::Load, 9),
            (RawKind::Store, 13),
            (RawKind::MemoryCopy, 1),
            (RawKind::HeapGlobalWrite, 1),
        ],
    ),
    (
        "crates/witchy-wir/src/wir_helpers/numeric.rs",
        &[
            (RawKind::Load, 2),
            (RawKind::Store8, 10),
            (RawKind::MemoryCopy, 2),

        ],
    ),
    (
        "crates/witchy-wir/src/wir_helpers/strings.rs",
        &[
            (RawKind::Load, 9),
            (RawKind::Load8U, 6),
            (RawKind::Store, 1),





        ],
    ),
    (
        "crates/witchy-wir/src/wir_helpers/strings/host.rs",
        &[
            (RawKind::Store8, 1),
        ],
    ),
    (
        "crates/witchy-wir/src/wir_helpers/strings/transform.rs",
        &[
            (RawKind::Load, 2),
            (RawKind::Load8U, 5),
            (RawKind::Store, 5),
            (RawKind::Store8, 2),
            (RawKind::MemoryCopy, 7),



        ],
    ),
];

fn reviewed_baseline() -> BTreeMap<&'static str, BTreeMap<RawKind, usize>> {
    REVIEWED_BASELINE
        .iter()
        .map(|(file, counts)| (*file, counts.iter().copied().collect()))
        .collect()
}

fn is_nightly() -> bool {
    std::env::var_os("WITCHY_NIGHTLY").is_some_and(|v| v == "1")
        || std::env::var_os("WITCHY_FULL_SUITE").is_some_and(|v| v == "1")
        || std::env::var_os("WITCHY_FULL_CENSUS").is_some_and(|v| v == "1")
}

/// The ceiling and the table must agree, checked in the BLOCKING suite.
///
/// The scan below is nightly because parsing every production file with `syn`
/// costs ~16s. This check is arithmetic over two constants and costs
/// microseconds, so it does not belong behind the same gate.
///
/// This guard has now been lost once already: it landed in "make the census
/// ratchet actually run", and the next shard's rebase took its own copy of this
/// file wholesale and deleted it, along with three of the row corrections. The
/// ceiling then read 477 against a table summing 501 and nothing complained,
/// because the only enforcing test early-returns outside nightly. A ratchet
/// nobody runs is not a ratchet, and a shard that regenerates this file must
/// keep this test.
#[test]
fn reviewed_census_is_internally_consistent() {
    let baseline = reviewed_baseline();
    let baseline_total: usize = baseline.values().flat_map(BTreeMap::values).sum();
    assert_eq!(
        baseline_total, REVIEWED_TOTAL,
        "reviewed census is internally inconsistent: regenerate REVIEWED_BASELINE \
         and REVIEWED_TOTAL together from print_current_safewir_raw_constructor_census",
    );
    assert_eq!(
        baseline.len(),
        REVIEWED_BASELINE.len(),
        "REVIEWED_BASELINE lists a file twice; the map silently dropped an entry",
    );
    for (file, counts) in REVIEWED_BASELINE {
        assert_eq!(
            counts.len(),
            baseline[*file].len(),
            "REVIEWED_BASELINE repeats a RawKind for `{file}`; the map dropped a count",
        );
    }
}

#[test]
fn production_raw_constructor_surface_does_not_grow_during_migration() {
    if !is_nightly() {
        return;
    }
    let baseline = reviewed_baseline();
    let current = current_census();
    let baseline_total: usize = baseline.values().flat_map(BTreeMap::values).sum();
    assert_eq!(baseline_total, REVIEWED_TOTAL, "reviewed census is internally inconsistent");

    for (file, census) in &current {
        let Some(allowed) = baseline.get(file.as_str()) else {
            panic!("new production raw-constructor file `{file}`: {:?}", census.counts);
        };
        for (kind, actual) in &census.counts {
            let maximum = allowed.get(kind).copied().unwrap_or(0);
            assert!(
                *actual <= maximum,
                "raw constructor count grew in `{file}` for {kind:?}: {actual} > {maximum}"
            );
        }
    }

    let current_total: usize = current.values().map(FileCensus::total).sum();
    assert!(
        current_total <= REVIEWED_TOTAL,
        "production raw constructor total grew: {current_total} > {REVIEWED_TOTAL}"
    );
}

#[test]
fn print_current_safewir_raw_constructor_census() {
    if !is_nightly() {
        return;
    }
    let census = current_census();
    let total: usize = census.values().map(FileCensus::total).sum();
    eprintln!("SAFEWIR_CENSUS total={total} files={}", census.len());
    for (file, counts) in census {
        eprintln!("{file}\t{}\t{:?}", counts.total(), counts.counts);
    }
}

#[test]
fn census_distinguishes_semantic_operations_from_aliased_raw_wir() {
    let syntax = syn::parse_file(
        r#"
        fn sample() {
            let _ = Operation::Load { place, width };
            let _ = Operation::Store { place, width };
            let _ = E::Load { ptr, kind, offset };
            let _ = N::Store { ptr, value, kind, offset };
        }

        #[cfg(not(test))]
        fn production_cfg() {
            let _ = E::Load8U { ptr, offset };
        }

        #[cfg(any(test, feature = "production"))]
        fn maybe_production_cfg() {
            let _ = N::Store8 { ptr, value, offset };
        }

        #[cfg(all(test, feature = "fixture"))]
        fn test_only_cfg() {
            let _ = E::Load { ptr, kind, offset };
        }
        "#,
    )
    .expect("parse census fixture");
    let mut census = FileCensus::default();
    ConstructorVisitor { census: &mut census }.visit_file(&syntax);
    assert_eq!(census.counts.get(&RawKind::Load), Some(&1));
    assert_eq!(census.counts.get(&RawKind::Load8U), Some(&1));
    assert_eq!(census.counts.get(&RawKind::Store), Some(&1));
    assert_eq!(census.counts.get(&RawKind::Store8), Some(&1));
    assert_eq!(census.total(), 4);
}
