use super::*;
use crate::{codegen, interpreter, parser, typeck};

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
        let linked = resolve_std_src(src);
        let want = vec!["2".to_string()];
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpreter"),
            want,
            "interpreter"
        );
        let bytes = codegen::compile_module_binary(&linked).expect_lowered("compile wasm");
        assert_eq!(crate::run_wasm_bytes(&bytes).expect("wasm"), want, "wasm");
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
    fn rfc0081_receiver_and_nested_var_writebacks_agree_across_backends() {
        let src = r#"
trait CounterOps:
    fn bare(self) -> Int
    fn inspect(let self) -> Int
    fn tail(var self) -> Int
    fn explicit(var self) -> Int
    fn question(var self) -> Result(Int, String)
    fn adjust(let self, var value: Int) -> Int
    fn pair(let self, var left: Int, var right: Int) -> Int
    fn announce(let self, console: Console)
    fn take(own self) -> Int

type Counter:
    Counter(Int)

type Holder:
    item: dyn CounterOps

type Slots:
    left: Int
    right: Int

fn tail_step(var value: Counter) -> Int:
    let Counter(current) = value
    value = Counter(current + 1)
    current + 1

impl CounterOps for Counter:
    fn bare(self) -> Int:
        match self:
            Counter(value) -> value

    fn inspect(let self) -> Int:
        match self:
            Counter(value) -> value

    fn tail(var self) -> Int:
        tail_step(self)

    fn explicit(var self) -> Int:
        let Counter(current) = self
        self = Counter(current + 2)
        return current + 2

    fn question(var self) -> Result(Int, String):
        let Counter(current) = self
        self = Counter(current + 3)
        Err("stopped")?

    fn adjust(let self, var value: Int) -> Int:
        value = value + 1
        match self:
            Counter(current) -> current + value

    fn pair(let self, var left: Int, var right: Int) -> Int:
        left = left + 1
        right = right + 2
        match self:
            Counter(current) -> current + left + right

    fn announce(let self, console: Console):
        match self:
            Counter(value) -> console.print("counter=${value}")

    fn take(own self) -> Int:
        match self:
            Counter(value) -> value

fn direct(console: Console):
    var counter = Counter(1)
    var slots = Slots(3, 9)
    console.print("${counter.bare()} ${counter.inspect()}")
    console.print("${counter.tail()} ${counter.explicit()}")
    let ignored = counter.question()
    let adjusted = counter.adjust(slots.left)
    console.print("${adjusted} ${counter.pair(slots.left, slots.right)} ${counter.inspect()} ${slots.left} ${slots.right}")
    counter.announce(console)
    let consumed = Counter(12)
    console.print("${consumed.take()}")

fn dynamic(console: Console):
    var holder = Holder(Counter(1))
    var slots = Slots(3, 9)
    console.print("${holder.item.bare()} ${holder.item.inspect()}")
    console.print("${holder.item.tail()} ${holder.item.explicit()}")
    let ignored = holder.item.question()
    let adjusted = holder.item.adjust(slots.left)
    console.print("${adjusted} ${holder.item.pair(slots.left, slots.right)} ${holder.item.inspect()} ${slots.left} ${slots.right}")
    holder.item.announce(console)
    let consumed: dyn CounterOps = Counter(12)
    console.print("${consumed.take()}")

fn main(console: Console):
    direct(console)
    dynamic(console)
"#;
        let linked = resolve_std_src(src);
        let interpreter =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpreter");
        let bytes = codegen::compile_module_binary(&linked).expect_lowered("compile wasm");
        let wasm = crate::run_wasm_bytes(&bytes).expect("wasm");
        let one_backend = vec![
            "1 1".to_string(),
            "2 4".to_string(),
            "11 23 7 5 11".to_string(),
            "counter=7".to_string(),
            "12".to_string(),
        ];
        assert_eq!(interpreter, [one_backend.clone(), one_backend.clone()].concat());
        assert_eq!(wasm, interpreter);
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

        let moved = r#"
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

    #[test]
    fn rfc0081_var_receiver_traps_before_writeback_on_both_backends() {
        let src = r#"
trait Explode:
    fn explode(var self) -> Int

type Counter:
    Counter(Int)

impl Explode for Counter:
    fn explode(var self) -> Int:
        self = Counter(99)
        1 / 0

fn main() -> Int:
    var counter: dyn Explode = Counter(1)
    counter.explode()
"#;
        let linked = resolve_std_src(src);
        let interpreter_error = interpreter::run_module(linked.clone(), ".", Vec::new())
            .expect_err("interpreter call must trap")
            .to_string();
        let bytes = codegen::compile_module_binary(&linked).expect_lowered("compile wasm");
        let wasm_error = crate::run_wasm_bytes(&bytes).expect_err("wasm call must trap");
        assert!(
            interpreter_error.contains("division by zero"),
            "{interpreter_error}"
        );
        assert!(
            wasm_error.contains("divide by zero") || wasm_error.contains("division by zero"),
            "{wasm_error}"
        );
    }

    #[test]
    fn rfc0081_normal_and_opt_modes_have_identical_values_and_traps() {
        let values = r#"
trait Render:
    fn render(let self) -> String

type Number:
    Number(Int)

type Label:
    Label(String)

impl Render for Number:
    fn render(let self) -> String:
        match self:
            Number(value) -> "number=${value}"

impl Render for Label:
    fn render(let self) -> String:
        match self:
            Label(value) -> "label=${value}"

fn main(console: Console):
    let values: List(dyn Render) = [Number(7), Label("safe")]
    for value in values:
        console.print(value.render())
"#;
        let run = |source: &str| {
            let linked = resolve_std_src(source);
            let interpreted =
                interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpreter");
            let bytes = codegen::compile_module_binary(&linked).expect_lowered("compile wasm");
            let compiled = crate::run_wasm_bytes(&bytes).expect("wasm");
            assert_eq!(compiled, interpreted);
            interpreted
        };
        let normal = run(values);
        let opt = run(&format!("mode opt\n{values}"));
        assert_eq!(normal, vec!["number=7", "label=safe"]);
        assert_eq!(opt, normal);

        let trap = r#"
trait Explode:
    fn explode(let self) -> Int

type Bomb:
    Bomb

impl Explode for Bomb:
    fn explode(let self) -> Int:
        1 / 0

fn main() -> Int:
    let value: dyn Explode = Bomb
    value.explode()
"#;
        let fail = |source: &str| {
            let linked = resolve_std_src(source);
            let interpreted = interpreter::run_module(linked.clone(), ".", Vec::new())
                .expect_err("interpreter trap")
                .to_string();
            let bytes = codegen::compile_module_binary(&linked).expect_lowered("compile wasm");
            let compiled = crate::run_wasm_bytes(&bytes).expect_err("wasm trap");
            assert!(interpreted.contains("division by zero"), "{interpreted}");
            assert!(
                compiled.contains("divide by zero") || compiled.contains("division by zero"),
                "{compiled}"
            );
            (interpreted, compiled)
        };
        let normal_traps = fail(trap);
        let opt_traps = fail(&format!("mode opt\n{trap}"));
        assert_eq!(normal_traps.0, normal_traps.1);
        assert_eq!(opt_traps.0, opt_traps.1);
        let normal_kind = normal_traps.0.rsplit(": ").next().unwrap_or(&normal_traps.0);
        let opt_kind = opt_traps.0.rsplit(": ").next().unwrap_or(&opt_traps.0);
        assert_eq!(normal_kind, opt_kind);
    }

    /// FROM / INTO (std/convert): a user implements `From` and gets `Into` free via
    /// the blanket `impl Into(b) for a where b: From(a)`. The blanket body calls the
    /// STATIC `b.from(self)` on the bound target type (no receiver), resolved through
    /// the bound at monomorphization. Both backends.
    #[test]
    fn from_into_conversion_traits() {
        let src = "import convert\n\ntype Celsius:\n    deg: Int\n\nimpl From(Int) for Celsius:\n    fn from(value: Int) -> Celsius:\n        Celsius(value)\n\nfn main(console: Console):\n    let c: Celsius = (5).into()\n    let d = Celsius.from(9)\n    console.print(\"${c.deg} ${d.deg}\")\n";
        let want = vec!["5 9".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-534) RFC-0042's qualified type spelling composes with static trait
    /// methods: plain `import json` exposes the type as `json.Json`, and that
    /// receiver should reach the same `From(a) for Json` impl as bare `Json.from`.
    #[test]
    fn qualified_type_receiver_static_trait_method_backends_agree() {
        let src = "import json\nimport reflect\n\ntype Point derive(Reflect):\n    x: Int\n\nfn main(console: Console):\n    let p = Point(7)\n    let j = json.Json.from(p)\n    console.print(json.encode(j))\n";
        let want = vec!["{\"x\":7}".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// `say` covers every scalar out of the box (Duration in its HUMAN form
    /// — the custom rendering `Show` exists for), and a missing impl is a
    /// clean check-time error naming the trait and type, not a post-lowering
    /// "unknown function" artifact.
    #[test]
    fn show_scalars_and_missing_impl_diagnostic() {
        let src = "import show\n\nfn main(console: Console):\n    show.say(console, 42)\n    show.say(console, 3.5)\n    show.say(console, 90s)\n    show.say(console, true)\n";
        let want: Vec<String> =
            ["42", "3.5", "1m30s", "true"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
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

    /// `examples/traits/src/traits.witchy` — defines a custom `Shape` trait, implements it for
    /// three types, and dispatches generically (`where s: Shape`). Monomorphized,
    /// so it runs identically on both backends.
    #[test]
    fn traits_example_dispatches_a_custom_trait() {
        assert_eq!(
            crate::execute_file("examples/traits/src/traits.witchy", Vec::new()).unwrap(),
            vec![
                "square with area 25",
                "rectangle with area 12",
                "right triangle with area 12",
                "total of three squares: 29",
            ]
        );
    }

    #[test]
    fn std_show_list_backends_agree() {
        // The blanket `impl Show for List(a) where a: Show` renders via the
        // works for a user type (Coord) that the built-in to_string cannot print.
        // Monomorphized dispatch keeps it content-correct on both backends.
        let client = r#"
import show

type Coord:
    Coord(Int, Int)

impl Show for Coord:
    fn show(self) -> String:
        match self:
            Coord(x, y) -> (((("(" + "${x}") + ",") + "${y}") + ")")

fn main(console: Console):
    console.print(show([1, 2, 3]))
    console.print(show(["a", "b"]))
    console.print(show([Coord(0, 0), Coord(1, 2)]))
    console.print(show([true, false]))
"#;
        let sources = [
            ("show", crate::bundled_module("show").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std show_list diverged");
        assert_eq!(
            compiled,
            vec!["[1, 2, 3]", "[a, b]", "[(0,0), (1,2)]", "[true, false]"]
        );
    }

    #[test]
    fn inherent_impl_in_indentation_syntax() {
        // The inherent impl works under the off-side rule too: `impl Point:`.
        let client = "type Point:\n    Point(Int, Int)\n\nimpl Point:\n    fn sum(self) -> Int:\n        match self:\n            Point(x, y) -> x + y\n\nfn main(console: Console):\n    console.print(\"${sum(Point(4, 5))}\")\n";
        assert_eq!(interp(client), vec!["9"]);
        assert_eq!(run_on_wasm(client), vec!["9"]);
    }

    #[test]
    fn inherent_impl_methods_dispatch_by_type() {
        // `impl Type { fn m(self) ... }` (no trait) defines methods dispatched by
        // receiver type, reusing the trait machinery. Two types share the method
        // name `mag`; each call resolves to the right one. Both backends agree.
        let client = r#"
type Point:
    Point(Int, Int)

type Circle:
    Circle(Int)

impl Point:
    fn mag(self) -> Int:
        match self:
            Point(x, y) -> ((x * x) + (y * y))

impl Circle:
    fn mag(self) -> Int:
        match self:
            Circle(r) -> (r * r)

fn main(console: Console):
    console.print("${mag(Point(3, 4))}")
    console.print("${mag(Circle(6))}")
"#;
        assert_eq!(interp(client), vec!["25", "36"]);
        assert_eq!(run_on_wasm(client), vec!["25", "36"]);
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
    fn push(var self, x: a) -> Nil:
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
    fn recursive_trait_dispatch_on_match_bound_fields() {
        // A trait method can now dispatch on a variable bound by a constructor
        // pattern when the field type is concrete: `show(x)` / `show(c)` inside a
        // Show impl resolve through the match arm. Covers a nested struct (Named
        // holds a Coord) and stays content-correct on both backends.
        let client = r#"
import show

type Coord:
    Coord(Int, Int)

impl Show for Coord:
    fn show(self) -> String:
        match self:
            Coord(x, y) -> (((("(" + show(x)) + ", ") + show(y)) + ")")

type Named:
    Named(String, Coord)

impl Show for Named:
    fn show(self) -> String:
        match self:
            Named(label, c) -> ((label + "=") + show(c))

fn main(console: Console):
    console.print(show(Coord(3, 4)))
    console.print(show(Named("p", Coord(1, 2))))
    console.print(show([Coord(0, 0), Coord(5, 6)]))
"#;
        let sources = [
            ("show", crate::bundled_module("show").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "recursive show dispatch diverged");
        assert_eq!(compiled, vec!["(3, 4)", "p=(1, 2)", "[(0, 0), (5, 6)]"]);
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

    // Generic functions instantiated at several distinct types within one
    // program: pair_up / first_of / second_of are used at (Int,Int),
    // (String,String), and (Int,String). Per-call generalization must give each
    // call site its own instantiation, and both backends must agree.
    #[test]
    fn generics_at_multiple_types_backends_agree() {
        let src = r#"
fn pair_up(x: a, y: b) -> (a, b):
    (x, y)

fn first_of(p: (a, b)) -> a:
    let (f, s) = p
    f

fn second_of(p: (a, b)) -> b:
    let (f, s) = p
    s

fn main(console: Console):
    let pi = pair_up(1, 2)
    let ps = pair_up("a", "b")
    let pm = pair_up(7, "mixed")
    console.print("${first_of(pi)}")
    console.print(first_of(ps))
    console.print(second_of(ps))
    console.print("${first_of(pm)}")
    console.print(second_of(pm))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "multi-type generics diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "a", "b", "7", "mixed"]);
    }

    // Indentation syntax with traits/impls and a nested if/else expression.
    #[test]
    fn indentation_traits_backends_agree() {
        let src = r#"
trait Describe:
    fn describe(self) -> String

impl Describe for Int:
    fn describe(self) -> String:
        "${self}"

impl Describe for Bool:
    fn describe(self) -> String:
        if self:
            "yes"
        else:
            "no"

fn main(console: Console):
    console.print(describe(42))
    console.print(describe(true))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "indentation traits diverged");
        assert_eq!(run_on_wasm(src), vec!["42", "yes"]);
    }
