use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};
use std::sync::Arc;
use witchy_wir::canonical_safewir::DIR_STORAGE_IMPORT_NAMES;

#[path = "exec.rs"]
mod canonical_exec;

use super::super::{
    confine, fill_slice, memory_of, read_slice_str, read_wbytes, read_wstr, read_wstr_list,
    DirAuthority, DirBacking, ExecAuthority, FileAuthority, FileBacking, FsRights, VmState,
};

/// Register the root `Dir` grant mint.
pub(in crate::runtime) fn link_mint_dir(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "mint_dir", host_mint_dir)?;
    Ok(())
}

/// Register the Dir read family, including `dir.open` navigation to a File
/// externref (RFC-0012/RFC-0005 stage 2).
pub(in crate::runtime) fn link_dir_read(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "dir_subdir", host_dir_subdir)?;
    linker.func_wrap("witchy", "dir_only", host_dir_only)?;
    linker.func_wrap("witchy", "dir_read_len", host_dir_read_len)?;
    linker.func_wrap("witchy", "dir_read_bytes_len", host_dir_read_bytes_len)?;
    linker.func_wrap("witchy", "dir_exists", host_dir_exists)?;
    linker.func_wrap("witchy", "dir_is_dir", host_dir_is_dir)?;
    linker.func_wrap("witchy", "dir_list_size", host_dir_list_size)?;
    linker.func_wrap("witchy", "dir_open", host_dir_open)?;
    linker.func_wrap("witchy", "dir_read_file", host_dir_read_file)?;
    // (RFC-0165 slice 42) The canonical route's read family, gated by the SAME
    // `Dir[Read]` grant as its frozen siblings above: a program without the
    // right cannot even link the import.
    linker.func_wrap("witchy", "dir_read_into", host_dir_read_into)?;
    linker.func_wrap("witchy", "dir_list_into", host_dir_list_into)?;
    linker.func_wrap("witchy", "dir_exists_at", host_dir_exists_at)?;
    linker.func_wrap("witchy", "dir_is_dir_at", host_dir_is_dir_at)?;
    linker.func_wrap("witchy", "dir_subtree_at", host_dir_subtree_at)?;
    linker.func_wrap("witchy", DIR_STORAGE_IMPORT_NAMES[2], host_dir_read_bytes_at)?;
    linker.func_wrap("witchy", DIR_STORAGE_IMPORT_NAMES[3], host_dir_bytes_response_size)?;
    linker.func_wrap("witchy", DIR_STORAGE_IMPORT_NAMES[4], host_dir_bytes_response_into)?;
    Ok(())
}

/// Register the Dir write family, including `dir.create` navigation to a File
/// externref (RFC-0012/RFC-0005 stage 2).
pub(in crate::runtime) fn link_dir_write(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "dir_write", host_dir_write)?;
    linker.func_wrap("witchy", "dir_append", host_dir_append)?;
    linker.func_wrap("witchy", "dir_make_dir", host_dir_make_dir)?;
    linker.func_wrap("witchy", "dir_create", host_dir_create)?;
    linker.func_wrap("witchy", "dir_create_new", host_dir_create_new)?;
    linker.func_wrap("witchy", "dir_replace", host_dir_replace)?;
    linker.func_wrap("witchy", "dir_rename", host_dir_rename)?;
    linker.func_wrap("witchy", "dir_write_bytes", host_dir_write_bytes)?;
    linker.func_wrap("witchy", "dir_append_bytes", host_dir_append_bytes)?;
    linker.func_wrap("witchy", "dir_set_executable", host_dir_set_executable)?;
    linker.func_wrap("witchy", "dir_append_bytes_at", host_dir_append_bytes_at)?;
    linker.func_wrap("witchy", "dir_set_executable_at", host_dir_set_executable_at)?;
    linker.func_wrap("witchy", "dir_rename_at", host_dir_rename_at)?;
    // (RFC-0165 slice 42) The canonical route's write op, under the same
    // `Dir[Write]` gate as its frozen siblings.
    linker.func_wrap("witchy", "dir_write_at", host_dir_write_at)?;
    linker.func_wrap("witchy", "dir_make_dir_at", host_dir_make_dir_at)?;
    linker.func_wrap("witchy", DIR_STORAGE_IMPORT_NAMES[0], host_dir_create_new_at)?;
    linker.func_wrap("witchy", DIR_STORAGE_IMPORT_NAMES[1], host_dir_replace_at)?;
    linker.func_wrap("witchy", DIR_STORAGE_IMPORT_NAMES[5], host_dir_write_bytes_at)?;
    Ok(())
}

/// Register the File read leaf op (RFC-0012).
pub(in crate::runtime) fn link_file_read(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "file_read_len", host_file_read_len)?;
    linker.func_wrap("witchy", "file_read_into", host_file_read_into)?;
    Ok(())
}

/// Register the File write leaf op (RFC-0012).
pub(in crate::runtime) fn link_file_write(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "file_write", host_file_write)?;
    Ok(())
}

/// Register the direct `File` grant mint (RFC-0005 stage 2).
pub(in crate::runtime) fn link_mint_file(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "mint_file", host_mint_file)?;
    Ok(())
}

/// Register the Exec subprocess spawn. The executable is named through a
/// `Dir[Read]` capability, so `exec_run` reuses the shared confinement path.
pub(in crate::runtime) fn link_exec(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "mint_exec", host_mint_exec)?;
    linker.func_wrap("witchy", "exec_only", host_exec_only)?;
    linker.func_wrap("witchy", "exec_run", host_exec_run)?;
    canonical_exec::link(linker)?;
    Ok(())
}

fn exec_authority_ref(
    caller: &Caller<'_, VmState>,
    exec: Option<Rooted<ExternRef>>,
) -> Result<ExecAuthority> {
    let exec = exec.ok_or_else(|| Error::msg("Exec externref is null"))?;
    exec.data(caller)?
        .ok_or_else(|| Error::msg("Exec externref has no host data"))?
        .downcast_ref::<ExecAuthority>()
        .cloned()
        .ok_or_else(|| Error::msg("Exec externref has wrong host data"))
}

pub(in crate::runtime) fn exec_grant(state: &VmState) -> Result<ExecAuthority> {
    if !state.caps.exec {
        return Err(Error::msg("the program's `main` requires an Exec capability that was not granted"));
    }
    let allow = state.caps.exec_allow.as_ref()
        .map(|programs| programs.iter().cloned().collect());
    Ok(ExecAuthority { allow })
}

fn host_mint_exec(mut caller: Caller<'_, VmState>) -> Result<Option<Rooted<ExternRef>>> {
    let authority = exec_grant(caller.data())?;
    ExternRef::new(&mut caller, authority).map(Some)
}

fn host_exec_only(
    mut caller: Caller<'_, VmState>,
    exec: Option<Rooted<ExternRef>>,
    programs_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let current = exec_authority_ref(&caller, exec)?;
    let mem = memory_of(&mut caller)?;
    let requested: std::collections::BTreeSet<_> =
        read_wstr_list(mem.data(&caller), programs_ptr)?.into_iter().collect();
    let allow = Some(match current.allow {
        Some(current) => requested.intersection(&current).cloned().collect(),
        None => requested,
    });
    ExternRef::new(&mut caller, ExecAuthority { allow }).map(Some)
}

// --- the Dir capability family ---
//
// A guest `Dir` value is an opaque externref carrying a host-side `DirAuthority`.
// The paths never enter guest memory, and there is no guest-visible integer
// handle to forge or widen. Every operation resolves through the SAME
// handle-anchored confinement the interpreter uses, so the two backends agree
// on exactly which paths are reachable and neither reopens ambient paths.

/// Look up a root Dir grant by ordinal while minting the root `Dir` externref
/// for `main`. This ordinal never crosses into guest code.
fn dir_grant_by_ordinal(caller: &Caller<'_, VmState>, h: i32) -> Result<DirAuthority> {
    usize::try_from(h)
        .ok()
        .and_then(|i| dir_grant(caller.data(), i))
        .ok_or_else(|| Error::msg(format!("invalid Dir grant index {h}")))
}

/// The root `Dir` grant at ordinal `i`, or `None` when the launch did not carry
/// one. The frozen route reaches it through `mint_dir` and the canonical
/// launcher mints its entry parameters from it directly, so both routes hand
/// the guest the SAME authority object rather than two constructions of it.
pub(in crate::runtime) fn dir_grant(state: &VmState, i: usize) -> Option<DirAuthority> {
    state.dirs.get(i).cloned()
}

/// Recover the authority object carried by a guest `Dir` value.
pub(in crate::runtime) fn dir_authority_ref(
    caller: &Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
) -> Result<DirAuthority> {
    let d = d.ok_or_else(|| Error::msg("Dir externref is null"))?;
    d.data(caller)?
        .ok_or_else(|| Error::msg("Dir externref has no host data"))?
        .downcast_ref::<DirAuthority>()
        .cloned()
        .ok_or_else(|| Error::msg("Dir externref has wrong host data"))
}

fn dir_require_read(dir: &DirAuthority) -> Result<()> {
    if dir.rights.read {
        Ok(())
    } else {
        Err(Error::msg("this Dir capability does not grant Read"))
    }
}

fn dir_require_write(dir: &DirAuthority) -> Result<()> {
    if dir.rights.write {
        Ok(())
    } else {
        Err(Error::msg("this Dir capability does not grant Write"))
    }
}

/// Trap unless Dir `dir`'s entry policy admits touching `name` (RFC-0011).
/// `is_dir` is whether `name` denotes a directory entry (a traversal), so the policy's
/// `kind:` dimension can distinguish files from directories.
fn dir_guard(dir: &DirAuthority, name: &str, is_dir: bool) -> Result<()> {
    if witchy_caps::capabilities::dir_admits(&dir.policy, name, is_dir) {
        Ok(())
    } else {
        Err(Error::msg(format!(
            "`{name}` is not permitted by this Dir capability's entry policy"
        )))
    }
}

fn host_mint_dir(mut caller: Caller<'_, VmState>, i: i32) -> Result<Option<Rooted<ExternRef>>> {
    let dir = dir_grant_by_ordinal(&caller, i)?;
    ExternRef::new(&mut caller, dir).map(Some)
}

fn dir_child_backing(dir: &DirAuthority, name: &str) -> Result<DirBacking> {
    match &dir.backing {
        DirBacking::Fs(base) => {
            Ok(DirBacking::Fs(confine(base.open_dir(name))?))
        }
    }
}

fn dir_file_backing(dir: &DirAuthority, rel: &str, write: bool) -> Result<FileBacking> {
    match &dir.backing {
        DirBacking::Fs(base) => {
            if dir.policy.is_empty() {
                return Ok(FileBacking::Fs(confine(base.file(rel, !write))?));
            }
            match confine(base.entry_kind_exact(rel))? {
                Some(crate::confine::ConfinedEntryKind::File) => {}
                None if write => {}
                None => return Ok(FileBacking::Fs(confine(base.file(rel, true))?)),
                Some(crate::confine::ConfinedEntryKind::Directory) => {
                    return Err(Error::msg(format!("`{rel}` is a directory, not a file")));
                }
                Some(crate::confine::ConfinedEntryKind::Other) => {
                    return Err(Error::msg(format!(
                        "`{rel}` is a filesystem alias or unsupported entry"
                    )));
                }
            }
            Ok(FileBacking::Fs(confine(base.file(rel, false))?))
        }
    }
}

fn visible_entry_kind(dir: &DirAuthority, rel: &str) -> Result<Option<bool>> {
    let DirBacking::Fs(base) = &dir.backing;
    if dir.policy.is_empty() {
        if !base.exists(rel).unwrap_or(false) {
            return Ok(None);
        }
        return Ok(Some(base.is_dir(rel).unwrap_or(false)));
    }
    match confine(base.entry_kind_exact(rel))? {
        Some(crate::confine::ConfinedEntryKind::File)
            if witchy_caps::capabilities::dir_admits(&dir.policy, rel, false) => Ok(Some(false)),
        Some(crate::confine::ConfinedEntryKind::Directory)
            if witchy_caps::capabilities::dir_admits(&dir.policy, rel, true) => Ok(Some(true)),
        _ => Ok(None),
    }
}

fn read_file_backing(file: &FileBacking) -> Result<String> {
    match file {
        FileBacking::Fs(file) => {
            file.read_to_string().map_err(|e| {
                Error::msg(format!("read failed for `{}`: {e}", file.display_path().display()))
            })
        }
    }
}

fn write_file_backing(file: &FileBacking, contents: String) -> Result<()> {
    match file {
        FileBacking::Fs(file) => {
            file.write_all(contents.as_bytes()).map_err(|e| {
                Error::msg(format!("write failed for `{}`: {e}", file.display_path().display()))
            })
        }
    }
}

/// `dir_subdir(d, name) -> Dir`: attenuate to a confined subdirectory.
fn host_dir_subdir(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    name_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    // Opening a sub-directory is a directory traversal (RFC-0011 `kind`): a `files()`
    // policy forbids it, an `ext`/empty policy does not.
    dir_guard(&dir, &name, true)?;
    ExternRef::new(&mut caller, DirAuthority {
        backing: dir_child_backing(&dir, &name)?,
        policy: dir.policy,
        rights: dir.rights,
    }).map(Some)
}

/// `dir_only(d, policy) -> Dir` (RFC-0011): mint a Dir whose entry policy is the
/// current one narrowed by `policy` (refinement only shrinks).
fn host_dir_only(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    policy_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let refine = read_wstr(mem.data(&caller), policy_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    let narrowed = witchy_caps::capabilities::dir_only(&dir.policy, &refine);
    ExternRef::new(&mut caller, DirAuthority {
        backing: dir.backing,
        policy: narrowed,
        rights: dir.rights,
    }).map(Some)
}

/// Look up a direct `--file` grant by ordinal while minting the root `File`
/// externref for `main`. This ordinal never crosses into guest code.
fn file_grant_by_ordinal(caller: &Caller<'_, VmState>, f: i32) -> Result<FileAuthority> {
    caller
        .data()
        .files
        .get(f as usize)
        .cloned()
        .ok_or_else(|| Error::msg(format!("invalid File grant index {f}")))
}

/// Recover the authority object carried by a guest `File` value. A valid guest
/// value is an opaque `externref` minted by `mint_file` or `dir_open/create`; there
/// is no guest-side integer handle to forge or swap.
fn file_authority_ref(caller: &Caller<'_, VmState>, f: Option<Rooted<ExternRef>>) -> Result<FileAuthority> {
    let f = f.ok_or_else(|| Error::msg("File externref is null"))?;
    f.data(caller)?
        .ok_or_else(|| Error::msg("File externref has no host data"))?
        .downcast_ref::<FileAuthority>()
        .cloned()
        .ok_or_else(|| Error::msg("File externref has wrong host data"))
}

/// (RFC-0005 Stage 2) `mint_file(i) -> externref` — mint an unforgeable `File`
/// capability for `--file` grant index `i`, wrapping the confined file authority
/// in a wasmtime `externref` the guest cannot fabricate or corrupt (unlike the old
/// i32 handle a linear-memory overwrite could swap). The `run` wrapper calls this
/// in declaration order to fill `main`'s `File` params. The returned reference
/// roots the backing grant for as long as the guest holds it (the GC keeps host
/// data alive while a live wasm frame references the `externref`), so no separate
/// index table survives it (§8.9).
fn host_mint_file(mut caller: Caller<'_, VmState>, i: i32) -> Result<Option<Rooted<ExternRef>>> {
    let file = file_grant_by_ordinal(&caller, i)?;
    ExternRef::new(&mut caller, file).map(Some)
}

/// `dir_open(d, rel) -> File` (RFC-0012/RFC-0005 Stage 2): open an existing file
/// confined to Dir externref `d`, minting an unforgeable `File` externref.
/// `dir_create` is the write counterpart (the target need not exist yet).
fn host_dir_open(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    rel_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    dir_guard(&dir, &rel, false)?;
    ExternRef::new(&mut caller, FileAuthority {
        backing: dir_file_backing(&dir, &rel, false)?,
        rights: FsRights::new(true, false),
    }).map(Some)
}

/// `dir_read_file(d, path, path_len) -> File[Read]`: open an existing regular
/// file under a readable Dir using the canonical raw UTF-8 slice ABI.
fn host_dir_read_file(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    path_ptr: i32,
    path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let path = read_unsigned_slice_str(mem.data(&caller), path_ptr, path_len, "File path")?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    dir_guard(&dir, &path, false)?;
    ExternRef::new(&mut caller, FileAuthority {
        backing: dir_file_backing(&dir, &path, false)?,
        rights: FsRights::new(true, false),
    }).map(Some)
}

/// `dir_create(h, rel) -> File`: like `dir_open` but for a not-yet-existing target
/// (confined via its parent, like `dir_write`).
fn host_dir_create(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    rel_ptr: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    dir_guard(&dir, &rel, false)?;
    ExternRef::new(&mut caller, FileAuthority {
        backing: dir_file_backing(&dir, &rel, true)?,
        rights: FsRights::new(false, true),
    }).map(Some)
}

/// `file_read_len(f) -> byte length`: read the confined File externref NOW, stage its
/// bytes, and report the length; the guest allocates and calls `fill_pending`.
/// Mirrors `dir_read_len`, but a `File` is a leaf so there is no path argument.
fn host_file_read_len(mut caller: Caller<'_, VmState>, f: Option<Rooted<ExternRef>>) -> Result<i32> {
    let file = file_authority_ref(&caller, f)?;
    if !file.rights.read {
        return Err(Error::msg("this File capability does not grant Read"));
    }
    let contents = read_file_backing(&file.backing)?;
    let len = contents.len() as i32;
    caller.data_mut().pending = Some(contents.into_bytes());
    Ok(len)
}

/// `file_read_into(f, out, out_capacity) -> byte length`: validate the complete
/// output range, read the current confined file as strict UTF-8, and write only
/// when the supplied capacity fits the complete value. No pending host state or
/// cursor is involved, and the returned i32 carries the unsigned wasm32 length.
fn host_file_read_into(
    mut caller: Caller<'_, VmState>,
    f: Option<Rooted<ExternRef>>,
    out_ptr: i32,
    out_capacity: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let (out_start, out_capacity) = checked_unsigned_guest_range(
        mem.data(&caller).len(),
        out_ptr,
        out_capacity,
        "File output",
    )?;
    let file = file_authority_ref(&caller, f)?;
    if !file.rights.read {
        return Err(Error::msg("this File capability does not grant Read"));
    }
    let contents = read_file_backing(&file.backing)?;
    let required = u32::try_from(contents.len())
        .map_err(|_| Error::msg("File contents exceed the wasm32 size limit"))?;
    if usize::try_from(required).expect("u32 fits usize on supported targets") <= out_capacity {
        let bytes = contents.as_bytes();
        mem.data_mut(&mut caller)[out_start..out_start + bytes.len()].copy_from_slice(bytes);
    }
    Ok(required as i32)
}

fn read_unsigned_slice_str(data: &[u8], ptr: i32, len: i32, what: &str) -> Result<String> {
    let (start, len) = checked_unsigned_guest_range(data.len(), ptr, len, what)?;
    std::str::from_utf8(&data[start..start + len])
        .map(str::to_owned)
        .map_err(|error| Error::msg(format!("{what} is not valid UTF-8: {error}")))
}

fn checked_unsigned_guest_range(
    data_len: usize,
    ptr: i32,
    len: i32,
    what: &str,
) -> Result<(usize, usize)> {
    let start = ptr as u32 as usize;
    let len = len as u32 as usize;
    let end = start
        .checked_add(len)
        .ok_or_else(|| Error::msg(format!("{what} range overflows wasm32 address space")))?;
    if end > data_len {
        return Err(Error::msg(format!(
            "{what} range [{start}, {end}) exceeds guest memory ({data_len} bytes)"
        )));
    }
    Ok((start, len))
}

/// `file_write(f, contents)`: write a confined File externref
/// (RFC-0012/RFC-0005 Stage 2).
fn host_file_write(
    mut caller: Caller<'_, VmState>,
    f: Option<Rooted<ExternRef>>,
    contents_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let contents = read_wstr(mem.data(&caller), contents_ptr)?;
    let file = file_authority_ref(&caller, f)?;
    if !file.rights.write {
        return Err(Error::msg("this File capability does not grant Write"));
    }
    write_file_backing(&file.backing, contents)
}

/// `dir_read_len(h, rel) -> byte length`: read the confined file NOW, stage its
/// bytes, and report the length; the guest allocates and calls `fill_pending`.
/// A failed read traps — the interpreter errors on it too.
fn host_dir_read_len(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    rel_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    dir_guard(&dir, &rel, false)?;
    let file = dir_file_backing(&dir, &rel, false)?;
    let contents = read_file_backing(&file)?;
    let len = contents.len() as i32;
    caller.data_mut().pending = Some(contents.into_bytes());
    Ok(len)
}

// --- (RFC-0165 slice 42) the canonical Dir family ---
//
// Same authority, same confinement, same entry policy as the frozen ops above —
// only the buffer protocol differs. A path arrives as a raw checked `(ptr, len)`
// slice rather than a length-prefixed object, and a read writes a BOUNDED prefix
// into the guest's own buffer instead of staging an unbounded payload in host
// state for a second call to place. Nothing here touches `pending`, so no host
// state survives a canonical call.

/// `dir_read_into(dir, path, path_len, out, out_cap) -> Size`: read the confined
/// file NOW under `dir_read_len`'s exact rules (a missing or denied file, or one
/// whose bytes are not UTF-8, traps), write `min(length, out_cap)` bytes at
/// `out`, and return the file's FULL byte length. A guest whose buffer was short
/// sees that in the answer and calls again with a bigger one; it must never
/// index by the first answer, because the file may have changed in between.
fn host_dir_read_into(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    path_ptr: i32,
    path_len: i32,
    out_ptr: i32,
    out_cap: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_slice_str(mem.data(&caller), path_ptr, path_len)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    dir_guard(&dir, &rel, false)?;
    let contents = read_file_backing(&dir_file_backing(&dir, &rel, false)?)?;
    fill_slice(&mut caller, out_ptr, out_cap, contents.as_bytes(), "file contents")
}

/// `dir_exists_at(dir, path, path_len) -> Bool`: `dir_exists`'s semantics over a
/// checked slice — total, so an escaping or denied path is `false`, not a trap.
fn host_dir_exists_at(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    path_ptr: i32,
    path_len: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_slice_str(mem.data(&caller), path_ptr, path_len)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    let ok = visible_entry_kind(&dir, &rel).ok().flatten().is_some();
    Ok(ok as i32)
}

/// The legacy total directory predicate over one bounded UTF-8 slice. Invalid
/// authority or transport traps; denied, missing, and inaccessible entries are
/// false. Authenticate before touching memory, and preserve the exact existing
/// handle-backed entry-policy semantics rather than interpreting paths here.
fn host_dir_is_dir_at(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    path_ptr: i32,
    path_len: i32,
) -> Result<i32> {
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    let mem = memory_of(&mut caller)?;
    let rel = read_slice_str(mem.data(&caller), path_ptr, path_len)?;
    Ok((visible_entry_kind(&dir, &rel).ok().flatten() == Some(true)) as i32)
}

#[cfg(test)]
#[path = "filesystem_metadata_tests.rs"]
mod metadata_tests;

/// `dir_write_at(dir, path, path_len, data, data_len)`: `dir_write`'s semantics
/// over checked slices — the data is UTF-8 text, and the write needs `Dir[Write]`.
fn host_dir_write_at(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    path_ptr: i32,
    path_len: i32,
    data_ptr: i32,
    data_len: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let rel = read_slice_str(data, path_ptr, path_len)?;
    let contents = read_slice_str(data, data_ptr, data_len)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    dir_guard(&dir, &rel, false)?;
    let file = dir_file_backing(&dir, &rel, true)?;
    write_file_backing(&file, contents)
}

/// `dir_subtree_at(dir, path, path_len) -> Dir`: `dir_subdir`'s semantics over a
/// checked slice. The result is a fresh host-minted token carrying the narrowed
/// authority, so a narrowed `Dir` only ever comes from the host.
fn host_dir_subtree_at(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    path_ptr: i32,
    path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let name = read_slice_str(mem.data(&caller), path_ptr, path_len)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    dir_guard(&dir, &name, true)?;
    ExternRef::new(&mut caller, DirAuthority {
        backing: dir_child_backing(&dir, &name)?,
        policy: dir.policy,
        rights: dir.rights,
    }).map(Some)
}

/// (RFC-0095) `dir_read_bytes_len(h, rel) -> byte length`: like `dir_read_len` but
/// reads the confined file's RAW bytes (no UTF-8 validation) — for a binary
/// artifact. Stages the bytes; the guest allocates a `Bytes` and calls `fill_pending`.
fn host_dir_read_bytes_len(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    rel_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    dir_guard(&dir, &rel, false)?;
    let bytes = read_bytes_backing(&dir_file_backing(&dir, &rel, false)?)?;
    let len = bytes.len() as i32;
    caller.data_mut().pending = Some(bytes);
    Ok(len)
}

/// (RFC-0095) `dir_write_bytes(h, rel, data)`: write a confined file from RAW bytes
/// (read via `read_wbytes`, no UTF-8 requirement) — for a binary artifact. Mirrors
/// `dir_write`'s confinement + write authority.
fn host_dir_write_bytes(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    rel_ptr: i32,
    data_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let rel = read_wstr(data, rel_ptr)?;
    let bytes = read_wbytes(data, data_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    dir_guard(&dir, &rel, false)?;
    match dir_file_backing(&dir, &rel, true)? {
        FileBacking::Fs(file) => file.write_all(&bytes).map_err(|e| {
            Error::msg(format!("write failed for `{}`: {e}", file.display_path().display()))
        }),
    }
}

/// Read a confined file's raw bytes (RFC-0095), no UTF-8 validation.
fn read_bytes_backing(file: &FileBacking) -> Result<Vec<u8>> {
    match file {
        FileBacking::Fs(file) => file.read_bytes().map_err(|e| {
            Error::msg(format!("read failed for `{}`: {e}", file.display_path().display()))
        }),
    }
}

/// Private immutable read result, not a Dir or an entry grant. Neither an
/// integer kind nor bytes supplied by the guest can construct this authority.
#[derive(Clone)]
struct DirBytesResponse {
    issuer: Arc<()>,
    bytes: Arc<Vec<u8>>,
}

fn dir_bytes_length(length: usize) -> Result<i32> {
    i32::try_from(length).map_err(|_| Error::msg("Dir byte response exceeds the guest ABI size limit"))
}

fn dir_storage_range(ptr: i32, length: i32, limit: usize) -> Result<std::ops::Range<usize>> {
    let start = usize::try_from(ptr).map_err(|_| Error::msg("negative Dir storage slice pointer"))?;
    let length = usize::try_from(length).map_err(|_| Error::msg("negative Dir storage slice length"))?;
    let end = start.checked_add(length).filter(|end| *end <= limit)
        .ok_or_else(|| Error::msg("Dir storage slice lies outside guest memory"))?;
    Ok(start..end)
}

fn dir_bytes_response(caller: &Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<Arc<Vec<u8>>> {
    let token = token.ok_or_else(|| Error::msg("Dir byte response is null"))?;
    let data = token.data(caller)?.ok_or_else(|| Error::msg("Dir byte response has no host data"))?;
    let response = data.downcast_ref::<DirBytesResponse>()
        .ok_or_else(|| Error::msg("Dir byte response has wrong host kind"))?;
    if !Arc::ptr_eq(&caller.data().registry_issuer, &response.issuer) {
        return Err(Error::msg("Dir byte response belongs to another instance"));
    }
    Ok(response.bytes.clone())
}

fn host_dir_read_bytes_at(
    mut caller: Caller<'_, VmState>, dir: Option<Rooted<ExternRef>>, path_ptr: i32, path_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    let dir = dir_authority_ref(&caller, dir)?;
    dir_require_read(&dir)?;
    let memory = memory_of(&mut caller)?;
    let path = read_slice_str(memory.data(&caller), path_ptr, path_len)?;
    dir_guard(&dir, &path, false)?;
    // Exactly one filesystem read. Later getters only borrow this snapshot.
    let bytes = read_bytes_backing(&dir_file_backing(&dir, &path, false)?)?;
    dir_bytes_length(bytes.len())?;
    let response = DirBytesResponse { issuer: caller.data().registry_issuer.clone(), bytes: Arc::new(bytes) };
    ExternRef::new(&mut caller, response).map(Some)
}

fn host_dir_bytes_response_size(caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>) -> Result<i32> {
    dir_bytes_length(dir_bytes_response(&caller, token)?.len())
}

fn host_dir_bytes_response_into(
    mut caller: Caller<'_, VmState>, token: Option<Rooted<ExternRef>>, out_ptr: i32, capacity: i32,
) -> Result<i32> {
    let bytes = dir_bytes_response(&caller, token)?;
    let required = dir_bytes_length(bytes.len())?;
    let memory = memory_of(&mut caller)?;
    let output = dir_storage_range(out_ptr, capacity, memory.data_size(&caller))?;
    // Preflight even an empty or short buffer in full. No partial copy, state
    // consumption, or guest-visible pointer into another instance's memory.
    if output.len() >= bytes.len() {
        memory.write(&mut caller, output.start, bytes.as_slice())?;
    }
    Ok(required)
}

fn dir_storage_text_inputs(
    caller: &mut Caller<'_, VmState>, dir: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32,
) -> Result<(DirAuthority, String, String)> {
    let dir = dir_authority_ref(caller, dir)?;
    dir_require_write(&dir)?;
    let memory = memory_of(caller)?;
    let path = read_slice_str(memory.data(&*caller), path_ptr, path_len)?;
    let contents = read_slice_str(memory.data(&*caller), data_ptr, data_len)?;
    Ok((dir, path, contents))
}

fn host_dir_create_new_at(
    mut caller: Caller<'_, VmState>, dir: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32,
) -> Result<i32> {
    let (dir, path, contents) = dir_storage_text_inputs(&mut caller, dir, path_ptr, path_len, data_ptr, data_len)?;
    dir_guard(&dir, &path, false)?;
    let _ = dir_file_backing(&dir, &path, true)?;
    let created = match &dir.backing {
        DirBacking::Fs(base) => base.create_new(&path, contents.as_bytes())
            .map_err(|e| Error::msg(format!("create_new failed for `{path}`: {}", e.0)))?,
    };
    Ok(i32::from(created))
}

fn host_dir_replace_at(
    mut caller: Caller<'_, VmState>, dir: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32,
) -> Result<()> {
    let (dir, path, contents) = dir_storage_text_inputs(&mut caller, dir, path_ptr, path_len, data_ptr, data_len)?;
    dir_guard(&dir, &path, false)?;
    let _ = dir_file_backing(&dir, &path, true)?;
    match &dir.backing {
        DirBacking::Fs(base) => base.replace(&path, contents.as_bytes())
            .map_err(|e| Error::msg(format!("replace failed for `{path}`: {}", e.0))),
    }
}

fn host_dir_write_bytes_at(
    mut caller: Caller<'_, VmState>, dir: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32,
) -> Result<()> {
    let dir = dir_authority_ref(&caller, dir)?;
    dir_require_write(&dir)?;
    let memory = memory_of(&mut caller)?;
    let path = read_slice_str(memory.data(&caller), path_ptr, path_len)?;
    let input = dir_storage_range(data_ptr, data_len, memory.data_size(&caller))?;
    let bytes = memory.data(&caller)[input].to_vec();
    dir_guard(&dir, &path, false)?;
    match dir_file_backing(&dir, &path, true)? {
        FileBacking::Fs(file) => file.write_all(&bytes).map_err(|e| {
            Error::msg(format!("write failed for `{}`: {e}", file.display_path().display()))
        }),
    }
}

#[cfg(test)]
#[path = "dir_storage_tests.rs"]
mod dir_storage_tests;

/// `exec_run(d, path, args, stdin) -> byte length`: spawn the executable named by
/// `path` within Dir externref `d` (confined like `dir_read`), passing `args` (a
/// single `\0`-joined argv string) and `stdin`. Captures the child's stdout+stderr
/// and stages a payload `"<exit_code>\n<stdout><stderr>"`, reporting its byte
/// length; the guest allocates and calls `fill_pending`. Mirrors `dir_read_len`'s
/// two-phase protocol so both backends produce identical results. Needs `Exec`.
fn host_exec_run(
    mut caller: Caller<'_, VmState>,
    exec: Option<Rooted<ExternRef>>,
    d: Option<Rooted<ExternRef>>,
    path_ptr: i32,
    args_ptr: i32,
    stdin_ptr: i32,
) -> Result<i32> {
    let exec = exec_authority_ref(&caller, exec)?;
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let path = read_wstr(data, path_ptr)?;
    let joined = read_wstr(data, args_ptr)?;
    let stdin = read_wstr(data, stdin_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    let payload = run_exec(&exec, &dir, &path, &joined, &stdin)?;
    let len = payload.len() as i32;
    caller.data_mut().pending = Some(payload);
    Ok(len)
}

/// Shared policy and handle-backed execution for both guest ABI adapters.
/// `ConfinedFile::run` remains the sole process launcher.
fn run_exec(exec: &ExecAuthority, dir: &DirAuthority, path: &str, joined: &str, stdin: &str) -> Result<Vec<u8>> {
    if let Some(allow) = &exec.allow {
        if !allow.contains(path) {
            return Err(Error::msg(format!("exec: `{path}` is not in this Exec grant's allow-list")));
        }
    }
    dir_require_read(dir)?;
    // (RFC-0011) exec is the sharpest right, so it takes the SAME entry-policy gate as
    // every other Dir op: a `Dir[...].only(...)` may only run a file it admits — "you
    // can only run a file you can read" was false while this was skipped.
    dir_guard(dir, path, false)?;
    let DirBacking::Fs(base) = &dir.backing;
    let prog = confine(base.file(path, true))?;
    let argv: Vec<&str> = if joined.is_empty() { Vec::new() } else { joined.split('\0').collect() };
    let output = prog.run(&argv, stdin).map_err(|e| {
        Error::msg(format!("exec failed running `{}`: {e}", prog.display_path().display()))
    })?;
    let code = output.status.code().unwrap_or(-1);
    let out = String::from_utf8_lossy(&output.stdout);
    let serr = String::from_utf8_lossy(&output.stderr);
    Ok(format!("{code}\n{out}{serr}").into_bytes())
}

/// `dir_exists(h, rel) -> bool`: total — an escaping or missing path is `false`.
fn host_dir_exists(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    rel_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    let ok = visible_entry_kind(&dir, &rel).ok().flatten().is_some();
    Ok(ok as i32)
}

/// `dir_is_dir(h, rel) -> bool`: total, like `dir_exists`.
fn host_dir_is_dir(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    rel_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    let ok = visible_entry_kind(&dir, &rel).ok().flatten() == Some(true);
    Ok(ok as i32)
}

/// `dir_list_into(dir, out, out_cap) -> byte length`: the admitted entry names
/// (sorted, the same policy filter as `dir_list_size`) joined by NUL, written
/// through the bounded slice protocol of `dir_read_into`; the guest splits
/// them. A name cannot contain NUL, so the separator is unambiguous.
fn host_dir_list_into(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    out_ptr: i32,
    out_cap: i32,
) -> Result<i32> {
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    let names = admitted_entry_names(&dir)?;
    let payload = names.join("\0");
    fill_slice(&mut caller, out_ptr, out_cap, payload.as_bytes(), "directory listing")
}

/// The directory's entry names the grant's policy admits, sorted.
fn admitted_entry_names(dir: &DirAuthority) -> Result<Vec<String>> {
    Ok(match &dir.backing {
        DirBacking::Fs(base) => base.entries().map_err(|e| {
            Error::msg(format!("list failed for `{}`: {e}", base.display_path().display()))
        })?.into_iter().filter(|name| {
            let is_dir = base.is_dir(name).unwrap_or(false);
            witchy_caps::capabilities::dir_admits(&dir.policy, name, is_dir)
        }).collect(),
    })
}

/// `dir_list_size(h) -> bytes`: read the directory NOW (sorted names, matching
/// the interpreter), stage the listing, and report the total byte size of the
/// witchy `List(String)` structure the guest must reserve.
fn host_dir_list_size(mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>) -> Result<i32> {
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_read(&dir)?;
    let names = admitted_entry_names(&dir)?;
    let size = 4 + 8 * names.len() + names.iter().map(|n| 4 + n.len()).sum::<usize>();
    caller.data_mut().pending_list = Some(names);
    Ok(size as i32)
}

/// `dir_write(h, rel, contents)`: write a confined file (trap on failure or
/// escape — the interpreter errors on both).
fn host_dir_write(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    rel_ptr: i32,
    contents_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let rel = read_wstr(data, rel_ptr)?;
    let contents = read_wstr(data, contents_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    dir_guard(&dir, &rel, false)?;
    let file = dir_file_backing(&dir, &rel, true)?;
    write_file_backing(&file, contents)
}

/// `dir_append(h, rel, contents)`: append to a confined file, creating it if
/// absent — `dir_write`'s confinement without clobbering existing contents.
fn host_dir_append(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    rel_ptr: i32,
    contents_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let rel = read_wstr(data, rel_ptr)?;
    let contents = read_wstr(data, contents_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    dir_guard(&dir, &rel, false)?;
    match dir_file_backing(&dir, &rel, true)? {
        FileBacking::Fs(file) => {
            file.append_all(contents.as_bytes()).map_err(|e| {
                Error::msg(format!("append failed for `{}`: {e}", file.display_path().display()))
            })
        }
    }
}

/// Bounded installer writes authenticate the existing Dir Write authority before
/// memory access, and snapshot all arguments before any filesystem operation.
fn host_dir_append_bytes_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32,
) -> Result<()> {
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    let memory = memory_of(&mut caller)?;
    let data = memory.data(&caller);
    dir_storage_range(path_ptr, path_len, data.len())?;
    let payload = dir_storage_range(data_ptr, data_len, data.len())?;
    let path = read_slice_str(data, path_ptr, path_len)?;
    let bytes = data[payload].to_vec();
    dir_guard(&dir, &path, false)?;
    match dir_file_backing(&dir, &path, true)? {
        FileBacking::Fs(file) => file.append_all(&bytes).map_err(|e| {
            Error::msg(format!("append failed for `{}`: {e}", file.display_path().display()))
        }),
    }
}

fn host_dir_set_executable_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32,
) -> Result<()> {
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    let memory = memory_of(&mut caller)?;
    dir_storage_range(path_ptr, path_len, memory.data_size(&caller))?;
    let path = read_slice_str(memory.data(&caller), path_ptr, path_len)?;
    dir_guard(&dir, &path, false)?;
    match dir_file_backing(&dir, &path, false)? {
        FileBacking::Fs(file) => file.set_executable().map_err(|e| {
            Error::msg(format!("set_executable failed for `{}`: {e}", file.display_path().display()))
        }),
    }
}

fn host_dir_rename_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>,
    from_ptr: i32, from_len: i32, to_ptr: i32, to_len: i32,
) -> Result<()> {
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    let memory = memory_of(&mut caller)?;
    let data = memory.data(&caller);
    dir_storage_range(from_ptr, from_len, data.len())?;
    dir_storage_range(to_ptr, to_len, data.len())?;
    let from = read_slice_str(data, from_ptr, from_len)?;
    let to = read_slice_str(data, to_ptr, to_len)?;
    rename_dir_entries(&dir, &from, &to)
}

#[cfg(test)]
#[path = "dir_installer_tests.rs"]
mod dir_installer_tests;

/// (RFC-0095) `dir_append_bytes(h, rel, data)`: append RAW bytes to a confined file,
/// creating it if absent — `dir_append`'s confinement/rights with `dir_write_bytes`'
/// binary marshalling. Streaming a large artifact chunk by chunk this way keeps the
/// guest's working set flat (one chunk), where accumulating the whole in memory would
/// exhaust it.
fn host_dir_append_bytes(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    rel_ptr: i32,
    data_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let rel = read_wstr(data, rel_ptr)?;
    let bytes = read_wbytes(data, data_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    dir_guard(&dir, &rel, false)?;
    match dir_file_backing(&dir, &rel, true)? {
        FileBacking::Fs(file) => file.append_all(&bytes).map_err(|e| {
            Error::msg(format!("append failed for `{}`: {e}", file.display_path().display()))
        }),
    }
}

/// (RFC-0095) `dir_set_executable(h, rel)`: add the execute bits to a confined file
/// (`chmod +x`), leaving its contents untouched — a Write op (it mutates file
/// metadata). The installer calls it after streaming a trusted-publisher so the written
/// file is runnable.
fn host_dir_set_executable(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    rel_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    dir_guard(&dir, &rel, false)?;
    match dir_file_backing(&dir, &rel, false)? {
        FileBacking::Fs(file) => file.set_executable().map_err(|e| {
            Error::msg(format!("set_executable failed for `{}`: {e}", file.display_path().display()))
        }),
    }
}

/// `dir_make_dir(h, name)`: create a confined subdirectory (idempotent). Creating a
/// directory is a directory op (RFC-0011 `kind`): a `files()` policy forbids it.
fn host_dir_make_dir_at(
    mut caller: Caller<'_, VmState>, d: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32,
) -> Result<()> {
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    let memory = memory_of(&mut caller)?;
    let name = read_slice_str(memory.data(&caller), path_ptr, path_len)?;
    dir_guard(&dir, &name, true)?;
    match &dir.backing {
        DirBacking::Fs(base) => base.make_dir(&name).map_err(|e| Error::msg(format!(
            "make_dir failed for `{}`: {}", base.display_path().join(&name).display(), e.0
        ))),
    }
}

fn host_dir_make_dir(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    name_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    dir_guard(&dir, &name, true)?;
    match &dir.backing {
        DirBacking::Fs(base) => {
            base.make_dir(&name).map_err(|e| Error::msg(format!(
                "make_dir failed for `{}`: {}",
                base.display_path().join(&name).display(),
                e.0
            )))
        }
    }
}

/// `dir_create_new(h, rel, contents) -> i32` (RFC-0118): atomically create the
/// confined file if and only if it does not already exist (`O_CREAT|O_EXCL`).
/// Returns 1 when this call created it, 0 when it was already present (the
/// race-loser signal); traps on escape or a real IO fault.
fn host_dir_create_new(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    rel_ptr: i32,
    contents_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let rel = read_wstr(data, rel_ptr)?;
    let contents = read_wstr(data, contents_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    dir_guard(&dir, &rel, false)?;
    let _ = dir_file_backing(&dir, &rel, true)?;
    let created = match &dir.backing {
        DirBacking::Fs(base) => base.create_new(&rel, contents.as_bytes()).map_err(|e| {
            Error::msg(format!("create_new failed for `{rel}`: {}", e.0))
        })?,
    };
    Ok(created as i32)
}

/// `dir_replace(h, rel, contents)` (RFC-0118): atomically replace a confined
/// file's contents (creating it if absent) via a temp sibling + rename, so a
/// concurrent reader never observes a torn write. Traps on escape or IO fault.
fn host_dir_replace(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    rel_ptr: i32,
    contents_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let rel = read_wstr(data, rel_ptr)?;
    let contents = read_wstr(data, contents_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    dir_guard(&dir, &rel, false)?;
    let _ = dir_file_backing(&dir, &rel, true)?;
    match &dir.backing {
        DirBacking::Fs(base) => base.replace(&rel, contents.as_bytes()).map_err(|e| {
            Error::msg(format!("replace failed for `{rel}`: {}", e.0))
        }),
    }
}

/// `dir_rename(h, from, to)` (RFC-0118): atomically move `from` to `to` within
/// the Dir authority, replacing `to` if present. Both paths stay confined.
fn host_dir_rename(
    mut caller: Caller<'_, VmState>,
    d: Option<Rooted<ExternRef>>,
    from_ptr: i32,
    to_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let from = read_wstr(data, from_ptr)?;
    let to = read_wstr(data, to_ptr)?;
    let dir = dir_authority_ref(&caller, d)?;
    dir_require_write(&dir)?;
    rename_dir_entries(&dir, &from, &to)
}

// Shared only after authentication and input marshalling. Preserve the legacy
// source/destination entry-kind policy and descriptor-anchored atomic rename.
fn rename_dir_entries(dir: &DirAuthority, from: &str, to: &str) -> Result<()> {
    let source_is_dir = if dir.policy.is_empty() {
        false
    } else {
        let DirBacking::Fs(base) = &dir.backing;
        match confine(base.entry_kind_exact(from))? {
            Some(crate::confine::ConfinedEntryKind::File) => false,
            Some(crate::confine::ConfinedEntryKind::Directory) => true,
            Some(crate::confine::ConfinedEntryKind::Other) => {
                return Err(Error::msg(format!(
                    "`{from}` is a filesystem alias or unsupported entry"
                )));
            }
            None => return Err(Error::msg(format!("rename source `{from}` does not exist"))),
        }
    };
    dir_guard(dir, from, source_is_dir)?;
    dir_guard(dir, to, source_is_dir)?;
    if !dir.policy.is_empty() {
        let DirBacking::Fs(base) = &dir.backing;
        if let Some(kind) = confine(base.entry_kind_exact(to))? {
            let destination_is_dir = match kind {
                crate::confine::ConfinedEntryKind::File => false,
                crate::confine::ConfinedEntryKind::Directory => true,
                crate::confine::ConfinedEntryKind::Other => {
                    return Err(Error::msg(format!(
                        "`{to}` is a filesystem alias or unsupported entry"
                    )));
                }
            };
            dir_guard(dir, to, destination_is_dir)?;
        }
    }
    match &dir.backing {
        DirBacking::Fs(base) => base.rename(from, to).map_err(|e| {
            Error::msg(format!("rename failed (`{from}` -> `{to}`): {}", e.0))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{Capabilities, FsRights, Runtime};

    /// The same read, aimed outside the granted root.
    const READ_ESCAPING: &str = r#"
        (module
          (import "witchy" "mint_dir" (func $mint_dir (param i32) (result externref)))
          (import "witchy" "dir_read_into"
            (func $dir_read_into (param externref i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "../escape.txt")
          (func (export "run")
            (drop (call $dir_read_into (call $mint_dir (i32.const 0))
              (i32.const 0) (i32.const 13) (i32.const 64) (i32.const 64)))))
    "#;

    /// Write through the FIRST Dir grant, which the tests below grant read-only.
    const WRITE_AT: &str = r#"
        (module
          (import "witchy" "mint_dir" (func $mint_dir (param i32) (result externref)))
          (import "witchy" "dir_write_at"
            (func $dir_write_at (param externref i32 i32 i32 i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "w.txt")
          (data (i32.const 16) "data")
          (func (export "run")
            (call $dir_write_at (call $mint_dir (i32.const 0))
              (i32.const 0) (i32.const 5) (i32.const 16) (i32.const 4))))
    "#;

    fn seeded_root(name: &str, contents: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("granted root");
        std::fs::write(root.path().join(name), contents).expect("seed the granted root");
        root
    }

    /// Confinement is the frozen route's, unchanged: a path that leaves the
    /// granted root traps rather than reading.
    #[test]
    fn dir_read_into_traps_on_a_path_escaping_the_root() {
        let root = seeded_root("a.txt", "hello");
        std::fs::write(root.path().parent().expect("temp parent").join("escape.txt"), "secret")
            .expect("seed a file outside the grant");
        let mut runtime = Runtime::new().expect("runtime");
        let mut vm = runtime
            .spawn(
                READ_ESCAPING,
                Capabilities {
                    dir_root: Some(root.path().to_path_buf()),
                    dir_read: true,
                    ..Capabilities::default()
                },
                4,
            )
            .expect("instantiate the escaping-read probe");
        let error = vm.run().expect_err("an escaping path must trap");
        let detail = format!("{error:?}");
        assert!(
            !detail.contains("secret"),
            "the trap must not leak the escaped file, got: {detail}"
        );
    }

    /// Rights live in the authority object, so a read-only token refuses the
    /// write even when a sibling grant made the import linkable.
    #[test]
    fn dir_write_at_refuses_a_read_only_dir() {
        let read_only = seeded_root("a.txt", "hello");
        let writable = tempfile::tempdir().expect("writable root");
        let mut runtime = Runtime::new().expect("runtime");
        let mut vm = runtime
            .spawn(
                WRITE_AT,
                Capabilities {
                    dir_root: Some(read_only.path().to_path_buf()),
                    dir_roots: vec![writable.path().to_path_buf()],
                    dir_rights: vec![FsRights::new(true, false), FsRights::new(true, true)],
                    ..Capabilities::default()
                },
                4,
            )
            .expect("a writable sibling grant links the write family");
        let error = vm.run().expect_err("a read-only Dir must refuse a write");
        assert!(
            format!("{error:?}").contains("does not grant Write"),
            "expected a rights refusal, got: {error:?}"
        );
        assert!(
            !read_only.path().join("w.txt").exists(),
            "the refused write must not have touched the read-only root"
        );
    }

    /// The gate before the gate: with no Write right anywhere, the canonical
    /// write import does not exist for the VM at all.
    #[test]
    fn dir_write_at_is_not_linked_without_a_write_grant() {
        let root = seeded_root("a.txt", "hello");
        let mut runtime = Runtime::new().expect("runtime");
        let error = runtime
            .spawn(
                WRITE_AT,
                Capabilities {
                    dir_root: Some(root.path().to_path_buf()),
                    dir_read: true,
                    ..Capabilities::default()
                },
                4,
            )
            .map(|_| ())
            .expect_err("an ungranted write import must not link");
        assert!(
            error.to_string().contains("unknown import"),
            "expected an unknown-import error, got: {error}"
        );
    }
}
