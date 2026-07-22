//! RFC-0080 definition-site resolution for compiler-owned typed tag output.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{ast, codegen, interpreter, parser, pipeline, typeck};

const SUPPORT: &str = r#"
type ImportedValue:
    ImportedValue(Int)
"#;

const TAG_LIBRARY: &str = r#"
import meta
import support

fn hidden() -> Int:
    40

fn selected() -> Int:
    0

type SelectedRecord:
    value: Int

fn selected_record() -> SelectedRecord:
    SelectedRecord(value: 0)

type HiddenValue:
    HiddenValue(Int)

type HiddenRecord:
    value: Int

type HiddenAlias = HiddenValue
type ImportedAlias = support.ImportedValue

type LocalPatternValue:
    LocalPatternValue(String)

type PatternEnvelope(a):
    PatternEnvelope(a)

pub comptime fn answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        hidden() + 2

pub comptime fn lexical(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        (fn(hidden: Int): hidden + 1)(41)

pub comptime fn call_selected(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let selected = meta.expr_name(meta.call_site("selected"))
    quote expr:
        ${selected}()

pub comptime fn reference_selected(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    meta.expr_name(meta.call_site("selected"))

pub comptime fn composed_call_selected(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let selected = meta.expr_name(meta.call_site("selected"))
    meta.expr_call(selected, [])

pub comptime fn composed_field_selected(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let selected = meta.expr_name(meta.call_site("selected_record"))
    let record = meta.expr_call(selected, [])
    meta.expr_field(record, meta.ident("value"))

pub comptime fn composed_match_selected(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let selected = meta.expr_name(meta.call_site("selected"))
    let value = meta.expr_call(selected, [])
    let arm = meta.match_arm(meta.pattern_int(42), meta.expr_call(selected, []))
    let fallback = meta.match_arm(meta.pattern_wildcard(), meta.expr_int(0))
    meta.expr_match(value, [arm, fallback])

pub comptime fn composed_pattern_selected(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let first = meta.pattern_ctor(meta.call_site("LocalPatternValue"), [meta.pattern_var(meta.ident("left"))])
    let exact = meta.pattern_list([first])
    let second = meta.pattern_ctor(meta.call_site("LocalPatternValue"), [meta.pattern_var(meta.ident("right"))])
    let alternative = meta.pattern_or([second])
    let rest = meta.pattern_list_rest([alternative], Some(meta.ident("tail")))
    let tuple = meta.pattern_tuple([exact, rest])
    let pattern = meta.pattern_ctor(meta.ident("PatternEnvelope"), [tuple])
    let local = meta.expr_name(meta.call_site("LocalPatternValue"))
    let payload = quote expr:
        ([${local}(20)], [${local}(22), ${local}(0)])
    let envelope = meta.expr_name(meta.ident("PatternEnvelope"))
    let scrutinee = meta.expr_call(envelope, [payload])
    let body = quote expr:
        left + right
    meta.expr_match(scrutinee, [meta.match_arm(pattern, body)])

pub comptime fn construct_hidden(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        match HiddenValue(41):
            HiddenValue(value) -> value + 1

pub comptime fn type_hidden(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        (fn(value: HiddenValue): 42)(HiddenValue(0))

pub comptime fn record_hidden(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        HiddenRecord(value: 42).value

pub comptime fn alias_hidden(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        (fn(value: HiddenAlias) -> Int:
            let copied = region -> HiddenAlias:
                value
            match copied:
                HiddenValue(number) -> number + 1
        )(HiddenValue(41))

pub comptime fn preserve_hole(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let value = meta.expr_raw(list.at(holes, 0))
    quote expr:
        ${value}

pub comptime fn imported_hidden(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        (fn(imported: ImportedAlias): match imported:
            ImportedValue(value) -> value + 1
        )(support.ImportedValue(41))

"#;

const CONSUMER: &str = r#"
import tag_library

fn hidden() -> Int:
    0

type HiddenValue:
    HiddenValue(String)

type HiddenRecord:
    value: String

type ImportedValue:
    ImportedValue(String)

type HiddenAlias = HiddenRecord

type LocalSelectedRecord:
    value: Int

type LocalPatternValue:
    LocalPatternValue(Int)

fn main(console: Console):
    let hidden = fn() -> Int:
        1
    let selected = fn() -> Int:
        42
    let selected_record = fn() -> LocalSelectedRecord:
        LocalSelectedRecord(value: 42)
    console.print("${answer"ignored"}")
    console.print("${lexical"ignored"}")
    console.print("${call_selected"ignored"}")
    let selected_fn = reference_selected"ignored"
    console.print("${selected_fn()}")
    console.print("${composed_call_selected"ignored"}")
    console.print("${composed_field_selected"ignored"}")
    console.print("${composed_match_selected"ignored"}")
    console.print("${composed_pattern_selected"ignored"}")
    console.print("${construct_hidden"ignored"}")
    console.print("${type_hidden"ignored"}")
    console.print("${record_hidden"ignored"}")
    console.print("${alias_hidden"ignored"}")
    match preserve_hole"${HiddenValue("call site")}":
        HiddenValue(value) -> console.print(value)
    console.print("${imported_hidden"ignored"}")
"#;

fn linked() -> ast::Module {
    let support = parser::parse_module(SUPPORT).expect("parse typed tag support module");
    let library = parser::parse_module(TAG_LIBRARY).expect("parse typed tag library");
    let consumer = parser::parse_module(CONSUMER).expect("parse typed tag consumer");
    let linked = pipeline::link(
        vec![
            ("support".into(), support),
            ("tag_library".into(), library),
            ("main".into(), consumer),
        ],
        "main",
    )
    .expect("link definition-site typed tag program");
    typeck::check(&linked).expect("typecheck definition-site typed tag program");
    linked
}

#[test]
fn typed_tag_names_resolve_at_definition_site_on_both_backends() {
    let linked = linked();
    let expected = vec![
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "call site".to_string(),
        "42".to_string(),
    ];

    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected,
    );

    let wasm = codegen::compile_module_binary(&linked).expect_lowered("compile typed tag program");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        )
        .expect("spawn");
    actor.run().expect("run compiled typed tag program");
    assert_eq!(actor.output(), expected);
}

#[test]
fn definition_site_markers_are_consumed_before_typechecking() {
    let linked = linked();
    let rendered = format!("{linked:?}");
    assert!(!rendered.contains("@definition_site:"), "{rendered}");
    assert!(!rendered.contains("@definition_site_"), "{rendered}");
    assert!(!rendered.contains("@call_site:"), "{rendered}");
    assert!(rendered.contains("tag_library.hidden"), "{rendered}");
    assert!(rendered.contains("tag_library.HiddenValue"), "{rendered}");
    assert!(rendered.contains("tag_library.HiddenRecord"), "{rendered}");
    assert!(rendered.contains("support.ImportedValue"), "{rendered}");
    assert!(!rendered.contains("HiddenAlias"), "{rendered}");
}

#[test]
fn call_site_rejects_non_identifiers() {
    let source = r#"
import meta

comptime:
    let unsupported = meta.call_site("not.valid")
"#;
    let parsed = parser::parse_module(source).expect("parse invalid call-site origin");
    let error = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect_err("call-site origins still require one valid identifier")
        .to_string();
    assert!(
        error.contains("meta.call_site") && error.contains("expected an identifier"),
        "{error}"
    );
}

#[test]
fn call_site_pattern_identifiers_cannot_become_rest_bindings() {
    let source = r#"
import meta

comptime:
    let _ = meta.pattern_list_rest([], Some(meta.call_site("rest")))
"#;
    let parsed = parser::parse_module(source).expect("parse invalid call-site rest binding");
    let error = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect_err("call-site pattern identifiers are reference-only")
        .to_string();
    assert!(
        error.contains("meta.pattern_list_rest")
            && error.contains("binding identifier")
            && error.contains("reference-only"),
        "{error}"
    );
}

#[test]
fn call_site_type_constructor_and_pattern_resolve_in_the_consumer() {
    let library = parser::parse_module(
        r#"
import meta

type ConsumerValue:
    ConsumerValue(String)

type ConsumerInner:
    ConsumerInner(String)

type ConsumerEmpty:
    ConsumerEmpty(String)

type ConsumerAlias(a) = List(a)

pub comptime fn inspect_type(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let inner_ty = meta.type_named(meta.call_site("ConsumerInner"), [])
    let ty = meta.type_named(meta.call_site("ConsumerValue"), [inner_ty])
    let inner_constructor = meta.expr_name(meta.call_site("ConsumerInner"))
    let constructor = meta.expr_name(meta.call_site("ConsumerValue"))
    quote expr:
        (fn(item: ${ty}) -> Int: 42)(${constructor}(${inner_constructor}(41)))

pub comptime fn inspect_pattern(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let binding = meta.pattern_var(meta.ident("value"))
    let inner_pattern = meta.pattern_ctor(meta.call_site("ConsumerInner"), [binding])
    let pattern = meta.pattern_ctor(meta.call_site("ConsumerValue"), [inner_pattern])
    let inner_constructor = meta.expr_name(meta.call_site("ConsumerInner"))
    let constructor = meta.expr_name(meta.call_site("ConsumerValue"))
    quote expr:
        match ${constructor}(${inner_constructor}(41)):
            ${pattern} -> value + 1

pub comptime fn inspect_empty(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let constructor = meta.expr_name(meta.call_site("ConsumerEmpty"))
    let pattern = meta.pattern_ctor(meta.call_site("ConsumerEmpty"), [])
    quote expr:
        match ${constructor}:
            ${pattern} -> 42

pub comptime fn inspect_alias(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let argument = quote type:
        Int
    let ty = meta.type_named(meta.call_site("ConsumerAlias"), [argument])
    let constructor = meta.expr_name(meta.call_site("ConsumerValue"))
    quote expr:
        (fn(item: ${ty}) -> Int: 42)(${constructor}(41))
"#,
    )
    .expect("parse call-site identity tag");
    let consumer = parser::parse_module(
        r#"
import tag_library

type ConsumerValue(a):
    ConsumerValue(a)

type ConsumerInner:
    ConsumerInner(Int)

type ConsumerEmpty:
    ConsumerEmpty

type ConsumerAlias(a) = ConsumerValue(a)

fn main(console: Console):
    console.print("${inspect_type"ignored"}")
    console.print("${inspect_pattern"ignored"}")
    console.print("${inspect_empty"ignored"}")
    console.print("${inspect_alias"ignored"}")
"#,
    )
    .expect("parse call-site identity consumer");
    let linked = pipeline::link(
        vec![
            ("tag_library".into(), library),
            ("main".into(), consumer),
        ],
        "main",
    )
    .expect("link call-site type and constructor identities");
    typeck::check(&linked).expect("typecheck call-site type and constructor identities");

    let rendered = format!("{linked:?}");
    assert!(!rendered.contains("@call_site"), "{rendered}");
    assert!(rendered.contains("main.ConsumerValue"), "{rendered}");

    let expected = vec![
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
    ];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected,
    );
    let wasm = codegen::compile_module_binary(&linked)
        .expect_lowered("compile call-site type and constructor identities");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        )
        .expect("spawn");
    actor.run().expect("run compiled call-site identity program");
    assert_eq!(actor.output(), expected);
}

#[test]
fn definition_site_identity_does_not_bypass_sealed_construction() {
    let library = parser::parse_module(
        r#"
import meta

sealed type Token:
    value: Int

pub comptime fn forge(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        Token(value: 7)
"#,
    )
    .expect("parse sealed tag library");
    let consumer = parser::parse_module(
        r#"
import tag_library

fn main():
    let _ = forge"ignored"
"#,
    )
    .expect("parse sealed tag consumer");
    let error = pipeline::link(
        vec![
            ("tag_library".into(), library),
            ("main".into(), consumer),
        ],
        "main",
    )
    .expect_err("definition-site resolution must not grant sealed construction authority")
    .to_string();

    assert!(
        error.contains("sealed type")
            && error.contains("Token")
            && error.contains("cannot construct"),
        "{error}"
    );
}

#[test]
fn generated_field_update_cannot_bypass_sealed_typechecking() {
    let library = parser::parse_module(
        r#"
import meta

sealed type Token:
    value: Int

pub fn token() -> Token:
    Token(value: 1)

pub comptime fn mutate(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let token = meta.expr_raw(list.at(holes, 0))
    quote expr:
        (fn(token: Token) -> Token:
            var copy = token
            copy.value = 7
            copy
        )(${token})
"#,
    )
    .expect("parse sealed tag library");
    let consumer = parser::parse_module(
        r#"
import tag_library

fn main():
    let token_value = tag_library.token()
    let _ = mutate"${token_value}"
"#,
    )
    .expect("parse sealed tag consumer");
    let linked = pipeline::link(
        vec![
            ("tag_library".into(), library),
            ("main".into(), consumer),
        ],
        "main",
    )
    .expect("link generated field update");
    let error = typeck::check(&linked)
        .expect_err("the typed layer must reject updates to sealed record values")
        .to_string();

    assert!(
        error.contains("sealed") && error.contains("update"),
        "{error}"
    );
}

#[test]
fn sealed_record_update_is_legal_in_the_defining_module() {
    let module = parser::parse_module(
        r#"
sealed type Token:
    value: Int

fn update_at_home(var token: Token) -> Token:
    token.value = 2
    token

fn main():
    var token = Token(value: 1)
    let _ = update_at_home(token)
"#,
    )
    .expect("parse home-module sealed update");
    let linked = pipeline::link(vec![("main".into(), module)], "main")
        .expect("link home-module sealed update");
    typeck::check(&linked).expect("the defining module retains sealed construction authority");
}

#[test]
fn ambient_sealed_record_update_uses_its_canonical_owner() {
    let module = parser::parse_module(
        r#"
import set

fn overwrite(var values: Set(Int)) -> Set(Int):
    values.items = []
    values

fn main():
    let _ = 0
"#,
    )
    .expect("parse ambient sealed update");
    let linked = pipeline::link(vec![("main".into(), module)], "main")
        .expect("link ambient sealed update");
    let error = typeck::check(&linked)
        .expect_err("ambient sealed records retain their stdlib owner")
        .to_string();
    assert!(
        error.contains("Set") && error.contains("defining module `set`"),
        "{error}"
    );
}

#[test]
fn unknown_definition_site_record_does_not_capture_consumer_type() {
    let library = parser::parse_module(
        r#"
import meta

pub comptime fn forge(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        ConsumerOnly(value: 7)
"#,
    )
    .expect("parse tag library");
    let consumer = parser::parse_module(
        r#"
import tag_library

type ConsumerOnly:
    value: Int

fn main():
    let _ = forge"ignored"
"#,
    )
    .expect("parse consumer");
    let error = pipeline::link(
        vec![
            ("tag_library".into(), library),
            ("main".into(), consumer),
        ],
        "main",
    )
    .expect_err("definition-site record names must not capture consumer declarations")
    .to_string();

    assert!(
        error.contains("unknown constructor") && error.contains("ConsumerOnly"),
        "{error}"
    );
}
