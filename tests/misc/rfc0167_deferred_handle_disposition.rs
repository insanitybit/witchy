//! Temporary implementation boundary: private erasure is not disposition.
//! Lawful data-only recipes remain replayable; no universal must-Task rule.
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

#[test]
fn bridge_policy_uses_authenticated_identity_and_recursive_stored_fields() {
    witchy_types::source_bridge_obligation_self_checks::authenticated_identity_and_stored_obligations();
}

fn assert_pending_refusal(source: &str) {
    let error = match witchy::resolve_std_only_checked(source) {
        Err(error) => error,
        Ok(_) => panic!("pending owner erasure must refuse"),
    };
    let message = error.to_string();
    assert!(message.contains("erases a stored must-consume obligation")
        && message.contains("deferred resource disposition is unsupported"), "{message}");
}

#[test]
fn private_resource_erasure_refuses_in_both_modes() {
    // Exact baseline admitted loss, not merely an unused factory declaration.
    assert_pending_refusal(r#"import chan
async fn child(console: Console):
    chan.yield_now().await
    chan.yield_now().await
    console.print("child escaped cancellation")
async fn main(console: Console):
    let handle = chan.spawn(child(console)).await
    let pending = chan.cancel(handle)
    console.print("cold cancellation discarded")
"#);
    // Join and cancel share the same authenticated erasure bridge. One opt
    // join plus the actual normal cancel loss covers both without four copies
    // of the full std pipeline.
    assert_pending_refusal("mode opt\nimport task\nfn pending(own h: task.Handle):\n    let recipe = task.join(h)\nfn main():\n    ()\n");
}

#[test]
fn private_resource_erasure_survives_generic_and_imported_wrappers() {
    let fast = r#"mode opt
import task
must type Envelope:
    Envelope(task.Handle)
pub fn pending(own h: task.Handle) -> task.Task(()):
    task.cancel(h)
"#;
    use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};
    let application = PackageCoordinate::new(PackageSource::Workspace, "test/app", "1").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
    let mut assignments = ["main", "fast"].into_iter().map(|name| (
        name.to_string(), ModuleLoadIdentity::new(application.clone(), ["source", name]).unwrap(),
    )).collect::<Vec<_>>();
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
        (*name).to_string(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
    )));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap();
    let user_modules = ["main".to_string(), "fast".to_string()].into_iter().collect();
    for body in [
        "fn relay[a](own value: a, operation: fn(own a) -> task.Task(())) -> task.Task(()):\n    operation(value)\nfn pending(own h: task.Handle):\n    let recipe = relay(h, fast.pending)\n",
        "fn pending(own value: fast.Envelope):\n    let recipe = match value:\n        fast.Envelope(h) -> task.join(h)\n",
    ] {
        let main = format!("import task\nimport fast\n{body}\nfn main():\n    ()\n");
        let error = match witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
            vec![("main".into(), main.into_bytes()), ("fast".into(), fast.as_bytes().to_vec())],
            "main", witchy_interp::comptime::expand_compile_time,
            &user_modules, owners.clone(),
        ) {
            Err(error) => error,
            Ok(_) => panic!("import cannot erase pending ownership"),
        };
        assert!(error.to_string().contains("erases a stored must-consume obligation"), "{error}");
    }
}

#[test]
fn synchronous_resource_disposition_can_return_replayable_data() {
    opt::set_for_tests(Some(OptSet::none()));
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { opt::set_for_tests(None); } }
    let _reset = Reset;
    let source = r#"import task
must type Ticket:
    Ticket(Int)
fn finish(own ticket: Ticket) -> task.Task(Int):
    let value = match ticket:
        Ticket(value) -> value
    task.done(value)
async fn main(console: Console):
    let recipe = finish(Ticket(7))
    let first = recipe.await
    let second = recipe.await
    console.print("${first * 10 + second}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("lawful synchronous disposition");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("ordinary recipe remains replayable");
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, ..Capabilities::none()
    }, 128).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), vec!["77"]);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}
