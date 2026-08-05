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
