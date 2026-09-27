//! Copyable projections do not duplicate an opaque aggregate's affine siblings.
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

const WORKER: &str = r#"
must type Ticket:
    Ticket(Int)

type Viewed(a):
    value: a
    snapshot: &Int
    count: Int
    label: String

pub fn bump[a](own box: Viewed(a)) -> Viewed(a):
    Viewed(count: box.count + 1, ..box)

pub fn wrap_ticket(own ticket: Ticket, snapshot: &Int) -> Viewed(Ticket):
    Viewed(ticket, snapshot, 0, "x")

pub fn finish(own box: Viewed(Ticket)) -> Int:
    let label = if box.count > 0: box.label else: box.label
    match bump(box):
        Viewed(ticket, snapshot, count, _) -> match move ticket:
            Ticket(value) -> value * 100 + count * 10 + (*snapshot) + label.length()
"#;

const MAIN: &str = r#"
import worker
fn main() -> Int:
    let root = 5
    let first = worker.finish(worker.Viewed(worker.Ticket(7), &root, 2, "ab" + "c"))
    let second = worker.finish(worker.Viewed(worker.Ticket(7), &root, 0, "ab" + "c"))
    let third = worker.finish(worker.wrap_ticket(worker.Ticket(7), &root))
    first * 1000000 + second * 1000 + third
"#;

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn opaque_environment_field_read_preserves_affine_siblings_and_retained_children() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    for (main_mode, worker_mode) in [("", ""), ("mode opt\n", "mode opt\n"), ("", "mode opt\n")] {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("worker".into(), format!("{worker_mode}{WORKER}").into_bytes()),
                 ("main".into(), format!("{main_mode}{MAIN}").into_bytes())],
            "main", witchy_interp::comptime::EXPANDER,
        ).expect("checked affine aggregate projections");
        witchy_lower::canonical_owner_field_checks::projection_mutations_refuse(
            checked.canonical_source().expect("canonical source"));
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("admitted unoptimized field projections");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128,
        ).expect("admitted projection Wasm");
        vm.run().expect("projection execution");
        // Field-result edges: 700+30+5+3=738; 700+10+5+3=718.
        // The owned Ticket parameter moves into Viewed before finish:
        // 700+10+5+1=716, with no second parameter drop.
        assert_eq!(vm.output(), vec!["738718716"]);
        assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
    }
}
