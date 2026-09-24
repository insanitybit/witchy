//! RFC-0167 section 10: checked facts retained for *future* optimization.
//!
//! This is deliberately a fact-preservation matrix, not a performance suite:
//! no optional pass is enabled or claimed here.  Each positive assertion reads
//! a production checked product and each near miss either loses the exact fact
//! or reaches the production fail-closed boundary.  The remaining section-10
//! rows are not reconstructed here: today there is no public checked carrier
//! joining frozen with effects, bounds with owner generations, or discard with
//! effects/termination. Async cancellation and one-time state edges are bound
//! by the canonical `AsyncProgram` witness below.

use witchy::ast::{Block, Expr, Function, Item, Pattern, Stmt};
use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_lower::analysis::{call_ownership_fact, last_use_drops, Summaries};
use witchy_canonical_source::access::AccessQualifier;
use witchy_types::async_program::AsyncProgram;
use witchy_canonical_source::access::CheckedPlaceStep;
use witchy_types::async_readiness::AsyncReadinessError;
use witchy_types::optimization_readiness::{CheckedCallEffects, OptimizationReadinessError};
use witchy_types::typeck::{RegionMustDisposition, RegionPromotionFact, RegionPromotionQualifier};
use witchy_types::typed_async_lowering::{TypedAsyncOperationKind, TypedAsyncQueueBinding};
use witchy_types::typed_execution::{
    ExecutionShapeId, PayloadOwnership, QueueCapacity, QueueShape, QueueShapeId,
};

fn checked(source: &str) -> witchy_types::pipeline::CheckedModule {
    witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("source must produce a checked module: {error}"))
}

fn function<'a>(module: &'a witchy::ast::Module, name: &str) -> &'a Function {
    module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function)
                if function.name == name
                    || function.name.strip_suffix(name).is_some_and(|prefix| prefix.ends_with('.')) =>
            {
                Some(function)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("checked module has function `{name}`"))
}

fn direct_call_arguments(function: &Function) -> &[Expr] {
    function
        .body
        .stmts
        .iter()
        .find_map(|statement| match statement {
            Stmt::Expr(Expr::Call { args, .. }) => Some(args.as_slice()),
            _ => None,
        })
        .expect("fixture has one direct call statement")
}

fn direct_call<'a>(function: &'a Function, expected_callee: &str) -> &'a Expr {
    function
        .body
        .stmts
        .iter()
        .find_map(|statement| {
            let expression = match statement {
                Stmt::Expr(expression) | Stmt::Let { value: expression, .. } => expression,
                _ => return None,
            };
            matches!(expression, Expr::Call { name, .. }
                if name == expected_callee
                    || name.strip_suffix(expected_callee)
                        .is_some_and(|prefix| prefix.ends_with('.')))
            .then_some(expression)
        })
        .unwrap_or_else(|| panic!("checked function has a direct `{expected_callee}` call"))
}

fn discard_expression(function: &Function) -> &Expr {
    function
        .body
        .stmts
        .iter()
        .find_map(|statement| match statement {
            Stmt::Let { name, value, .. } if name == "_" => Some(value),
            Stmt::LetPattern { pattern: Pattern::Wildcard, value } => Some(value),
            _ => None,
        })
        .expect("fixture has one explicit discard")
}

fn binding_value<'a>(function: &'a Function, expected_name: &str) -> &'a Expr {
    function
        .body
        .stmts
        .iter()
        .find_map(|statement| match statement {
            Stmt::Let { name, value, .. } if name == expected_name => Some(value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("fixture has binding `{expected_name}`"))
}

fn assert_canonical_oracle_and_wasm(source: &str, expected: &[&str]) {
    let checked = checked(source);
    let expected = expected.iter().map(|line| (*line).to_owned()).collect::<Vec<_>>();
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("readiness witness reaches admitted canonical Wasm");
    let entry = artifact.entry().expect("canonical readiness entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry
            .export()
            .strip_prefix('f')
            .expect("numeric entry export")
            .parse()
            .expect("numeric entry function"),
    );
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .expect("readiness witness validates for the canonical oracle");
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability)
        .collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated,
        function,
        arguments,
        Default::default(),
    )
    .expect("readiness witness runs in the canonical interpreter");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter heap audit");

    let mut runtime = Runtime::batch().expect("canonical runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, print_int: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("readiness witness spawns admitted Wasm");
    vm.run().expect("readiness witness runs in admitted Wasm");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

fn region_block<'a>(function: &'a Function, expected_name: &str) -> &'a Block {
    assert_eq!(
        function.name, expected_name,
        "fixture must inspect the intended canonical function"
    );
    let block = function
        .body
        .stmts
        .iter()
        .find_map(|statement| match statement {
            Stmt::Let { value: Expr::Block(block), .. }
            | Stmt::Expr(Expr::Block(block))
            | Stmt::Return(Some(Expr::Block(block)))
                if block.region.is_some() =>
            {
                Some(block)
            }
            _ => None,
        })
        .expect("fixture has one direct region expression");
    assert!(
        function.body.stmts.iter().filter(|statement| {
            matches!(
                statement,
                Stmt::Let { value: Expr::Block(block), .. }
                    | Stmt::Expr(Expr::Block(block))
                    | Stmt::Return(Some(Expr::Block(block)))
                    if block.region.is_some()
            )
        }).count() == 1,
        "fixture must have exactly one direct region expression"
    );
    block
}

#[test]
fn own_unique_and_last_use_publish_distinct_checked_facts() {
    let ownership = checked(
        r#"mode opt

fn relay(own values: unique List(Int)) -> unique List(Int):
    values

fn rewrite(var values: unique List(Int)):
    values[0] = 9

fn main():
    var values: unique List(Int) = [1]
    values = relay(values)
    rewrite(values)
"#,
    );
    let source = ownership.canonical_source().expect("raw source retains canonical checked facts");
    let access = source.access_facts().expect("exact checked access facts");
    let call_facts = access.call_contracts().map(|(_, signature)| call_ownership_fact(signature)).collect::<Vec<_>>();

    assert!(
        call_facts.iter().any(|fact| {
            fact.own_unique_params() == [0]
                && fact.unique_params_to_check() == [0]
                && fact.unique_ownership_result()
        }),
        "an own unique relay must retain both input and result ownership facts: {call_facts:#?}",
    );
    assert!(
        call_facts.iter().any(|fact| {
            fact.no_copy_var_params() == [0]
                && fact.unique_params_to_check() == [0]
        }),
        "a var unique call must retain its no-copy input fact: {call_facts:#?}",
    );

    let last_use = checked(
        r#"
import list

fn f() -> Int:
    let buf: List(Int) = list.with_capacity(1)
    list.length(buf)

fn main():
    let _ = f()
"#,
    );
    let summaries = Summaries::of_module(last_use.module());
    let f = function(last_use.module(), "f");
    let call = f.body.stmts.last().expect("single-use call statement");
    let drops = last_use_drops(&f.body, &summaries);
    assert!(
        drops.drops_after(call).iter().any(|drop| drop.name == "buf"),
        "the nonescaping single use must remain a precise last-use drop fact",
    );

    let rejection = witchy::resolve_std_only_checked(
        r#"mode opt

fn rewrite(var values: unique List(Int)):
    values[0] = 9

fn main():
    var values = [1]
    let alias = values
    rewrite(values)
    let _ = alias
"#,
    )
    .expect_err("an alias must invalidate an opt unique no-copy proof")
    .to_string();
    assert!(
        rejection.contains("unique") || rejection.contains("no-copy") || rejection.contains("mode opt"),
        "the near miss must fail rather than fabricate a unique fact: {rejection}",
    );
}

#[test]
fn rooted_views_and_disjoint_places_remain_exact_and_dynamic_overlap_refuses() {
    let view_fixture = checked(
        r#"mode opt

fn first(text: &String) -> &String:
    text

fn main(console: Console):
    var text = "value"
    let view = first(&text)
    console.print(*view)
"#,
    );
    let source = view_fixture.canonical_source().expect("canonical checked source");
    assert!(
        source.loan_facts().is_err(),
        "canonical rooted views never reconstruct the retired lexical-loan graph",
    );
    let views = source.typed_module().view_facts().expect("authenticated rooted-view facts");
    assert!(
        views.iter().any(|view| view.place().root() == "text"),
        "rooted view formation must retain its exact owner root",
    );

    let places = checked(
        r#"
type Pair:
    left: Int
    right: Int

fn write(var left: Int, var right: Int):
    left = 1
    right = 2

fn main():
    var pair = Pair(0, 0)
    write(pair.left, pair.right)
"#,
    );
    let source = places.canonical_source().expect("canonical checked source");
    let typed = source.typed_module();
    let access = source.access_facts().expect("exact checked place graph");
    let arguments = direct_call_arguments(function(typed.module(), "main"));
    let left = access.place_at(typed.module(), &arguments[0]).expect("left checked place");
    let right = access.place_at(typed.module(), &arguments[1]).expect("right checked place");
    assert_eq!(left.root(), "pair");
    assert_eq!(right.root(), "pair");
    assert_eq!(left.steps(), &[CheckedPlaceStep::Field("left".into())]);
    assert_eq!(right.steps(), &[CheckedPlaceStep::Field("right".into())]);
    assert!(!left.overlaps(right), "distinct fixed projections preserve disjointness");

    let rejection = witchy::resolve_std_only_checked(
        r#"
fn write(var left: Int, var right: Int):
    left = 1
    right = 2

fn dynamic(i: Int):
    var values = [0, 0]
    write(values[i], values[i])

fn main():
    dynamic(0)
"#,
    )
    .expect_err("dynamic overlapping write-back places must fail closed")
    .to_string();
    assert!(
        rejection.contains("overlap") || rejection.contains("alias") || rejection.contains("var"),
        "dynamic places must not receive a false disjointness fact: {rejection}",
    );
}

#[test]
fn region_promotions_retain_exact_layout_owner_and_fresh_must_descriptors() {
    let packed = checked(
        r#"mode opt

type Point packed:
    x: Int
    y: Int

fn main():
    let point: Point = region -> Point:
        Point(40, 2)
    let _ = point
"#,
    );
    let source = packed.canonical_source().expect("canonical checked source");
    let typed = source.typed_module();
    let block = region_block(function(typed.module(), "main"), "main");
    match typed.table().region_promotion(block) {
        Some(RegionPromotionFact::PackedInline { descriptor, .. }) => {
            assert_eq!(descriptor.record, "main.Point");
            assert_eq!(descriptor.fields.len(), 2);
        }
        other => panic!("packed region must retain its exact descriptor, got {other:#?}"),
    }

    let parent = checked(
        r#"mode opt

fn main():
    let parent = "parent-owned"
    let value: String = region -> String:
        parent
    let _ = value
"#,
    );
    let source = parent.canonical_source().expect("canonical checked source");
    let typed = source.typed_module();
    let block = region_block(function(typed.module(), "main"), "main");
    match typed.table().region_promotion(block) {
        Some(RegionPromotionFact::OptParentPassThrough { owner_root, qualifier, .. }) => {
            assert_eq!(owner_root, "parent");
            assert_eq!(*qualifier, RegionPromotionQualifier::Ordinary);
        }
        other => panic!("parent pass-through must retain its owner proof, got {other:#?}"),
    }

    let must = checked(
        r#"
must type Ticket:
    Ticket(Int)

fn make() -> Ticket:
    region -> Ticket:
        Ticket(42)

fn finish(own ticket: Ticket):
    let _ = 0

fn main():
    finish(make())
"#,
    );
    let source = must.canonical_source().expect("canonical checked source");
    let typed = source.typed_module();
    let block = region_block(function(typed.module(), "make"), "main.make");
    match typed.table().region_promotion(block) {
        Some(RegionPromotionFact::NormalOwnedSnapshot {
            must_disposition: RegionMustDisposition::FreshConstructor(transfer),
            ..
        }) => assert_eq!(transfer.constructor, "main.Ticket"),
        other => panic!("must region must retain its exact fresh transfer, got {other:#?}"),
    }

    let near_miss = checked(
        r#"mode opt

fn main():
    let value: String = region -> String:
        "must-not-hide" + "-copy"
    let _ = value
"#,
    );
    let refusal = codegen::compile_checked_module_artifact(&near_miss)
        .expect_unsupported("opt region without copy-free proof");
    assert!(
        refusal.message.contains("copy-free") && refusal.message.contains("region"),
        "canonical lowering must refuse a missing promotion proof: {}",
        refusal.message,
    );
}

#[test]
fn frozen_places_and_callable_effect_contract_share_one_checked_identity() {
    let checked = checked(
        r#"
pure fn snapshot(value: frozen String) -> frozen String:
    value

fn invoke(callback: fn(frozen String) -> frozen String, value: frozen String) -> frozen String:
    callback(value)

fn main():
    let direct = snapshot("direct")
    let indirect = invoke(snapshot, direct)
    let _ = indirect
"#,
    );
    let source = checked.canonical_source().expect("canonical checked source");
    let typed = source.typed_module();
    let access = source.access_facts().expect("checked callable facts");

    let direct = direct_call(function(typed.module(), "main"), "snapshot");
    let direct_signature = access.call_at(typed.module(), direct).expect("direct call signature");
    assert!(direct_signature.callable_contract().pure);
    assert!(direct_signature.params()[0].qualifiers().contains(&AccessQualifier::Frozen));
    assert!(direct_signature.result().qualifiers().contains(&AccessQualifier::Frozen));

    let indirect = direct_call(function(typed.module(), "invoke"), "callback");
    let indirect_signature = access.call_at(typed.module(), indirect).expect("indirect call signature");
    assert!(
        !indirect_signature.callable_contract().pure,
        "an ordinary function-value boundary must not inherit the concrete pure target's effect promise",
    );
    assert!(indirect_signature.params()[0].qualifiers().contains(&AccessQualifier::Frozen));
    assert!(indirect_signature.result().qualifiers().contains(&AccessQualifier::Frozen));
    assert_ne!(
        direct_signature.identity_key(),
        indirect_signature.identity_key(),
        "effect weakening must change the same identity that carries frozen places",
    );
}

#[test]
fn explicit_discard_preserves_effects_and_cannot_erase_affine_obligations() {
    assert_canonical_oracle_and_wasm(
        r#"
fn observe(console: Console) -> Int:
    console.print("effect-retained")
    42

fn main(console: Console):
    let _ = observe(console)
"#,
        &["effect-retained"],
    );

    let rejection = witchy::resolve_std_only_checked(
        r#"
must type Ticket:
    Ticket(Int)

fn make() -> Ticket:
    Ticket(1)

fn main():
    let _ = make()
"#,
    )
    .expect_err("explicit discard cannot discharge a must-consume result")
    .to_string();
    assert!(
        rejection.contains("must")
            && (rejection.contains("discard") || rejection.contains("erase")),
        "affine discard near miss must fail at the checked boundary: {rejection}",
    );
}

#[test]
fn readiness_carrier_binds_frozen_effects_discards_and_exact_call_identity() {
    let module = checked(
        r#"
pure fn snapshot(value: frozen String) -> frozen String:
    value

fn identity(value: a) -> a:
    value

fn invoke(callback: fn(frozen String) -> frozen String, value: frozen String) -> frozen String:
    callback(value)

fn observe(console: Console) -> Int:
    console.print("retained")
    42

fn main(console: Console):
    let direct = snapshot("direct")
    let generic = identity(direct)
    let indirect = invoke(snapshot, generic)
    let _ = observe(console)
    let _ = indirect
"#,
    );
    let source = module.canonical_source().expect("canonical checked source");
    let typed = source.typed_module();
    let readiness = source.optimization_readiness();

    let direct = binding_value(function(typed.module(), "main"), "direct");
    let generic = binding_value(function(typed.module(), "main"), "generic");
    let indirect = direct_call(function(typed.module(), "invoke"), "callback");
    let discard = discard_expression(function(typed.module(), "main"));

    let direct_facts = readiness.call_at(source, direct).expect("direct source identity is exact");
    assert_eq!(direct_facts.effects(), CheckedCallEffects::Pure);
    assert_eq!(direct_facts.frozen_parameters(), &[0]);
    assert!(direct_facts.frozen_result());

    let generic_facts = readiness.call_at(source, generic).expect("generic call is checked at its concrete use");
    assert_eq!(generic_facts.effects(), CheckedCallEffects::MayHaveObservableEffects);
    assert!(generic_facts.frozen_parameters().is_empty());

    let indirect_facts = readiness.call_at(source, indirect).expect("indirect call keeps exact declared callable identity");
    assert_eq!(indirect_facts.effects(), CheckedCallEffects::MayHaveObservableEffects);
    assert_eq!(indirect_facts.frozen_parameters(), &[0]);
    assert!(indirect_facts.frozen_result());

    let discard_facts = readiness.discard_at(source, discard).expect("discard remains source-addressed");
    assert_eq!(discard_facts.effects(), CheckedCallEffects::MayHaveObservableEffects);
    assert!(discard_facts.may_trap() && discard_facts.may_diverge());
    assert!(discard_facts.no_live_must_obligation());
    assert!(!discard_facts.permits_elision(), "pure is never a totality proof and discards retain traps/effects");

    let stale = checked(
        r#"
pure fn snapshot(value: frozen String) -> frozen String:
    value

fn main():
    let _ = snapshot("second")
"#,
    );
    let stale_source = stale.canonical_source().expect("independent checked source");
    let stale_call = discard_expression(function(stale_source.typed_module().module(), "main"));
    assert_eq!(
        readiness.call_at(stale_source, stale_call).unwrap_err(),
        OptimizationReadinessError::StaleCheckedSource,
        "facts cannot cross a checked-source allocation even when surface syntax matches",
    );
}

#[test]
fn bounds_are_revalidated_after_the_owner_is_mutated() {
    assert_canonical_oracle_and_wasm(
        r#"mode opt

fn main(console: Console):
    var values = [10, 20]
    console.print("${values[1]}")
    values.push(30)
    console.print("${values[2]}")
"#,
        &["20", "30"],
    );

    let rejection = witchy::resolve_std_only_checked(
        r#"mode opt

fn read(values: List(Int), index: Int) -> Int:
    values[index]

fn main():
    let _ = read([1], 9)
"#,
    )
    .expect("a dynamic index remains a checked source program");
    let artifact = codegen::compile_checked_module_artifact(&rejection)
        .expect_lowered("dynamic bounds remain explicit in admitted canonical IR");
    let mut runtime = Runtime::batch().expect("canonical runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities::none(), 256)
        .expect("out-of-bounds witness spawns admitted Wasm");
    assert!(vm.run().is_err(), "the retained bounds guard must trap instead of becoming a range fact");
}

#[test]
fn async_program_binds_cancellation_and_one_time_edges_into_its_fingerprint() {
    let checked = checked(
        r#"
from chan import Receiver

async fn receive_one(rx: Receiver(Int)) -> Option(Int):
    chan.recv(rx).await

fn main():
    let root = 1
    let view = &root
    ()
"#,
    );
    let mut input = checked
        .typed_async_lowering_input()
        .expect("checked source retains typed async lowering input");
    let receive = input
        .operations
        .iter()
        .find(|operation| operation.runtime_kind == TypedAsyncOperationKind::ChannelReceive)
        .expect("fixture retains its source-addressed receive operation");
    input.queue_bindings.push(TypedAsyncQueueBinding {
        endpoint: receive.operand_bindings[0],
        shape: QueueShape {
            id: QueueShapeId(0),
            capacity: QueueCapacity::Rendezvous,
            payload_layout_fingerprint: [0x67; 32],
            ownership: PayloadOwnership::Inline,
        },
    });
    let execution = input
        .into_linear_execution(ExecutionShapeId(0), [0x16; 32])
        .expect("typed queue operation lowers without replaying source syntax");
    let mut program = AsyncProgram::from_final_typed_executions(vec![execution])
        .expect("typed execution forms a canonical async program");
    let readiness = checked
        .checked_async_readiness(&input, &program)
        .expect("one checked product binds immutable rooted values/access, typed captures, and scheduling");
    assert_eq!(readiness.cancellation_count(), 1);
    assert!(readiness.uses_rooted_view_contract(),
        "canonical async readiness must retain the RFC-0167 rooted-value product, not recreate lexical loans");
    readiness
        .validate(&checked, &input, &program)
        .expect("the exact checked async artifacts remain queryable");
    let shape = &program.execution_shapes[0];
    let cancellation = shape
        .cancellation
        .expect("suspending receive retains its cancellation edge");
    assert!(
        shape.operations.iter().any(|operation| operation.id == cancellation.operation),
        "the cancellation edge names an exact retained operation",
    );
    assert!(
        shape.states.iter().any(|state| state.state == cancellation.state),
        "the cancellation edge names an exact retained state",
    );

    // A later transform cannot redirect cancellation or replay an earlier
    // state while retaining the authenticated semantic fingerprint.
    program.execution_shapes[0].cancellation = None;
    let rejection = program
        .validate()
        .expect_err("tampering with cancellation must invalidate canonical async identity");
    assert!(
        rejection.contains("fingerprint"),
        "the near miss must fail through the canonical identity check: {rejection}",
    );
    assert!(matches!(
        readiness.validate(&checked, &input, &program),
        Err(AsyncReadinessError::InvalidAsyncProgram(_))
            | Err(AsyncReadinessError::ProgramIdentityMismatch),
    ));
}

#[test]
fn async_readiness_refuses_stale_capture_identity_and_replayable_once_recipe() {
    let checked = checked(
        r#"
from chan import Receiver

async fn receive_one(rx: Receiver(Int)) -> Option(Int):
    chan.recv(rx).await

fn main():
    let root = 1
    let view = &root
    ()
"#,
    );
    let input = checked.typed_async_lowering_input().expect("typed async input");
    let mut stale = input.clone();
    let state = stale.states.iter_mut().find(|state| !state.frame.is_empty())
        .expect("receive continuation has an owned capture destination");
    state.frame[0].source_binding = None;

    let rejection = checked.checked_async_readiness(
        &stale,
        &AsyncProgram::new(Vec::new()).expect("empty program for mismatch probe"),
    ).unwrap_err();
    assert_eq!(rejection, AsyncReadinessError::TypedInputMismatch,
        "missing source capture identity must not be repaired from a generated frame name");

    let replay = witchy::resolve_std_only_checked(
        r#"
async fn worker(own callback: once fn() -> Int) -> Int:
    callback()

fn main():
    ()
"#,
    ).expect_err("a replayable async recipe cannot carry an affine once callback").to_string();
    assert!(
        replay.contains("closure environment captures affine once-callable")
            && replay.contains("pass it explicitly to an `own` parameter"),
        "the source boundary must reject before a readiness carrier could bless replay: {replay}");
}
