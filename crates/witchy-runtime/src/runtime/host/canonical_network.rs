//! Canonical Net adapters. The existing host owns policy, sockets and TLS.
use super::*;
use super::super::super::{fill_slice, read_slice_str};

pub(super) fn link_listen(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "net_listen_at", listen_at)?;
    Ok(())
}

pub(super) fn link_socket_io(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "net_send_slice", send_slice)?;
    linker.func_wrap("witchy", "net_recv_line_size", recv_line_size)?;
    linker.func_wrap("witchy", "net_recv_bytes_size", recv_bytes_size)?;
    linker.func_wrap("witchy", "net_take_received_into", take_received_into)?;
    Ok(())
}

fn listen_at(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, ptr: i32, len: i32) -> Result<Option<Rooted<ExternRef>>> {
    let net = net_authority_ref(&caller, token)?;
    let memory = memory_of(&mut caller)?;
    let address = read_slice_str(memory.data(&caller), ptr, len)?;
    let (listener, tls) = match caller.data().worker_listener.clone() {
        Some(entry) if worker_listener_matches(&entry, &net.allow, &address, false) => entry,
        _ => (Arc::new(crate::net::bind_admitted_listener(&net.allow, &address).map_err(Error::msg)?), None),
    };
    listener_ref(&mut caller, listener, tls)
}

fn send_slice(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, ptr: i32, len: i32) -> Result<()> {
    use std::io::Write;
    let socket = socket_resource_ref(&caller, token)?;
    let memory = memory_of(&mut caller)?;
    let text = read_slice_str(memory.data(&caller), ptr, len)?;
    lock_socket(&socket)?.get_mut().write_all(text.as_bytes()).map_err(Error::msg)
}

fn pending(socket: &SocketResource) -> Result<std::sync::MutexGuard<'_, Option<Vec<u8>>>> {
    socket.canonical_pending.lock().map_err(|_| Error::msg("Net receive staging lock poisoned"))
}

fn recv_line_size(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    let socket = socket_resource_ref(&caller, token)?;
    let mut staged = pending(&socket)?;
    if staged.is_some() { bail!("Net receive must consume its previous result first"); }
    let bytes = crate::net::read_line_capped(&mut *lock_socket(&socket)?).map_err(Error::msg)?;
    let bytes = String::from_utf8_lossy(&bytes).trim_end_matches('\n').as_bytes().to_vec();
    let length = i32::try_from(bytes.len()).map_err(|_| Error::msg("Net receive exceeds ABI size"))?;
    *staged = Some(bytes);
    Ok(length)
}

fn recv_bytes_size(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, count: i64) -> Result<i32> {
    use std::io::Read;
    let socket = socket_resource_ref(&caller, token)?;
    let mut staged = pending(&socket)?;
    if staged.is_some() { bail!("Net receive must consume its previous result first"); }
    let want = usize::try_from(count.max(0)).map_err(|_| Error::msg("Net receive count exceeds host size"))?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut stream = lock_socket(&socket)?;
    while bytes.len() < want {
        let amount = (want - bytes.len()).min(chunk.len());
        match stream.read(&mut chunk[..amount]) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(error) if crate::net::is_read_timeout(&error) => break,
            Err(error) => return Err(Error::msg(error)),
        }
    }
    let bytes = String::from_utf8_lossy(&bytes).into_owned().into_bytes();
    let length = i32::try_from(bytes.len()).map_err(|_| Error::msg("Net receive exceeds ABI size"))?;
    *staged = Some(bytes);
    Ok(length)
}

fn take_received_into(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, ptr: i32, cap: i32) -> Result<i32> {
    let socket = socket_resource_ref(&caller, token)?;
    let mut staged = pending(&socket)?;
    let bytes = staged.as_ref().ok_or_else(|| Error::msg("this Socket has no pending Net receive"))?;
    if cap < 0 || bytes.len() > cap as usize { bail!("Net receive output capacity is too small"); }
    // fill_slice validates the WHOLE declared slice before touching memory.
    // A refusal leaves the pending receive available for a valid retry.
    let length = fill_slice(&mut caller, ptr, cap, bytes, "Net receive")?;
    *staged = None;
    Ok(length)
}
