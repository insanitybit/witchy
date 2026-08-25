//! WIR body capture for the future typed ready-channel executor.
//!
//! This module deliberately does not select a runtime path.  It establishes the
//! boundary that a later executor needs: a proved channel transition owns the
//! final task-building WIR node, while every earlier node remains the ordinary
//! state prefix and must execute exactly once before a direct probe or fallback.

use witchy_types::suspension_carrier::{
    ReadyChannelOperation, SuspensionCarrierCatalog,
};
use witchy_wir::wir::{WirModule, WirNode, WirSeq};

/// A compiler-owned state body split at its generated terminal task boundary.
///
/// `prefix` is cloned verbatim from the lowered WIR function.  A later emitter
/// may execute it, but may never rediscover or replay it from source.  Keeping
/// the original `terminal` node lets the first direct executor retain an exact
/// scheduler fallback until it can lower the transition itself.
#[derive(Debug, Clone)]
pub(super) struct ReadyStateWir {
    pub state: usize,
    pub path_ordinal: usize,
    pub function: String,
    pub prefix: WirSeq,
    pub terminal: WirNode,
    pub operations: Vec<ReadyChannelOperation>,
}

/// Extract WIR bodies only for individually formed, unguarded ready regions.
///
/// Formation has already rejected boxed state, dynamic endpoint/payload, and
/// guarded task construction.  This pass adds the independent WIR requirement:
/// the generated state must have one final value-producing task construction.
/// It refuses all other bodies rather than guessing a boundary from a nested
/// call, which could duplicate effects during a ready probe.
pub(super) fn extract(
    carrier: &SuspensionCarrierCatalog,
    module: &WirModule,
) -> Vec<ReadyStateWir> {
    carrier
        .ready_channel_regions()
        .regions
        .into_iter()
        .filter_map(|region| {
            let function = module
                .funcs
                .iter()
                .find(|function| function.name == region.state.function)?;
            let (terminal, prefix) = split_terminal_task_node(&function.body)?;
            Some(ReadyStateWir {
                state: region.id,
                path_ordinal: region.path_ordinal,
                function: function.name.clone(),
                prefix,
                terminal,
                operations: region.operations,
            })
        })
        .collect()
}

/// The async lowerer writes the task expression as the final statement of a
/// frame function. WIR's normal tail conversion represents that expression as
/// one final `Push` or `Return`; neither case is inferred from arbitrary nested
/// calls.  Any other final node stays on the scheduler.
fn split_terminal_task_node(body: &[WirNode]) -> Option<(WirNode, WirSeq)> {
    let (terminal, prefix) = body.split_last()?;
    match terminal {
        WirNode::Push(_) | WirNode::Return(Some(_)) => Some((terminal.clone(), prefix.to_vec())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_wir::wir::{WirExpr, WirNode};

    #[test]
    fn splits_only_a_final_value_boundary() {
        let body = vec![
            WirNode::Do(WirExpr::CallHost { import: "effect".into(), args: vec![] }),
            WirNode::Push(WirExpr::ConstI32(0)),
        ];
        let (terminal, prefix) = split_terminal_task_node(&body).expect("final task value");
        assert!(matches!(terminal, WirNode::Push(WirExpr::ConstI32(0))));
        assert_eq!(prefix.len(), 1, "the effectful prefix is retained verbatim");
        assert!(split_terminal_task_node(&[WirNode::Do(WirExpr::ConstI32(0))]).is_none());
    }
}
