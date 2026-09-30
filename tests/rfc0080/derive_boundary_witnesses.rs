use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};

fn checked_derived_source(source: &str) -> Result<witchy_types::pipeline::CheckedModule, String> {
    let workspace = PackageCoordinate::new(PackageSource::Workspace, "test/derive", "1").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
    let mut assignments = vec![("main".to_owned(), ModuleLoadIdentity::new(workspace, ["main"]).unwrap())];
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
        (*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
    )));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap();
    witchy::canonical_comptime::check_source_bundle(
        vec![("main".into(), source.as_bytes().to_vec())], "main",
        ["main".to_owned()].into_iter().collect(), owners,
        witchy::canonical_comptime::source_limits(),
    )
}

fn assert_derived_output(source: &str, expected: &[&str]) {
    struct ResetOptimization;
    impl Drop for ResetOptimization {
        fn drop(&mut self) { witchy::opt::set_for_tests(None); }
    }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    let checked = checked_derived_source(source).expect("authenticated derived source");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admitted derived witness");
    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut actor = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print: true, quiet: true, ..witchy::runtime::Capabilities::none()
    }, 64).expect("spawn admitted derived witness");
    actor.run().expect("run admitted derived witness");
    assert_eq!(actor.output(), expected);
    assert_eq!(actor.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[test]
fn public_state_record_sum_and_generic_witnesses_typecheck_and_run() {
    let source = r#"
import public_state

type Account derive(PublicState):
    name: String
    balance: Int

type Event(a) derive(PublicState):
    Created(a)
    Empty

fn main(console: Console):
    public_state.check(Account("Ada", 7))
    let created: Event(String) = Created("created")
    let empty: Event(Int) = Empty
    public_state.check(created)
    public_state.check(empty)
    console.print("ok")
"#;
    assert_derived_output(source, &["ok"]);
}

#[test]
fn public_state_rejects_function_fields() {
    let source = r#"
import public_state

type Bad derive(PublicState):
    callback: fn(Int) -> Int
"#;
    let error = checked_derived_source(source).expect_err("function fields must not derive PublicState");
    assert!(error.contains("PublicState"), "{error}");
}

#[test]
fn derived_order_is_lexicographic_and_preserves_float_none() {
    let source = r#"
import cmp

type Version derive(PartialEq, Eq, PartialOrd, Ord):
    major: Int
    minor: Int

type Measured derive(PartialEq, PartialOrd):
    value: Float
    rank: Int

fn describe(order: Option(Ordering)) -> String:
    match order:
        Some(Less) -> "less"
        Some(Greater) -> "greater"
        Some(Equal) -> "equal"
        None -> "none"

fn main(console: Console):
    console.print(describe(Version(1, 9).partial_compare(Version(1, 12))))
    console.print(describe(Version(1, 12).partial_compare(Version(1, 9))))
    console.print(describe(Version(1, 12).partial_compare(Version(1, 12))))
    let nan = 0.0 / 0.0
    console.print(describe(Measured(nan, 1).partial_compare(Measured(0.0, 2))))
    console.print(describe(Measured(1.0, 1).partial_compare(Measured(1.0, 2))))
"#;
    assert_derived_output(source, &["less", "greater", "equal", "none", "less"]);
}

#[test]
fn derived_ord_generic_bound_accepts_int_and_rejects_float() {
    let accepted = r#"
import cmp

type Boxed(a) derive(PartialEq, Eq, PartialOrd, Ord):
    value: a

fn main(console: Console):
    console.print("${Boxed(1).compare(Boxed(2))}")
"#;
    assert_derived_output(accepted, &["Less"]);

    let rejected = r#"
import cmp

type Boxed(a) derive(PartialEq, Eq, PartialOrd, Ord):
    value: a

fn main(console: Console):
    let _ = Boxed(1.0).compare(Boxed(2.0))
"#;
    let error = checked_derived_source(rejected).expect_err("Float must not satisfy derived Ord bound");
    assert!(error.contains("Ord"), "{error}");
}
