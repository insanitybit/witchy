//! Admitted Console slices only. No legacy mint import or ambient Console grant.
use std::any::Any;
use std::sync::Arc;
use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted, Store};
use witchy_test_host::{HostRequest, HostResponse};
use witchy_testkit::{SourceLocation, U64Text};
use super::super::{VmState, memory_of};

pub(in crate::runtime) struct Session;
struct ConsoleToken { issuer: Arc<Session>, kind: u32, write: bool }

fn authenticate(session: Option<&Arc<Session>>, value: &dyn Any) -> Result<()> {
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

fn text(data: &[u8], pointer: i32, length: i32) -> Result<String> {
    let start = pointer as u32 as usize;
    let end = start.checked_add(length as u32 as usize)
        .ok_or_else(|| Error::msg("fixture Console slice overflow"))?;
    let bytes = data.get(start..end).ok_or_else(|| Error::msg("fixture Console slice out of bounds"))?;
    String::from_utf8(bytes.to_vec()).map_err(|_| Error::msg("fixture Console slice is not UTF-8"))
}

pub(in crate::runtime) fn link(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "console_print_at", print_at)?;
    Ok(())
}

fn print_at(
    mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>,
    message_ptr: i32, message_len: i32, function_ptr: i32, function_len: i32, line: i32,
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_console_fixture_tokens_are_kind_rights_and_session_checked() {
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
    #[test]
    fn canonical_console_fixture_slices_refuse_overflow_oob_and_invalid_utf8() {
        assert_eq!(text(b"hello", 1, 3).unwrap(), "ell");
        for (pointer, length) in [(-1, 1), (0, -1), (4, 2)] {
            assert!(text(b"hello", pointer, length).is_err());
        }
        assert!(text(&[255], 0, 1).is_err());
    }
}
