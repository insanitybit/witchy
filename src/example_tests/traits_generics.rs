use super::*;
use crate::{codegen, interpreter, parser, typeck};

fn assert_closed_trait_parity(source: &str, expected: &[&str]) {
    let checked = witchy::resolve_std_only_checked(source).expect("source-backed trait fixture checks");
    let source_output = witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("source interpreter executes trait fixture");
    assert_eq!(source_output, expected, "independent source expectation");
    let (artifact, oracle) = witchy::codegen::compile_checked_module_artifact_with_observer(
        &checked, |validated, artifact| {
            use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterValue};
            let entry = artifact.entry().expect("source-bound entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry.export().strip_prefix('f').unwrap().parse().unwrap());
            let tokens = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
            interpret_entry_program(validated, function, tokens, &[], Default::default())
                .expect("canonical interpreter executes closed traits")
        },
    ).expect_lowered("closed traits admit through ordinary canonical semantics");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.output, source_output);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print: true, quiet: true, ..witchy::runtime::Capabilities::none()
    }, 128).expect("spawn admitted trait fixture");
    vm.run().expect("execute admitted trait fixture");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.output(), source_output);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}

    /// (BUG-538 / D9) Core values should compose through the public protocols,
    /// not through backend magic. This is the release gate over the protocol
    /// matrix: representative std values must have deliberate `Show`, `Reflect`,
    /// and `PartialEq`/`Eq` behavior, including when nested inside containers.
    #[test]
    fn core_protocol_matrix_composes_on_both_backends() {
        let src = r#"import bytes
import cmp
import encoding
import json
import list
import reflect
import set
import show
import testing

type Label derive(Reflect, PartialEq, Eq):
    Label(String)

impl Show for Label:
    fn show(self) -> String:
        match self:
            Label(s) -> "<" + s + ">"

type ProtocolRow derive(Reflect):
    label: Label
    payload: Bytes
    wait: Duration
    order: Ordering
    choices: Set(Int)
    outcome: Result(Bytes, String)
    tupled: (Int, String, Bool, Duration, Ordering)

fn same(x: a, y: a) -> Bool where a: PartialEq:
    x == y

fn total_same(x: a, y: a) -> Bool where a: Eq:
    x == y

fn sorted_window(xs: List(a)) -> String where a: Ord, a: Show:
    var sortable = xs
    sortable.sort()
    show.render(sortable) + "|" + show.render(list.min(sortable)) + "|" + show.render(list.max(sortable))

fn main(console: Console):
    let b = bytes.from_string("hi")
    let other_b = bytes.from_string("hi")
    let s = set.from_list([1, 2, 2])
    let other_s = set.from_list([2, 1])
    let outcome: Result(Bytes, String) = Ok(b)
    let other_outcome: Result(Bytes, String) = Ok(other_b)
    let tup = (7, "x", true, 90s, Greater)
    let other_tup = (7, "x", true, 90s, Greater)
    let labels = [Label("x"), Label("y")]
    let other_labels = [Label("x"), Label("y")]
    let row = ProtocolRow(Label("packet"), b, 90s, Greater, s, outcome, tup)

    console.print(sorted_window([3, 1, 2]))
    console.print(sorted_window(["b", "a", "c"]))
    console.print("${labels}")
    console.print(show.render(labels))
    console.print(show.render(b))
    console.print(show.render(90s))
    console.print(show.render(Greater))
    console.print(show.render(s))
    console.print(show.render(outcome))
    console.print(show.render(tup))
    let hex = encoding.hex_encode_bytes(b)
    match encoding.hex_decode_bytes(hex):
        Ok(back) -> console.print(hex + ":" + back.to_string())
        Err(e) -> console.print("bad:" + show.render(e))
    match encoding.hex_decode_bytes("zz"):
        Ok(_) -> console.print("bad")
        Err(_) -> console.print("hex-err")
    console.print(json.stringify(row))

    testing.assert_value_eq(labels, other_labels)
    console.print("${same(b, other_b)}")
    console.print("${total_same(b, other_b)}")
    console.print("${same(labels, other_labels)}")
    console.print("${same(Some(Greater), Some(Greater))}")
    console.print("${same(outcome, other_outcome)}")
    console.print("${total_same(s, other_s)}")
    console.print("${same(tup, other_tup)}")
    console.print("${total_same(tup, other_tup)}")
    console.print(show.render("nope".parse_int()))
    console.print(show.render(list.get([10, 20], 9)))
    match json.decode("1 2"):
        Ok(_) -> console.print("bad")
        Err(_) -> console.print("json-err")
"#;
        let expected = [
            "[1, 2, 3]|Some(1)|Some(3)",
            "[a, b, c]|Some(a)|Some(c)",
            "[<x>, <y>]",
            "[<x>, <y>]",
            "Bytes(len=2)",
            "1m30s",
            "Greater",
            "{1, 2}",
            "Ok(Bytes(len=2))",
            "(7, x, true, 1m30s, Greater)",
            "6869:hi",
            "hex-err",
            "{\"label\":{\"$variant\":\"Label\",\"$values\":[\"packet\"]},\"payload\":[104,105],\"wait\":90000,\"order\":{\"$variant\":\"Greater\",\"$values\":[]},\"choices\":[1,2],\"outcome\":{\"$variant\":\"Ok\",\"$values\":[[104,105]]},\"tupled\":[7,\"x\",true,90000,{\"$variant\":\"Greater\",\"$values\":[]}]}",
            "true",
            "true",
            "true",
            "true",
            "true",
            "true",
            "true",
            "true",
            "None",
            "None",
            "json-err",
        ];
        assert_eq!(link_run(src), expected, "interp: core protocol matrix");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: core protocol matrix",
        );
    }

    #[test]
    fn rfc0081_supertrait_upcast_backends_agree() {
        let src = r#"
trait Base:
    fn base(let self) -> Int

trait Render: Base:
    fn render(let self) -> Int

type Label:
    Label(Int)

impl Base for Label:
    fn base(let self) -> Int:
        match self:
            Label(value) -> value

impl Render for Label:
    fn render(let self) -> Int:
        match self:
            Label(value) -> value + 10

fn main(console: Console):
    let rendered: dyn Render = Label(2)
    let base: dyn Base = rendered
    console.print("${base.base()}")
"#;
        assert_closed_trait_parity(src, &["2"]);
    }

    #[test]
    fn canonical_closed_trait_open_parameter_does_not_mint_payload_authority() {
        let checked = witchy::resolve_std_only_checked(r#"
trait Base:
    fn base(let self) -> Int
fn forward(value: dyn Base) -> Int:
    value.base()
"#).expect("open trait source checks");
        assert!(checked.canonical_source().unwrap().typed_module().module().items.iter().all(|item| {
            !matches!(item, witchy::ast::Item::Type(definition) if definition.name.starts_with("$canonical.TraitPayload_"))
        }));
        witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_unsupported("an open trait parameter is not closed payload evidence");
    }

    #[test]
    fn rfc0081_rejects_unrelated_existential_upcasts_before_execution() {
        let src = r#"
trait Render:
    fn render(let self) -> Int

trait Inspect:
    fn inspect(let self) -> Int

type Label:
    Label(Int)

impl Render for Label:
    fn render(let self) -> Int:
        match self:
            Label(value) -> value

impl Inspect for Label:
    fn inspect(let self) -> Int:
        match self:
            Label(value) -> value

fn main() -> Int:
    let rendered: dyn Render = Label(2)
    let inspect: dyn Inspect = rendered
    inspect.inspect()
"#;
        let linked = resolve_std_src(src);
        let interpreter_error = interpreter::run_module(linked.clone(), ".", Vec::new())
            .expect_err("interpreter must reject unrelated upcast")
            .to_string();
        assert!(
            interpreter_error
                .contains("invalid existential upcast request `main.Render` to `main.Inspect`"),
            "unexpected interpreter error: {interpreter_error}"
        );
        let codegen_error = codegen::compile_module_binary(&linked)
            .expect_rejected("compiled backend must reject unrelated upcast")
            .to_string();
        assert!(
            codegen_error
                .contains("invalid existential upcast request `main.Render` to `main.Inspect`"),
            "unexpected codegen error: {codegen_error}"
        );
    }

    #[test]
    fn rfc0081_rejects_aliased_var_places_and_use_after_own() {
        let aliases = r#"
trait Adjust:
    fn clash(let self, var left: Int, var right: Int) -> Int

type Counter:
    Counter(Int)

impl Adjust for Counter:
    fn clash(let self, var left: Int, var right: Int) -> Int:
        left = left + 1
        right = right + 1
        left + right

fn main() -> Int:
    let counter: dyn Adjust = Counter(1)
    var value = 3
    counter.clash(value, value)
"#;
        let aliases = resolve_std_src(aliases);
        let alias_interpreter = interpreter::run_module(aliases.clone(), ".", Vec::new())
            .expect_err("interpreter must reject overlapping var places")
            .to_string();
        let alias_codegen = codegen::compile_module_binary(&aliases)
            .expect_rejected("compiled backend must reject overlapping var places")
            .to_string();
        for alias_error in [&alias_interpreter, &alias_codegen] {
            assert!(
                alias_error.contains("overlapping `var` places rooted in `value`"),
                "{alias_error}"
            );
        }

        let moved = r#"mode opt

trait Consume:
    fn take(own self) -> Int

type Counter:
    Counter(Int)

impl Consume for Counter:
    fn take(own self) -> Int:
        match self:
            Counter(value) -> value

fn main() -> Int:
    let counter: dyn Consume = Counter(1)
    let first = counter.take()
    first + counter.take()
"#;
        let moved = resolve_std_src(moved);
        let move_interpreter = interpreter::run_module(moved.clone(), ".", Vec::new())
            .expect_err("interpreter must reject use after own")
            .to_string();
        let move_codegen = codegen::compile_module_binary(&moved)
            .expect_rejected("compiled backend must reject use after own")
            .to_string();
        for move_error in [&move_interpreter, &move_codegen] {
            assert!(
                move_error.contains("was already consumed")
                    || move_error.contains("use after move")
                    || move_error.contains("after it was moved"),
                "{move_error}"
            );
        }
    }

    /// A missing `Show` impl is a clean check-time error naming the trait and
    /// type, not a post-lowering "unknown function" artifact.
    #[test]
    fn show_missing_impl_diagnostic() {
        let missing = "import show\n\ntype Blob:\n    n: Int\n\nfn main(console: Console):\n    show.say(console, Blob(1))\n";
        let module = parser::parse_module(missing).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("missing impl must be rejected");
        assert!(
            err.to_string().contains("`Blob` does not implement `Show`"),
            "want a clean trait error, got: {err}"
        );
    }

    #[test]
    fn inherent_impl_on_generic_type() {
        // An inherent `impl Stack(a):` carries the type's OWN parameter, so each
        // method's `self` is `Stack(a)` (not a bare `Stack`) and the methods
        // monomorphize per element type. Covers a static constructor (`empty`), an
        // instance method returning Self (`push`, chained off the static), and an
        // instance method on a let-bound chain receiver (`howbig`). Two distinct
        // element types exercise monomorphization; both backends agree.
        let client = r#"
type Stack(a):
    items: List(a)

impl Stack(a):
    fn empty() -> Stack(a):
        Stack([])
    fn push(var self, x: a) -> ():
        list.push(self.items, x)
    fn howbig(self) -> Int:
        list.length(self.items)

fn main(console: Console):
    var s = Stack.empty()
    s.push(1)
    s.push(2)
    s.push(3)
    console.print("${s.howbig()}")
    var w = Stack.empty()
    w.push("a")
    w.push("b")
    console.print("${w.howbig()}")
"#;
        assert_eq!(interp(client), vec!["3", "2"]);
        assert_eq!(run_on_wasm(client), vec!["3", "2"]);
    }

    #[test]
    fn convert_from_into_backends_agree() {
        // std/convert's From/Into: implementing `From` gives `.from(x)` on the
        // target type, and the blanket `impl Into for a where b: From(a)` derives
        // `.into()`. Both resolve and run identically on both backends — the
        // blanket trait impl + From->Into derivation was otherwise untested.
        let client = r#"
import convert

type Celsius:
    Celsius(Int)

type Fahrenheit:
    Fahrenheit(Int)

impl From(Celsius) for Fahrenheit:
    fn from(value: Celsius) -> Fahrenheit:
        match value:
            Celsius(deg) -> Fahrenheit(deg * 9 / 5 + 32)

fn degf(f: Fahrenheit) -> Int:
    match f:
        Fahrenheit(d) -> d

fn main(console: Console):
    console.print("${degf(Fahrenheit.from(Celsius(100)))}")
    let f: Fahrenheit = Celsius(0).into()
    console.print("${degf(f)}")
    let body: Fahrenheit = Celsius(37).into()
    console.print("${degf(body)}")
"#;
        let sources = [
            ("convert", crate::bundled_module("convert").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "convert From/Into diverged");
        assert_eq!(compiled, vec!["212", "32", "98"]);
    }

    #[test]
    fn method_calls_resolve_to_real_methods_only() {
        // Method-call syntax resolves to impl methods (instance + static) and
        // trait-bound dispatch — NOT to arbitrary free functions. A free
        // function called as a method is a loud error naming the spelling.
        let client = r#"
type Counter:
    n: Int

impl Counter:
    fn fresh() -> Counter:
        Counter(0)
    fn bumped(self) -> Counter:
        Counter(self.n + 1)

fn main(console: Console):
    let c = Counter.fresh().bumped().bumped()
    console.print("${c.n}")
"#;
        let want = vec!["2".to_string()];
        assert_eq!(link_run(client), want, "interpreter");
        assert_eq!(wasm_run(client), want, "wasm");
        // Free-function UFCS is gone — one cut, loud error.
        let ufcs = "fn inc(x: Int) -> Int:\n    x + 1\n\nfn main(console: Console):\n    console.print(\"${5.inc()}\")\n";
        let module = parser::parse_module(ufcs).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("free-fn UFCS must be rejected");
        assert!(
            err.to_string().contains("methods come from `impl` blocks"),
            "got: {err}"
        );
    }

