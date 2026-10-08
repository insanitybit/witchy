use witchy_lower::analysis::{analyze, module_cliffs, checked_module_cliffs, Summaries};
use witchy_syntax::ast::Item;

fn module(source: &str) -> witchy_syntax::ast::Module {
    let mut sources = witchy_syntax::linker::STD_MODULES.iter().map(|name|
        ((*name).to_string(), witchy_syntax::linker::std_source(name).expect("bundled std").as_bytes().to_vec()))
        .collect::<Vec<_>>();
    sources.push(("main".into(), source.as_bytes().to_vec()));
    witchy_types::pipeline::link_checked_from_source_bundle(sources, "main",
        witchy_syntax::linker::ComptimeExpander::new(super::no_expand)).expect("checked diagnostic source").module().clone()
}
const SOURCE: &str = r#"import list
type Callback:
    Callback(fn(var List(Int)) -> Bool)
fn drive(callback: Callback):
    var xs = [0]
    var i = 0
    while i < 4:
        match callback:
            Callback(step) -> if step(xs): () else: ()
        list.push(xs, i)
        i = i + 1
"#;

#[test]
fn checked_pattern_callback_writeback_does_not_invent_a_copy_cliff() {
    let module = module(SOURCE);
    let summaries = Summaries::of_module(&module);
    let function = module.items.iter().find_map(|item| match item {
        Item::Function(function) if function.name == "main.drive" => Some(function), _ => None,
    }).expect("driver");
    assert!(analyze(&function.body, &summaries).cliffs.iter().any(|cliff| cliff.var == "xs"));
    let typed = witchy_types::typeck::annotate_checked(witchy_types::traits::lower(module)).expect("typed module");
    let cliffs = checked_module_cliffs(&typed).expect("checked callback facts");
    assert!(cliffs.is_empty(), "{cliffs:?}");
}

#[test]
fn checked_callback_diagnostics_preserve_real_snapshot_and_shared_argument_cliffs() {
    let snapshot = SOURCE.replace("        match callback:", "        let before = xs\n        match callback:");
    let retaining = SOURCE.replace("fn(var List(Int)) -> Bool", "fn(List(Int)) -> Bool");
    for source in [snapshot, retaining] {
        let module = module(&source);
        let typed = witchy_types::typeck::annotate_checked(witchy_types::traits::lower(module)).expect("typed near miss");
        let cliffs = checked_module_cliffs(&typed).expect("checked near miss");
        assert!(cliffs.iter().any(|(function, cliff)| function == "main.drive" && cliff.var == "xs"));
    }
}

#[test]
fn unavailable_callback_contract_keeps_conservative_diagnostics() {
    let mut module = module(SOURCE);
    for item in &mut module.items {
        if let Item::Function(function) = item {
            witchy_syntax::ast::visit::visit_block_mut(&mut function.body, &mut |expression| {
                if let witchy_syntax::ast::Expr::Call { name, .. } = expression && name == "step" { *name = "unknown".into(); }
                Ok::<_, ()>(())
            }).unwrap();
        }
    }
    assert!(witchy_types::typeck::annotate_checked(witchy_types::traits::lower(module.clone())).is_err());
    assert!(module_cliffs(&module).iter().any(|(_, cliff)| cliff.var == "xs"));
}
