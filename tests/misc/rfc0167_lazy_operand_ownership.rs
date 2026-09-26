//! Conditional ownership uses the same control-flow join in both modes.
//! Only the LHS and selected RHS may consume/print: 7, 8, 9, then result 24.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

const RESOURCE: &str = r#"
must type Ticket:
    Ticket(Int)

pub fn finish(console: Console, own ticket: Ticket) -> Int:
    match ticket:
        Ticket(n) ->
            console.print("${n}")
            n

pub fn finish_bool(console: Console, own ticket: Ticket) -> Bool:
    finish(console, ticket) > 0
"#;

#[test]
fn lazy_resource_effects_execute_only_on_selected_edges() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    // Local normal/opt, normal calling opt, and normal consuming opt-defined data.
    for (mode, imported, opt_call) in [
        ("", false, false), ("mode opt\n", false, false),
        ("", true, true), ("", true, false),
    ] {
        let prefix = if opt_call { "fast." } else { "" };
        let type_prefix = if imported { "fast." } else { "" };
        let normal_consumer = format!("import fast\npub fn finish{}", RESOURCE
            .split_once("pub fn finish").expect("resource helper").1
            .replace("ticket: Ticket", "ticket: fast.Ticket")
            .replace("Ticket(n)", "fast.Ticket(n)"));
        let declarations = if opt_call { "import fast\n" }
            else if imported { &normal_consumer } else { RESOURCE };
        let main = format!(r#"{mode}{declarations}
fn main(console: Console) -> Int:
    let ticket = {type_prefix}Ticket(7)
    let first = Some({prefix}finish(console, ticket)) ?? {prefix}finish(console, {type_prefix}Ticket(90))
    let second = None ?? {prefix}finish(console, {type_prefix}Ticket(8))
    let third = false && {prefix}finish_bool(console, {type_prefix}Ticket(91))
    let fourth = true || {prefix}finish_bool(console, {type_prefix}Ticket(92))
    let fifth = true && {prefix}finish_bool(console, {type_prefix}Ticket(9))
    first + second + if fifth: 9 else: 0
"#);
        let fast = format!("mode opt\n{RESOURCE}");
        let mut sources = vec![("main".into(), main.into_bytes())];
        if imported { sources.push(("fast".into(), fast.into_bytes())); }
        let program = witchy_types::pipeline::link_checked_from_source_bundle(
            sources, "main", witchy_interp::comptime::expand_compile_time,
        ).expect("source check");
        let artifact = witchy::codegen::compile_checked_module_artifact(&program)
            .expect_lowered("unoptimized canonical SafeWIR admission");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() },
            128,
        ).expect("admitted Wasm instance");
        vm.run().expect("admitted Wasm execution");
        assert_eq!(vm.output(), vec!["7", "8", "9", "24"]);
        assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
    }
}
