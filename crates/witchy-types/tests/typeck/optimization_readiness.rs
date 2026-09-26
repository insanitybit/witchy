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

use witchy_types::async_program::AsyncProgram;
use witchy_types::async_readiness::AsyncReadinessError;
use witchy_types::typed_async_lowering::{TypedAsyncOperationKind, TypedAsyncQueueBinding};
use witchy_types::typed_execution::{
    ExecutionShapeId, LaneAction, LaneOwnership, PayloadOwnership, QueueCapacity, QueueShape, QueueShapeId,
};
use super::source::checked_module;

fn checked(source: &str) -> witchy_types::pipeline::CheckedModule {
    checked_module(source)
        .unwrap_or_else(|error| panic!("source must produce a checked module: {error}"))
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
    let mut unproduced = execution.execution.clone();
    let result = unproduced.result_lanes[0];
    unproduced.frame[result.0 as usize].ownership = LaneOwnership::MustConsume;
    let unproduced_error = unproduced.validate().unwrap_err();
    assert!(unproduced_error.contains("produced must-consume result"), "{unproduced_error}");

    let mut undisposed_on_cancel = execution.execution.clone();
    undisposed_on_cancel.frame[0].ownership = LaneOwnership::MustConsume;
    undisposed_on_cancel.frame[result.0 as usize].ownership = LaneOwnership::MustConsume;
    undisposed_on_cancel.states[1].transitions[0].actions.push(LaneAction::Move {
        from: undisposed_on_cancel.frame[0].id,
        to: result,
    });
    let cancellation_error = undisposed_on_cancel.validate().unwrap_err();
    assert!(cancellation_error.contains("undisposed"), "{cancellation_error}");
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

    let replay = checked_module(
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
