//! Concrete generic recursive nominals must carry their instantiated layout and
//! ownership through the source interpreter, canonical oracle, and admitted Wasm.

#[test]
fn nominal_dictionary_keys_use_declared_equality_across_three_legs() {
    assert_three_legs(r#"
import cmp
import dict

type ById derive(Eq):
    id: Int
    cache: Int

impl PartialEq for ById:
    fn eq(self, other: ById) -> Bool:
        self.id == other.id

type ByCache derive(Eq):
    id: Int
    cache: Int

impl PartialEq for ByCache:
    fn eq(self, other: ByCache) -> Bool:
        self.cache == other.cache

fn main(console: Console):
    var ids: Dict(ById, Int) = dict.new()
    let original = ById(1, 10)
    ids[original] = 1
    let snapshot = ids
    ids[ById(1, 20)] = 2
    ids[ById(2, 20)] = 3
    var caches: Dict(ByCache, Int) = dict.new()
    caches[ByCache(1, 10)] = 4
    caches[ByCache(2, 10)] = 5
    console.print("${ids.length()} ${ids.get_or(ById(1, 30), 0)} ${ids.get_or(ById(3, 20), 0)}")
    console.print("${snapshot.length()} ${snapshot.get_or(ById(1, 99), 0)} ${original.cache}")
    console.print("${caches.length()} ${caches.get_or(ByCache(99, 10), 0)}")
    var scalars: Dict(Int, Int) = dict.new()
    scalars[1] = 6
    scalars[1] = 7
    console.print("${scalars.length()} ${scalars.get_or(1, 0)}")
"#, vec!["2 2 0".into(), "1 1 10".into(), "1 5".into(), "1 7".into()]);
}

#[test]
fn anonymous_record_reflect_field_uses_its_concrete_instantiation() {
    assert_three_legs(r#"
import reflect
from json import Json

fn main(console: Console):
    console.print(json.encode(Json.from(.{x: 7})))
    console.print(json.encode(Json.from(.{x: "seven"})))
"#, vec![r#"{"x":7}"#.to_owned(), r#"{"x":"seven"}"#.to_owned()]);
}

fn assert_three_legs(source: &str, expected: Vec<String>) {
    assert_three_legs_with_config(source, expected, Default::default());
}

fn assert_three_legs_with_config(
    source: &str,
    expected: Vec<String>,
    config: witchy_wir::canonical_interpreter::InterpreterConfig,
) {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("recursive generic source checks");
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(),
        ".",
        Vec::new(),
        Vec::new(),
    )
    .expect("source interpreter executes both concrete Stack instantiations");
    assert_eq!(source_output, expected);

    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("recursive generic source admits");
    let entry = artifact.entry().expect("main entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').expect("canonical export prefix").parse().expect("function id"),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("oracle validation");
    let capabilities = entry.capabilities().iter().copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability)
        .collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated,
        function,
        capabilities,
        config,
    )
    .expect("canonical oracle executes both concrete Stack instantiations");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);

    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        witchy::runtime::Capabilities {
            print: true,
            quiet: true,
            ..Default::default()
        },
        256,
    )
    .expect("admitted instance");
    vm.run().expect("Wasm executes both concrete Stack instantiations");
    assert_eq!(vm.output(), expected);
    assert_eq!(
        vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"),
        0,
    );
}
