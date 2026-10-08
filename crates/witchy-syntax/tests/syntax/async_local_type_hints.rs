#[path = "../../src/async_lower/type_hints.rs"]
mod implementation;

use implementation::*;
use witchy_syntax::ast::*;

#[test]
fn hints_bind_actual_source_coordinate_initializer_and_declaration() {
    let original = witchy_syntax::parser::parse_module("async fn f():\n    let callback = fn(): 9\n    var xs = list.repeat(0, 4)\n    ()\n").unwrap();
    let Item::Function(function) = &original.items[0] else { panic!() };
    let Stmt::Let { value, .. } = &function.body.stmts[1] else { panic!() };
    let hint = AsyncLocalTypeHint { module: "main".into(), item: 0, method: None, statement: 1,
        expression: 1, name: "xs".into(), initializer: value.clone(),
        ty: Type::Named("List".into(), vec![Type::Named("Int".into(), vec![])]) };
    let annotated = |hints: &[AsyncLocalTypeHint]| {
        let mut module = original.clone(); apply("main", &mut module, hints);
        let Item::Function(function) = &module.items[0] else { panic!() };
        let Stmt::Let { ty, .. } = &function.body.stmts[1] else { panic!() };
        ty.clone()
    };
    assert_eq!(annotated(&[hint.clone()]), Some(hint.ty.clone()));
    let mut stale = hint.clone(); stale.expression += 1;
    assert_eq!(annotated(&[stale]), None);
    let mut changed = hint.clone(); changed.initializer = Expr::List(vec![]);
    assert_eq!(annotated(&[changed]), None);
    let mut shadow = hint.clone(); shadow.name = "callback".into();
    assert_eq!(annotated(&[shadow]), None);
    assert_eq!(annotated(&[hint.clone(), hint]), None);
}
