//! Canonical BuildOut: explicit, instance-bound write authority and bounded
//! UTF-8 inputs. No guest pointer or path is ever a capability constructor.
use std::sync::Arc;
use wasmtime::{bail, Caller, ExternRef, Linker, Result, Rooted};
use super::super::{confine, memory_of, read_slice_str, VmState};

#[derive(Clone, Debug)]
pub(in crate::runtime) struct Authority {
    issuer: Arc<()>,
    write: bool,
    directory: crate::confine::ConfinedDir,
}

pub(in crate::runtime) fn mint(state: &VmState) -> Result<Authority> {
    let directory = state.build_out.clone()
        .ok_or_else(|| wasmtime::Error::msg("write_out: no BuildOut grant"))?;
    Ok(Authority { issuer: state.build_out_issuer.clone(), write: true, directory })
}

fn authenticate<'a>(issuer: &Arc<()>, data: &'a dyn std::any::Any) -> Result<&'a Authority> {
    let authority = data.downcast_ref::<Authority>()
        .ok_or_else(|| wasmtime::Error::msg("write_out: wrong capability kind"))?;
    if !Arc::ptr_eq(issuer, &authority.issuer) { bail!("write_out: capability belongs to another instance"); }
    if !authority.write { bail!("write_out: capability lacks write right"); }
    Ok(authority)
}

pub(in crate::runtime) fn authenticate_capture(state: &VmState, data: &dyn std::any::Any) -> Result<()> {
    authenticate(&state.build_out_issuer, data).map(|_| ())
}

pub(in crate::runtime) fn link(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "build_out_write_at", write)?;
    Ok(())
}

fn write(mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>,
    name_ptr: i32, name_len: i32, contents_ptr: i32, contents_len: i32,
) -> Result<()> {
    let token = token.ok_or_else(|| wasmtime::Error::msg("write_out: null capability"))?;
    let data = token.data(&caller)?.ok_or_else(|| wasmtime::Error::msg("write_out: missing capability data"))?;
    let directory = authenticate(&caller.data().build_out_issuer, data)?.directory.clone();
    // Authenticate first; read BOTH inputs before performing any filesystem effect.
    let memory = memory_of(&mut caller)?;
    let name = read_slice_str(memory.data(&caller), name_ptr, name_len)?;
    let contents = read_slice_str(memory.data(&caller), contents_ptr, contents_len)?;
    let parent = std::path::Path::new(&name).parent().and_then(std::path::Path::to_str)
        .ok_or_else(|| wasmtime::Error::msg(format!("write_out path `{name}` is not valid UTF-8")))?;
    confine(directory.make_dir(parent))?;
    let file = confine(directory.file(&name, false))?;
    file.write_all(contents.as_bytes()).map_err(|error| wasmtime::Error::msg(
        format!("write_out failed for `{}`: {error}", file.display_path().display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_out_authentication_rejects_kind_session_and_rights() {
        let issuer = Arc::new(());
        let directory = crate::confine::ConfinedDir::open_ambient(&std::env::temp_dir()).unwrap();
        let valid = Authority { issuer: issuer.clone(), write: true, directory };
        assert!(authenticate(&issuer, &valid).is_ok());
        assert!(authenticate(&issuer, &()).unwrap_err().to_string().contains("wrong capability kind"));
        assert!(authenticate(&Arc::new(()), &valid).unwrap_err().to_string().contains("another instance"));
        let denied = Authority { write: false, ..valid };
        assert!(authenticate(&issuer, &denied).unwrap_err().to_string().contains("write right"));
    }

    #[test]
    fn build_out_slices_are_bounded_and_strict_utf8() {
        assert_eq!(read_slice_str(b"abcd", 1, 2).unwrap(), "bc");
        for (bytes, start, length) in [(&b"abcd"[..], -1, 1), (&b"abcd"[..], 3, 2),
            (&b"\xff"[..], 0, 1), (&b"abcd"[..], 0, -1)] {
            assert!(read_slice_str(bytes, start, length).is_err());
        }
    }
}
