use super::*;

struct ScalarResolver;

impl witchy_wir::layout::ClosedTypeResolver for ScalarResolver {
    fn resolve_named<'a>(
        &'a self,
        name: &str,
        _arguments: &[Type],
    ) -> Option<witchy_wir::layout::ResolvedNamed<'a>> {
        (name == "Int").then_some(witchy_wir::layout::ResolvedNamed::Scalar(
            witchy_wir::layout::ScalarKind::Int,
        ))
    }
}

struct PointResolver<'a> {
    definition: &'a witchy_syntax::ast::TypeDef,
}

impl witchy_wir::layout::ClosedTypeResolver for PointResolver<'_> {
    fn resolve_named<'a>(
        &'a self,
        name: &str,
        _arguments: &[Type],
    ) -> Option<witchy_wir::layout::ResolvedNamed<'a>> {
        match name {
            "Int" => Some(witchy_wir::layout::ResolvedNamed::Scalar(
                witchy_wir::layout::ScalarKind::Int,
            )),
            "Point" => Some(witchy_wir::layout::ResolvedNamed::PackedRecord(
                self.definition,
            )),
            _ => None,
        }
    }
}

#[test]
fn production_host_layout_registry_starts_fail_closed() {
    let policy = host_layout::production_host_layout_policy("future_structured_host_adapter");
    let mut layouts = LayoutInterner::new();
    let layout = layouts
        .intern_type(&Type::Named("Int".into(), Vec::new()), &ScalarResolver)
        .expect("validated scalar descriptor");

    assert_eq!(
        policy.decide(&layouts, layout),
        witchy_wir::layout::HostLayoutDecision::Reject,
    );
}

#[test]
fn boundary_layout_scan_includes_a_specialized_result_without_specialized_arguments() {
    let module = witchy_syntax::parser::parse_module(
        "mode opt\n\ntype Point packed:\n    x: Int\n",
    )
    .expect("parse packed result descriptor");
    let definition = module
        .items
        .iter()
        .find_map(|item| match item {
            witchy_syntax::ast::Item::Type(definition) if definition.name == "Point" => {
                Some(definition)
            }
            _ => None,
        })
        .expect("Point definition");
    let mut layouts = LayoutInterner::new();
    let result = layouts
        .intern_type(
            &Type::Named("Point".into(), Vec::new()),
            &PointResolver { definition },
        )
        .expect("validated specialized result descriptor");
    let arguments = Vec::<LayoutId>::new();
    let mut inspected = Vec::new();

    assert!(host_layout::boundary_layout_is_unsupported(
        arguments.into_iter(),
        Some(result),
        |layout| {
            inspected.push(layout);
            true
        },
    ));
    assert_eq!(inspected, vec![result]);
}

#[test]
fn generic_packed_result_boundary_lowers_through_the_exact_layout_adapter() {
    // RFC-0111: a `list.at` on an exact packed-list whose element is an inline
    // aggregate is materialized in place — the whole element is its row address,
    // read field-by-field through the descriptor. This is the exact LayoutId
    // adapter the fail-closed boundary policy demands, so the read now lowers
    // rather than being rejected as an unsupported box/reshape.
    let module = witchy_syntax::parser::parse_module(
        "mode opt\n\n\
         type Point packed:\n    x: Int\n\n\
         fn main() -> Int:\n    let points = [Point(7)]\n    let point = list.at(points, 0)\n    point.x\n",
    )
    .expect("parse packed result-boundary fixture");
    let wir = assemble_wir_module(&module)
        .expect_lowered("materialized packed list.at lowers through the exact layout adapter");
    let wat = witchy_wir::wir::to_wat(&wir);
    assert!(
        wat.contains("__witchy_packed_list_"),
        "the packed list constructor is retained (no reshape to a boxed list): {wat}"
    );
    assert!(
        !wat.contains("call $list_at"),
        "the packed element is read in place, not via the slot `list_at` helper: {wat}"
    );
}

fn assemble_with_opts(
    module: &Module,
    opts: witchy_syntax::opt::OptSet,
) -> LoweringOutcome<witchy_wir::wir::WirModule> {
    witchy_syntax::opt::set_for_tests(Some(opts));
    let result = assemble_wir_module(&module);
    witchy_syntax::opt::set_for_tests(None);
    result
}

fn link_list_app(source: &str) -> Module {
    fn no_comptime_expansion(
        _name: &str,
        _module: &mut Module,
        _modules: &[(String, Module)],
    ) -> Result<witchy_syntax::origin::OriginTable, String> {
        Ok(witchy_syntax::origin::OriginTable::default())
    }

    let list = witchy_syntax::parser::parse_module(
        witchy_syntax::linker::bundled_source("list").expect("bundled list module"),
    )
    .expect("parse bundled list module");
    let app = witchy_syntax::parser::parse_module(source).expect("parse list app");
    witchy_syntax::linker::link_with_user_modules(
        vec![("list".into(), list), ("app".into(), app)],
        "app",
        no_comptime_expansion,
        &std::collections::HashSet::from(["app".to_string()]),
    )
    .expect("link list app")
}

#[test]
fn nested_bool_lists_fall_back_from_dynamic_inline_specialization() {
    let source = "mode opt\n\n\
                  fn main() -> Int:\n    let row = [true]\n    let rows = [row]\n    rows.length()\n";

    let module = witchy_syntax::parser::parse_module(source).expect("parse nested Bool lists");
    let wir = assemble_with_opts(&module, witchy_syntax::opt::OptSet::default_set())
        .expect_lowered("a dynamic nested list must retain the uniform outer-list representation");
    let wat = witchy_wir::wir::to_wat(&wir);
    assert!(wat.contains("__witchy_packed_list_"), "the inner Bool list stays packed: {wat}");
    let packed_length_one_constructors = wat
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            line.starts_with("(func $__witchy_packed_list_") && line.contains("_n1 ")
        })
        .count();
    assert_eq!(
        packed_length_one_constructors, 1,
        "only the inner Bool list is specialized; the outer dynamic list is uniform: {wat}"
    );
}

#[test]
fn tuple_wrapped_bool_list_does_not_make_the_outer_list_inline() {
    let source = "mode opt\n\n\
                  fn main() -> Int:\n    let row = [true]\n    let wrapped = (row, 7)\n    let outer = [wrapped]\n    outer.length()\n";

    let module = witchy_syntax::parser::parse_module(source).expect("parse tuple wrapper");
    assemble_with_opts(&module, witchy_syntax::opt::OptSet::default_set())
        .expect_lowered("a tuple containing a dynamic list is not an inline outer-list element");
}

#[test]
fn packed_named_wrapper_with_dynamic_instantiation_is_not_inlineable() {
    let module = witchy_syntax::parser::parse_module(
        "mode opt\n\ntype Wrapper(a) packed:\n    value: a\n",
    )
    .expect("parse packed wrapper descriptor");
    let resolver = assembly::ModuleLayoutResolver::new(&module, Vec::new(), true);
    let wrapped = Type::Named(
        "Wrapper".into(),
        vec![Type::Named(
            "List".into(),
            vec![Type::Named("Bool".into(), Vec::new())],
        )],
    );

    assert!(assembly::type_contains_dynamic_list(&wrapped, &resolver));
    assert!(!assembly::type_requests_specialized_layout(&wrapped, &resolver));
}

#[test]
fn bool_list_across_a_function_boundary_falls_back_cleanly() {
    let source = "mode opt\n\n\
                  fn filled(value: Bool) -> List(Bool):\n    [value]\n\n\
                  fn main() -> Int:\n    if filled(true).at(0):\n        1\n    else:\n        0\n";

    let module = witchy_syntax::parser::parse_module(source).expect("parse Bool-list boundary");
    assemble_with_opts(&module, witchy_syntax::opt::OptSet::default_set())
        .expect_lowered("a boundary-crossing Bool list must use the supported uniform ABI");
}

#[test]
fn bool_list_across_a_lambda_boundary_falls_back_cleanly() {
    let source = "mode opt\n\n\
                  fn main() -> Int:\n    let first = fn(flags: List(Bool)) -> Int:\n        if flags.at(0):\n            1\n        else:\n            0\n    first([true])\n";

    let module = witchy_syntax::parser::parse_module(source).expect("parse Bool-list lambda");
    let wir = assemble_with_opts(&module, witchy_syntax::opt::OptSet::default_set())
        .expect_lowered("a lambda-crossing Bool list must use the supported uniform ABI");
    let wat = witchy_wir::wir::to_wat(&wir);
    assert!(
        !wat.contains("__witchy_packed_list_"),
        "the lambda boundary forces the uniform Bool-list representation: {wat}"
    );
}

#[test]
fn default_local_only_bool_list_remains_byte_specialized() {
    let source = "mode opt\n\n\
                  fn main() -> Int:\n    let flags = [true, false, true]\n    if flags.at(2):\n        1\n    else:\n        0\n";

    let module = witchy_syntax::parser::parse_module(source).expect("parse Bool-list fixture");
    let wir = assemble_with_opts(&module, witchy_syntax::opt::OptSet::default_set())
        .expect_lowered("a local-only Bool list retains byte specialization");
    let wat = witchy_wir::wir::to_wat(&wir);
    assert!(wat.contains("__witchy_packed_list_"), "packed Bool-list helper: {wat}");
    assert!(wat.contains("i32.load8_u"), "byte-wide Bool-list read: {wat}");
}

#[test]
fn default_nsieve_shape_keeps_the_packed_bool_adapter_path() {
    let module = link_list_app(
        "mode opt\n\nimport list\n\n\
         fn sieve(n: Int) -> Int:\n    var flags = list.repeat(true, n)\n    var count = 0\n    var i = 0\n    while i < list.length(flags):\n        if list.at(flags, i):\n            count = count + 1\n            flags[i] = false\n        i = i + 1\n    count\n\n\
         fn main() -> Int:\n    sieve(16)\n",
    );

    let wir = assemble_with_opts(&module, witchy_syntax::opt::OptSet::default_set())
        .expect_lowered("repeat/length/at/set_at is a complete packed Bool-list route");
    let wat = witchy_wir::wir::to_wat(&wir);
    assert!(wat.contains("__witchy_packed_list_"), "packed constructor route: {wat}");
    assert!(wat.contains("i32.load8_u"), "byte-wide nsieve read: {wat}");
    assert!(
        wat.contains("i32.store8"),
        "descriptor-aware byte-wide nsieve write: {wat}"
    );
    assert!(
        !wat.contains("call $__witchy_packed_scalar_set_"),
        "the exact packed Bool loop consumes its sequence cursor instead of calling the scalar setter: {wat}"
    );
}

#[test]
fn unbox_only_bool_push_set_and_read_fall_back_together() {
    let module = link_list_app(
        "mode opt\n\nimport list\n\n\
         fn main() -> Int:\n    var flags = list.repeat(false, 1)\n    list.push(flags, true)\n    flags[0] = true\n    if list.at(flags, 0) && list.at(flags, 1):\n        1\n    else:\n        0\n",
    );
    let wir = assemble_with_opts(
        &module,
        witchy_syntax::opt::OptSet::none().with(witchy_syntax::opt::Opt::Unbox),
    )
    .expect_lowered("unbox alone must preserve Bool-list push/set/read semantics");
    let wat = witchy_wir::wir::to_wat(&wir);
    assert!(
        !wat.contains("__witchy_packed_list_")
            && !wat.contains("i32.load8_u")
            && !wat.contains("call $list_repeat_bool"),
        "Bool-list operations share the uniform fallback without in-place: {wat}"
    );
}
