//! Admitted Console slices only. No legacy mint import or ambient Console grant.
use crate::runtime::GuestAddress;
use std::any::Any;
use std::sync::Arc;
use wasmtime::{Caller, Error, ExternRef, Result, Rooted, Store};
use witchy_test_host::{HostRequest, HostResponse};
use witchy_testkit::{SourceLocation, U64Text};
use super::super::{VmState, memory_of};

pub(in crate::runtime) struct Session;
struct ConsoleToken { issuer: Arc<Session>, kind: u32, write: bool }

pub(super) fn authenticate(session: Option<&Arc<Session>>, value: &dyn Any) -> Result<()> {
    let token = value.downcast_ref::<ConsoleToken>()
        .ok_or_else(|| Error::msg("fixture Console token has wrong kind"))?;
    if token.kind != 0 { return Err(Error::msg("fixture Console token has wrong kind")); }
    if !session.is_some_and(|session| Arc::ptr_eq(session, &token.issuer)) {
        return Err(Error::msg("fixture Console token belongs to another session"));
    }
    if !token.write { return Err(Error::msg("fixture Console token lacks Write")); }
    Ok(())
}

pub(in crate::runtime) fn mint(store: &mut Store<VmState>) -> Result<Rooted<ExternRef>> {
    let issuer = store.data().fixture_console.clone()
        .ok_or_else(|| Error::msg("fixture Console session missing"))?;
    if !store.data().fixture_host.as_ref().is_some_and(|host| host.roots().console) {
        return Err(Error::msg("admitted Console fixture: missing Console provider"));
    }
    ExternRef::new(store, ConsoleToken { issuer, kind: 0, write: true })
}

fn text<P: GuestAddress>(data: &[u8], pointer: P, length: i32) -> Result<String> {
    let start = pointer.offset();
    let end = start.checked_add(length as u32 as usize)
        .ok_or_else(|| Error::msg("fixture Console slice overflow"))?;
    let bytes = data.get(start..end).ok_or_else(|| Error::msg("fixture Console slice out of bounds"))?;
    String::from_utf8(bytes.to_vec()).map_err(|_| Error::msg("fixture Console slice is not UTF-8"))
}

pub(super) fn print_at<P: GuestAddress>(
    mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>,
    message_ptr: P, message_len: i32, function_ptr: P, function_len: i32, line: i32,
) -> Result<()> {
    let token = token.ok_or_else(|| Error::msg("fixture Console token is null"))?;
    let payload = token.data(&caller)?.ok_or_else(|| Error::msg("fixture Console token has no payload"))?;
    // Authenticate before reading even the source-label slice or invoking a provider.
    authenticate(caller.data().fixture_console.as_ref(), payload)?;
    let memory = memory_of(&mut caller)?;
    let message = text(memory.data(&caller), message_ptr, message_len)?;
    let message = message.trim_end_matches('\n').to_owned();
    let function = text(memory.data(&caller), function_ptr, function_len)?;
    let line = line as u32;
    let source = (line != 0).then(|| SourceLocation {
        module: function.rsplit_once('.').map_or(function.as_str(), |(module, _)| module).to_owned(),
        line: U64Text::new(u64::from(line)), column: U64Text::new(1),
    });
    let host = caller.data_mut().fixture_host.as_mut()
        .ok_or_else(|| Error::msg("fixture Console provider missing"))?;
    let response = host.invoke(HostRequest::ConsoleWrite { text: message.clone() }, source)
        .map_err(|failure| Error::msg(witchy_syntax::diag::runtime_error(
            &function, line, &format!("fixture {:?}: {}", failure.code, failure.message),
        )))?;
    if !matches!(response, HostResponse::Unit) {
        return Err(Error::msg("fixture Console write returned a non-Unit response"));
    }
    caller.data().output.lock().unwrap().push(message);
    Ok(())
}

#[cfg(feature = "self-checks")]
#[doc(hidden)]
pub mod tests {
    use super::*;
    pub fn canonical_console_fixture_capture_checks_the_active_store_session_and_rights() -> Result<()> {
        use super::super::closure_owner::check_fixture_capture_for_test;
        use super::super::super::{Capabilities, ConsoleCapability, Runtime, vmstate_from_caps};
        use witchy_wir::canonical_safewir::CapabilityKindId;
        let runtime = Runtime::batch()?;
        let engine = runtime.engine.clone();
        // Inert bookkeeping for a host-only Store test: no Wasm instance is
        // created or executed. Actual capture execution uses admitted source.
        let module = wasmtime::Module::new(&engine, "(module)")?;
        let mut state = vmstate_from_caps(0, &Capabilities::none(),
            wasmtime::StoreLimitsBuilder::new().build(), None, &engine, &module,
            runtime.preempt, runtime.compiler_services.clone())?;
        let first = Arc::new(Session);
        state.fixture_console = Some(first.clone());
        state.fixture_host = Some(witchy_test_host::FixtureHost::new(witchy_testkit::FixturePlan {
            console: Some(Default::default()), ..Default::default()
        }).map_err(|error| Error::msg(error.to_string()))?);
        let mut store = Store::new(&engine, state);
        let kind = CapabilityKindId::new(0);
        let valid = mint(&mut store)?;
        check_fixture_capture_for_test(&store, valid, kind)?;
        let foreign = ExternRef::new(&mut store, ConsoleToken { issuer: Arc::new(Session), kind: 0, write: true })?;
        assert!(check_fixture_capture_for_test(&store, foreign, kind).unwrap_err().to_string().contains("another session"));
        let wrong = ExternRef::new(&mut store, ConsoleToken { issuer: first.clone(), kind: 1, write: true })?;
        assert!(check_fixture_capture_for_test(&store, wrong, kind).unwrap_err().to_string().contains("wrong kind"));
        let read_only = ExternRef::new(&mut store, ConsoleToken { issuer: first, kind: 0, write: false })?;
        assert!(check_fixture_capture_for_test(&store, read_only, kind).unwrap_err().to_string().contains("Write"));
        let arbitrary = ExternRef::new(&mut store, 7_i32)?;
        let production = ExternRef::new(&mut store, ConsoleCapability)?;
        for token in [arbitrary, production] {
            assert!(check_fixture_capture_for_test(&store, token, kind).is_err(), "no production or arbitrary-token fallback in a fixture session");
        }
        assert!(check_fixture_capture_for_test(&store, valid, CapabilityKindId::new(1)).is_err());
        store.data_mut().fixture_console = None;
        assert!(check_fixture_capture_for_test(&store, valid, kind).is_err(), "fixture token grants nothing outside its session");
        check_fixture_capture_for_test(&store, production, kind)?;
        Ok(())
    }

    pub fn canonical_console_fixture_tokens_are_kind_rights_and_session_checked() {
        let first = Arc::new(Session);
        let second = Arc::new(Session);
        let token = ConsoleToken { issuer: first.clone(), kind: 0, write: true };
        authenticate(Some(&first), &token).unwrap();
        assert!(authenticate(Some(&second), &token).unwrap_err().to_string().contains("another session"));
        assert!(authenticate(None, &token).is_err());
        assert!(authenticate(Some(&first), &0u64).unwrap_err().to_string().contains("wrong kind"));
        let wrong = ConsoleToken { issuer: first.clone(), kind: 1, write: true };
        assert!(authenticate(Some(&first), &wrong).unwrap_err().to_string().contains("wrong kind"));
        let read_only = ConsoleToken { issuer: first.clone(), kind: 0, write: false };
        assert!(authenticate(Some(&first), &read_only).unwrap_err().to_string().contains("Write"));
    }
    pub fn canonical_console_fixture_slices_refuse_overflow_oob_and_invalid_utf8() {
        assert_eq!(text(b"hello", 1, 3).unwrap(), "ell");
        for (pointer, length) in [(-1, 1), (0, -1), (4, 2)] {
            assert!(text(b"hello", pointer, length).is_err());
        }
        assert!(text(&[255], 0, 1).is_err());
    }
}
