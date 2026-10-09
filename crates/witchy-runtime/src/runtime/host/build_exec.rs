//! Build-only tool execution from an instance-bound map of confined files.
//! Guest strings select explicit entries; they never become ambient executables.
use crate::runtime::GuestAddress;
use std::{collections::BTreeMap, sync::Arc};
use wasmtime::{bail, Caller, Error, ExternRef, Linker, Result, Rooted};
use super::super::{memory_of, read_slice_str, VmState};

#[derive(Clone, Debug)]
pub(in crate::runtime) struct Authority {
    issuer: Arc<()>,
    execute: bool,
    tools: Arc<BTreeMap<String, crate::confine::ConfinedFile>>,
}

pub(in crate::runtime) fn mint(state: &VmState) -> Result<Authority> {
    let allow = state.caps.exec_allow.as_ref()
        .ok_or_else(|| Error::msg("build step demands `BuildExec` but no tool grant was provided"))?;
    let mut tools = BTreeMap::new();
    for name in allow {
        let file = state.build_exec_tools.get(name).ok_or_else(|| Error::msg(format!(
            "run_tool: granted tool `{name}` has no resolved confined executable"
        )))?;
        tools.insert(name.clone(), file.clone());
    }
    // Some(empty) is an explicit grant with no selectable tools. Extra resolved
    // files not named by this grant cannot widen the token's authority.
    Ok(Authority {
        issuer: state.build_exec_issuer.clone(), execute: true, tools: Arc::new(tools),
    })
}

fn authenticate<'a>(issuer: &Arc<()>, data: &'a dyn std::any::Any) -> Result<&'a Authority> {
    let authority = data.downcast_ref::<Authority>()
        .ok_or_else(|| Error::msg("run_tool: wrong capability kind"))?;
    if !Arc::ptr_eq(issuer, &authority.issuer) {
        bail!("run_tool: capability belongs to another instance");
    }
    if !authority.execute { bail!("run_tool: capability lacks execute right"); }
    Ok(authority)
}

pub(in crate::runtime) fn authenticate_capture(state: &VmState, data: &dyn std::any::Any) -> Result<()> {
    authenticate(&state.build_exec_issuer, data).map(|_| ())
}

pub(in crate::runtime) fn link<P: GuestAddress>(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "build_exec_run", run::<P>)?;
    Ok(())
}

fn run<P: GuestAddress>(
    mut caller: Caller<'_, VmState>, grant: Option<Rooted<ExternRef>>,
    tool_ptr: P, tool_len: i32, input_ptr: P, input_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let grant = grant.ok_or_else(|| Error::msg("run_tool: null capability"))?;
    let data = grant.data(&caller)?.ok_or_else(|| Error::msg("run_tool: missing capability data"))?;
    let authority = authenticate(&caller.data().build_exec_issuer, data)?.clone();
    // Both full slices are checked, copied, and decoded before any subprocess
    // effect. No guest reference, pointer, or PATH lookup crosses into run().
    let memory = memory_of(&mut caller)?;
    let tool = read_slice_str(memory.data(&caller), tool_ptr, tool_len)?;
    let input = read_slice_str(memory.data(&caller), input_ptr, input_len)?;
    let executable = authority.tools.get(&tool).ok_or_else(|| Error::msg(format!(
        "run_tool: `{tool}` is not in this BuildExec grant's allow-list"
    )))?;
    let output = executable.run(&[], &input)
        .map_err(|error| Error::msg(format!("run_tool: cannot start `{tool}`: {error}")))?;
    if !output.status.success() {
        bail!("run_tool: `{tool}` exited with {}", output.status);
    }
    // Preserve run_tool's stdout-only lossy decoding, not ordinary Exec's
    // status/stdout/stderr envelope. Size/copy never execute or read again.
    let stdout = String::from_utf8_lossy(&output.stdout);
    super::build_read::new_response(&mut caller, Some(stdout.as_ref())).map(Some)
}

#[cfg(feature = "self-checks")]
#[doc(hidden)]
#[path = "build_exec_tests.rs"]
pub mod tests;
