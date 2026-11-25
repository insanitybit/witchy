//! Declared sum types: variants, exhaustive matches, and what each refusal says.
use crate::diagnostic::Diagnostic;
const TYPES:&str="type Shape:\n    Circle(U64)\n    Square(U64)\n    Empty\n\ntype Option<T>:\n    Some(T)\n    None\n\n";
fn checked(rest:&str)->Result<(),Diagnostic>{
    let source=format!("{TYPES}{rest}\nfn main():\n    return\n");
    let logical=super::check::program(&crate::compiler::parse(&source)?)?;super::concrete::program(logical)?;Ok(())
}
fn refused(rest:&str)->String{checked(rest).expect_err(rest).message.to_string()}
const AREA:&str="fn area(s: Shape) -> U64:\n    match s:\n        Circle(r):\n            return 3 * r * r\n        Square(side):\n            return side * side\n        Empty:\n            return 0\n";
#[test]
fn variants_build_values_and_a_match_takes_them_apart(){
    checked(AREA).unwrap();
    checked("fn pick(big: Bool) -> Shape:\n    if big:\n        return Square(9)\n    return Empty\n").unwrap();
    checked("fn wrap(v: U64) -> Option<U64>:\n    if v == 0:\n        return None\n    return Some(v)\n").unwrap();
    checked("fn both(v: U64) -> Result<Option<Shape>, Shape>:\n    if v == 0:\n        return Err(Empty)\n    return Ok(Some(Circle(v)))\n").unwrap();
    checked("fn held(data: Buffer<U8>) -> Option<Buffer<U8>>:\n    return Some(move data)\n").unwrap();
}
#[test]
fn a_match_has_one_arm_for_every_variant(){
    let missing=refused(&AREA.replace("        Square(side):\n            return side * side\n",""));
    assert!(missing.contains("every variant")&&missing.contains("missing: Square"),"{missing}");
    assert!(refused(&format!("{AREA}        Blob:\n            return 1\n")).contains("does not have"));
    assert!(refused(&format!("{AREA}        Empty:\n            return 1\n")).contains("duplicate match arm"));
    assert!(refused(&AREA.replace("Circle(r):\n            return 3 * r * r","Circle:\n            return 3")).contains("names it"));
    assert!(refused("fn f(v: U64) -> U64:\n    match v:\n        Circle(r):\n            return r\n").contains("sum type"));
}
#[test]
fn a_variant_is_written_with_exactly_what_it_holds(){
    assert!(refused("fn f() -> Shape:\n    return Empty(1)\n").contains("holds nothing"));
    assert!(refused("fn f() -> Shape:\n    return Circle\n").contains("holds a value"));
    assert!(refused("fn f() -> Shape:\n    return Circle(true)\n").contains("type mismatch"));
    assert!(refused("fn f() -> U64:\n    let a = Some(1)\n    return 0\n").contains("from the context"));
}
#[test]
fn an_owner_inside_a_variant_is_still_owned(){
    // The Buffer in `Some` must go somewhere.
    let message=refused("fn drop(found: Option<Buffer<U8>>) -> U64:\n    match move found:\n        Some(data):\n            return 1\n        None:\n            return 0\n");
    assert!(message.contains("discard"),"{message}");
}
#[test]
fn declarations_are_closed_and_finite(){
    const ITSELF:&str="can contain itself only inside a Buffer";
    assert!(refused("type List:\n    Cons(List)\n    Nil\n").contains(ITSELF));
    // Through a record field or another sum it is still held directly.
    assert!(refused("type Link:\n    item: U64\n    rest: List\n\ntype List:\n    Cons(Link)\n    Nil\n").contains("only inside a Buffer"));
    assert!(refused("type Chain:\n    item: U64\n    rest: Chain\n").contains("a record type can contain itself only inside a Buffer"));
    assert!(refused("type Even:\n    More(Odd)\n    Done\n\ntype Odd:\n    Next(Even)\n").contains(ITSELF));
    assert!(refused("type Tree:\n    Fork(Option<Tree>)\n    Leaf\n").contains(ITSELF));
    // Another type may have a variant of the same name; the expected type picks (sum/shared-variant.wy).
    checked("type Other:\n    Circle(U64)\n    Dot\n").unwrap();
    assert!(refused("type Other:\n    Dot\n    Dot\n").contains("used once"));
    assert!(refused("type Odd:\n    Ok(U64)\n    Dot\n").contains("used once"));
    assert!(refused("fn Circle(v: U64) -> U64:\n    return v\n").contains("conflicts"));
    assert!(crate::compiler::parse("sealed type Mixed:\n    Dot\n\nfn main():\n    return\n").is_err());
    assert!(crate::compiler::parse("type Mixed:\n    size: U64\n    Dot\n\nfn main():\n    return\n").is_err());
}
#[test]
fn a_sum_is_stored_in_buffer_cells_and_record_fields(){
    checked("type Tagged:\n    shape: Shape\n    weight: U64\n\nfn weigh(tagged: Tagged) -> U64:\n    match tagged.shape:\n        Circle(r):\n            return r + tagged.weight\n        Square(side):\n            return side\n        Empty:\n            return 0\n").unwrap();
    checked("fn put(cells: &mut Buffer<Shape>, shape: Shape) -> Result<(), Shape>:\n    return memory.try_init(cells, 0, shape)\n").unwrap();
    checked("fn get(cells: &mut Buffer<Option<Buffer<U8>>>) -> Result<Option<Buffer<U8>>, U64>:\n    return memory.take(cells, 0)\n").unwrap();
    // A sum that holds a reference lives only in locals.
    assert!(checked("type Peek:\n    Seen(&U64)\n    Gone\n\nfn keep(cells: Buffer<Peek>) -> Buffer<Peek>:\n    return move cells\n").is_err());
}
#[test]
fn a_sum_holds_itself_behind_a_buffer(){
    checked("type Tree:\n    Fork(Buffer<Tree>)\n    Leaf(U64)\n\nfn leaf(value: U64) -> Tree:\n    return Leaf(value)\n").unwrap();
    // Through a generic record whose field is the Buffer, and through another sum.
    checked("type Rows<T>:\n    cells: Buffer<T>\n    count: U64\n\ntype Tree:\n    Fork(Rows<Tree>)\n    Leaf(U64)\n\nfn fork(rows: Rows<Tree>) -> Tree:\n    return Fork(move rows)\n").unwrap();
    checked("type Member:\n    name: U64\n    value: Doc\n\ntype Doc:\n    Object(Buffer<Member>)\n    Words(Option<Buffer<Doc>>)\n    Nothing\n\nfn nothing() -> Doc:\n    return Nothing\n").unwrap();
    // It owns what it holds, whatever its other variants are.
    let message=refused("type Tree:\n    Fork(Buffer<Tree>)\n    Leaf(U64)\n\nfn lose(tree: Tree) -> U64:\n    return 0\n");
    assert!(message.contains("discard"),"{message}");
    assert!(refused("type Tree:\n    Fork(Buffer<Tree>)\n    Seen(&U64)\n").contains("cannot hold a reference"));
}
#[test]
fn a_record_holds_itself_behind_a_buffer(){
    checked("type Node:\n    value: U64\n    children: Buffer<Node>\n\nfn value(node: &Node) -> U64:\n    return (*node).value\n").unwrap();
    checked("type Rows<T>:\n    cells: Buffer<T>\n    count: U64\n\ntype Node:\n    value: U64\n    children: Rows<Node>\n\nfn value(node: &Node) -> U64:\n    return (*node).value\n").unwrap();
    assert!(refused("type Both<T>:\n    left: T\n    right: T\n\ntype Node:\n    value: U64\n    children: Both<Node>\n").contains("only inside a Buffer"));
}
#[test]
fn a_rest_arm_takes_the_variants_that_have_no_arm(){
    checked("fn round(s: Shape) -> Bool:\n    match s:\n        Circle(_):\n            return true\n        _:\n            return false\n").unwrap();
    checked("fn round(s: &Shape) -> Bool:\n    match s:\n        Circle(r):\n            return *r > 0\n        _:\n            return false\n").unwrap();
    checked("fn held(o: &Option<Buffer<U8>>) -> Bool:\n    match o:\n        Some(_):\n            return true\n        _:\n            return false\n").unwrap();
    checked("fn count(s: Shape) -> U64:\n    var n: U64 = 0\n    match s:\n        Empty:\n            n += 1\n        _:\n            n += 2\n    return n\n").unwrap();
}
#[test]
fn a_rest_arm_is_last_is_taken_and_loses_no_owner(){
    let rest=|arms:&str|refused(&format!("fn f(o: Option<Buffer<U8>>, s: Shape) -> U64:\n{arms}"));
    assert!(rest("    match s:\n        _:\n            return 1\n        Empty:\n            return 0\n").contains("comes last"));
    assert!(rest("    match s:\n        Circle(a):\n            return a\n        Square(b):\n            return b\n        Empty:\n            return 0\n        _:\n            return 1\n").contains("never taken"));
    assert!(rest("    match s:\n        Empty:\n            return 0\n        _(x):\n            return 1\n").contains("holds no value"));
    assert!(rest("    match move o:\n        None:\n            return 0\n        _:\n            return 1\n").contains("would lose the owner that `Some` holds"));
    assert!(rest("    match move o:\n        None:\n            return 0\n        Some(_):\n            return 1\n").contains("would lose the owner"));
}
/// Review 2026-11-21: `memory.try_init` on a buffer of buffers answers `Result<(), Buffer<_>>`,
/// which has no tag word (zero is `Ok`, the owner handed back is `Err`), and the emitter wrote
/// a tag into its one i64 carrier: a module the engine refused. The emitter now builds that
/// result as the layout says, and stops the compiler if a variant number is ever written into a
/// local that is not a tag word (emit.rs `tag_word`), which is what fails here if this returns.
/// The fixture runs the same program in the engine (`buffer/try-init-buffers`).
#[test]
fn try_init_hands_back_an_owner_in_a_result_without_a_tag(){
    let (module,report)=crate::compiler::compile_report(include_str!("../../tests/fixtures/buffer/try-init-buffers.wy")).expect("compiles");
    assert!(!module.is_empty());
    // No tag in what `try_init` answers for either element type (a buffer, a record holding only
    // a buffer), nor in what `try_release` answers for the two buffer types released.
    // (The storage keys come between the two, docs/wasm-results.md: counted apart.)
    assert_eq!(report.matches("\"representation\":\"zero-niche\"").count(),4);
    assert_eq!(report.matches("\"zero_variant\":0").count(),4);
}
