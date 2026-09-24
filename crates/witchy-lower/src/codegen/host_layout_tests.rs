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

fn assemble_with_opts(
    module: &Module,
    opts: witchy_syntax::opt::OptSet,
) -> LoweringOutcome<witchy_wir::wir::WirModule> {
    witchy_syntax::opt::set_for_tests(Some(opts));
    let result = assemble_wir_module(&module);
    witchy_syntax::opt::set_for_tests(None);
    result
}

